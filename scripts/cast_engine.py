#!/usr/bin/env python3
"""omascreencast cast engine.

Discovery, capture -> encode -> serve, and Chromecast playback. Emits one JSON
object per line (JSONL) so the QML side can stream structured phase updates.

M3 pipeline (this file):

    wf-recorder -o HEADLESS-N  ->  ffmpeg (fragmented MP4 to stdout)
      -> in-memory LL-HLS packager (bounded ring buffer, no disk)
      -> local HTTP server (CORS)  ->  pychromecast play_media

The HLS segments are held **in RAM** in a bounded ring buffer and served
directly from memory; nothing is written to disk for buffering. The only
filesystem objects are a short-lived FIFO (a pipe, not a buffer) between the
capture tool and ffmpeg, and the logs/debug dumps under
``~/.local/state/omascreencast`` (XDG state).

The receiver only accepts HLS (raw MPEG-TS and DASH are rejected) and requires
CORS headers; see ``plans/m0-spikes.md`` (Spikes H/J) and ``plans/milestones.md``
(M3, R2a-R2f).

Planned phases: discovering -> starting -> casting -> idle/error.
"""

from __future__ import annotations

import argparse
import http.server
import ipaddress
import json
import logging
import math
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from contextlib import contextmanager
from dataclasses import dataclass, field
from datetime import datetime, timezone
from logging.handlers import RotatingFileHandler
from pathlib import Path

_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

# The session-state module is the single source of truth shared with
# `session.py` and the QML service (M2 split-brain fix). It never imports this
# module, so there is no cycle.
import session as session_state  # noqa: E402

# ---------------------------------------------------------------------------
# Paths. Logs and opt-in debug dumps live in the XDG state directory (M3
# [ux]); runtime session state is the single `session.json` owned by
# `session.py` in the same directory.
# ---------------------------------------------------------------------------
STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "omascreencast"
LOG_DIR = STATE_DIR
LOG_FILE = LOG_DIR / "cast_engine.log"
DEBUG_DIR = LOG_DIR / "debug"

DEFAULT_PORT = 7236
# `auto` enumerates /dev/dri/renderD* at runtime (Spike L) instead of assuming
# the first GPU is renderD128.
DEFAULT_RENDER_NODE = "auto"

# LL-HLS tuning (mirrors the validated Spike J parameters).
PART_DUR = 0.5          # seconds per partial segment
SEG_PARTS = 4           # parts per full segment (2 s at 0.5 s parts)
LIST_PARTS = 6          # parts kept in the ring buffer
KEEP_SEGMENTS = 4       # full segments kept in the ring buffer
READY_PARTS = 2         # init + 2 parts (~1 s) is enough to start casting
BLOCKING_RELOAD_TIMEOUT = 2.0

# wf-recorder codec names (ffmpeg encoders). `auto` resolves at runtime.
CODEC_MAP = {
    "hevc": "hevc_vaapi",
    "h264": "h264_vaapi",
    "vp9": "libvpx-vp9",
    "av1": "av1_vaapi",
}

HLS_MIME = "application/vnd.apple.mpegurl"
MP4_MIME = "video/mp4"


# ---------------------------------------------------------------------------
# Output + logging
# ---------------------------------------------------------------------------
def emit(payload: dict) -> None:
    """Emit one JSON object per line on stdout (the QML-facing contract)."""
    print(json.dumps(payload, separators=(",", ":")), flush=True)


def setup_logging(debug: bool = False) -> logging.Logger:
    """Configure file logging under ~/.local/state/omascreencast and return it."""
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    logger = logging.getLogger("omascreencast.cast_engine")
    logger.setLevel(logging.DEBUG if debug else logging.INFO)
    if not logger.handlers:
        # Keep the log bounded: a long-running cast would otherwise grow it
        # without limit (M3 [ux]).
        handler = RotatingFileHandler(
            LOG_FILE, maxBytes=5 * 1024 * 1024, backupCount=2, encoding="utf-8"
        )
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
        logger.addHandler(handler)
    return logger


def _utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# In-memory LL-HLS ring buffer
# ---------------------------------------------------------------------------
class HlsBuffer:
    """Bounded, thread-safe, in-memory LL-HLS packager.

    Holds the fMP4 init segment, the most recent partial segments (parts), the
    most recent full segments, and the rendered playlist. Nothing touches disk.
    The segmenter thread writes; HTTP request threads read, and may block on a
    condition until the next part/segment is available (LL-HLS blocking reload).
    """

    def __init__(
        self,
        part_dur: float = PART_DUR,
        seg_parts: int = SEG_PARTS,
        list_parts: int = LIST_PARTS,
        keep_segments: int = KEEP_SEGMENTS,
        ll_parts: bool = True,
        ready_parts: int = READY_PARTS,
    ) -> None:
        self._cond = threading.Condition()
        self.part_dur = part_dur
        self.seg_parts = seg_parts
        self.list_parts = list_parts
        self.keep_segments = max(1, keep_segments)
        self.ready_parts = max(1, ready_parts)
        self.ll_parts = ll_parts

        self.init = b""
        self.parts: dict[int, bytes] = {}
        self.segments: dict[int, bytes] = {}
        self.current_parts: list[int] = []          # parts not yet in a segment
        self.seg_order: list[tuple[int, list[int]]] = []
        self.playlist = ""

        self._part_seq = 0
        self._seg_seq = 0

    # -- writers (segmenter thread) ----------------------------------------
    def set_init(self, data: bytes) -> None:
        with self._cond:
            self.init = data
            self._cond.notify_all()

    def add_part(self, data: bytes) -> int:
        with self._cond:
            idx = self._part_seq
            self._part_seq += 1
            self.parts[idx] = data
            self.current_parts.append(idx)
            self._evict_locked()
            self._render_locked()
            self._cond.notify_all()
            return idx

    def add_segment(self, data: bytes, part_idxs: list[int]) -> int:
        with self._cond:
            idx = self._seg_seq
            self._seg_seq += 1
            self.segments[idx] = data
            self.seg_order.append((idx, list(part_idxs)))
            self.current_parts = [p for p in self.current_parts if p not in part_idxs]
            while len(self.seg_order) > self.keep_segments:
                self.seg_order.pop(0)
            self._evict_locked()
            self._render_locked()
            self._cond.notify_all()
            return idx

    def _evict_locked(self) -> None:
        """Drop parts no longer referenced by a kept segment or the live edge.

        The live edge (``current_parts``) is itself bounded to ``list_parts`` so
        the buffer stays bounded even if the encoder stalls before a segment
        completes.
        """
        if len(self.current_parts) > self.list_parts:
            self.current_parts = self.current_parts[-self.list_parts:]
        keep = set(self.current_parts)
        for _, part_idxs in self.seg_order:
            keep.update(part_idxs)
        for idx in list(self.parts):
            if idx not in keep:
                del self.parts[idx]

    def _render_locked(self) -> None:
        # Reconcile the advertised window with the retained window: every
        # segment we keep is also listed, so `keep_segments` alone bounds both
        # playlist length and retained memory.
        keep_segs = self.seg_order[-self.keep_segments:]
        seg_duration = round(self.seg_parts * self.part_dur, 3)
        target_duration = max(1, int(math.ceil(seg_duration)))
        lines = [
            "#EXTM3U",
            "#EXT-X-VERSION:9",
            f"#EXT-X-TARGETDURATION:{target_duration}",
        ]
        if self.ll_parts:
            # Spike J validated PART-HOLD-BACK=1.5 for 0.5 s parts (3x
            # PART-TARGET); the spec minimum is 2x, so keep the measured value.
            lines += [
                f"#EXT-X-PART-INF:PART-TARGET={self.part_dur}",
                "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,"
                f"PART-HOLD-BACK={round(self.part_dur * 3, 3)}",
            ]
        lines += [
            "#EXT-X-INDEPENDENT-SEGMENTS",
            '#EXT-X-MAP:URI="init.mp4"',
            f"#EXT-X-MEDIA-SEQUENCE:{keep_segs[0][0] if keep_segs else self._seg_seq}",
        ]
        for seg_idx, seg_parts in keep_segs:
            lines.append(f"#EXTINF:{round(len(seg_parts) * self.part_dur, 3)},")
            if self.ll_parts:
                for p in seg_parts:
                    lines.append(
                        f'#EXT-X-PART:DURATION={self.part_dur},URI="part{p}.m4s",INDEPENDENT=YES'
                    )
            lines.append(f"seg{seg_idx}.m4s")
        if self.ll_parts:
            for p in self.current_parts:
                lines.append(
                    f'#EXT-X-PART:DURATION={self.part_dur},URI="part{p}.m4s",INDEPENDENT=YES'
                )
            if self._part_seq:
                lines.append(
                    f'#EXT-X-PRELOAD-HINT:TYPE=PART,URI="part{self._part_seq}.m4s"'
                )
        self.playlist = "\n".join(lines) + "\n"

    # -- readers (HTTP threads) --------------------------------------------
    def get(self, name: str) -> tuple[bytes, str] | None:
        """Return (body, content_type) for a request path, or None for 404."""
        with self._cond:
            if name == "stream.m3u8":
                if not self.playlist:
                    return None
                return self.playlist.encode("utf-8"), HLS_MIME
            if name == "init.mp4":
                return (self.init, MP4_MIME) if self.init else None
            if name.startswith("part") and name.endswith(".m4s"):
                idx = _int_between(name, "part", ".m4s")
                if idx is not None and idx in self.parts:
                    return self.parts[idx], MP4_MIME
            if name.startswith("seg") and name.endswith(".m4s"):
                idx = _int_between(name, "seg", ".m4s")
                if idx is not None and idx in self.segments:
                    return self.segments[idx], MP4_MIME
            return None

    def get_parts(self, idxs: list[int]) -> bytes:
        """Snapshot the given parts under the lock (segment assembly)."""
        with self._cond:
            return b"".join(self.parts[i] for i in idxs if i in self.parts)

    def ready(self) -> bool:
        with self._cond:
            return bool(self.init) and len(self.parts) >= self.ready_parts

    def wait_ready(self, timeout: float) -> bool:
        """Block until init plus ``ready_parts`` parts exist (fast start)."""
        deadline = time.monotonic() + timeout
        with self._cond:
            while not (self.init and len(self.parts) >= self.ready_parts):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self._cond.wait(min(remaining, 0.25))
            return True

    def _msn_available_locked(self, msn: int, part: int) -> bool:
        # `_seg_seq` is the index of the segment currently being assembled.
        if msn < self._seg_seq:
            return True
        if msn == self._seg_seq:
            return part < len(self.current_parts)
        return False

    def wait_playlist(self, msn: int, part: int = 0,
                      timeout: float = BLOCKING_RELOAD_TIMEOUT) -> bytes:
        """LL-HLS blocking reload: wait for the requested msn/part, then reply.

        A timeout returns the current playlist so a client is never held past
        the advertised hold-back; it simply retries.
        """
        deadline = time.monotonic() + timeout
        with self._cond:
            while self.playlist:
                if self._msn_available_locked(msn, part):
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self._cond.wait(min(remaining, 0.25))
            return self.playlist.encode("utf-8")

    def stats(self) -> dict:
        with self._cond:
            return {
                "initBytes": len(self.init),
                "parts": len(self.parts),
                "segments": len(self.segments),
                "playlistBytes": len(self.playlist),
                "currentParts": len(self.current_parts),
            }


def _int_between(name: str, prefix: str, suffix: str) -> int | None:
    core = name[len(prefix):-len(suffix)]
    return int(core) if core.isdigit() else None


# ---------------------------------------------------------------------------
# HTTP server (serves the in-memory buffer; CORS required by the receiver)
# ---------------------------------------------------------------------------
def make_handler(buf: HlsBuffer, log: logging.Logger):
    class _Handler(http.server.BaseHTTPRequestHandler):
        # HTTP/1.1 keep-alive: the receiver fetches a playlist and a new part
        # every few hundred ms; a fresh TCP connection per request adds latency
        # and TIME_WAIT churn (M3 [perf]).
        protocol_version = "HTTP/1.1"

        def _cors(self) -> None:
            self.send_header("Access-Control-Allow-Origin", "*")
            self.send_header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "*")

        def _serve(self, body: bytes, ctype: str, name: str) -> None:
            log.debug("http 200 %s (%dB) from %s", name, len(body), self.client_address[0])
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Cache-Control", "no-cache")
            self._cors()
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()

        def _query(self) -> dict:
            parsed = urllib.parse.urlparse(self.path)
            return urllib.parse.parse_qs(parsed.query)

        def do_GET(self) -> None:  # noqa: N802
            name = os.path.basename(self.path.split("?")[0])
            if name == "stream.m3u8":
                query = self._query()
                if "_HLS_msn" in query:
                    # LL-HLS blocking reload (`_HLS_msn`/`_HLS_part`).
                    try:
                        msn = int(query["_HLS_msn"][0])
                        part = int(query.get("_HLS_part", ["0"])[0])
                    except (ValueError, IndexError):
                        msn = part = 0
                    body = buf.wait_playlist(msn, part)
                    if body:
                        self._serve(body, HLS_MIME, name)
                        self.wfile.write(body)
                    else:
                        self.send_error(404)
                    return
            result = buf.get(name)
            if result is None:
                log.debug("http 404 %s from %s", name, self.client_address[0])
                self.send_error(404)
                return
            body, ctype = result
            self._serve(body, ctype, name)
            self.wfile.write(body)

        def do_HEAD(self) -> None:  # noqa: N802
            name = os.path.basename(self.path.split("?")[0])
            result = buf.get(name)
            if result is None:
                self.send_error(404)
                return
            body, ctype = result
            self._serve(body, ctype, name)

        def do_OPTIONS(self) -> None:  # noqa: N802
            self.send_response(204)
            self._cors()
            self.end_headers()

        def log_message(self, *args) -> None:
            pass

    return _Handler


# ---------------------------------------------------------------------------
# fMP4 box parsing + segmenter (ffmpeg stdout -> in-memory ring buffer)
# ---------------------------------------------------------------------------
def read_box(stream) -> tuple[bytes | None, bytes | None]:
    """Read one ISO-BMFF box from a binary stream; return (type, bytes)."""
    hdr = stream.read(8)
    if len(hdr) < 8:
        return None, None
    size = int.from_bytes(hdr[:4], "big")
    typ = hdr[4:8]
    if size == 1:
        ext = stream.read(8)
        if len(ext) < 8:
            return None, None
        size = int.from_bytes(ext, "big")
        return typ, hdr + ext + stream.read(size - 16)
    if size == 0:
        return typ, hdr + stream.read()
    return typ, hdr + stream.read(size - 8)


def segmenter(proc: subprocess.Popen, buf: HlsBuffer, log: logging.Logger,
              debug_dir: Path | None = None) -> None:
    """Parse fragmented MP4 from ffmpeg's stdout into the in-memory buffer."""
    init = b""
    while True:
        typ, box = read_box(proc.stdout)
        if box is None:
            return
        if typ in (b"ftyp", b"moov"):
            init += box
            if typ == b"moov":
                break
        else:
            break
    buf.set_init(init)
    if debug_dir is not None:
        (debug_dir / "init.mp4").write_bytes(init)
    log.info("init segment ready (%dB)", len(init))

    cur: list[int] = []
    while True:
        typ, box = read_box(proc.stdout)
        if box is None:
            return
        if typ != b"moof":
            continue
        _, mdat = read_box(proc.stdout)
        if mdat is None:
            return
        data = box + mdat
        idx = buf.add_part(data)
        if debug_dir is not None:
            (debug_dir / f"part{idx}.m4s").write_bytes(data)
        cur.append(idx)
        if len(cur) == buf.seg_parts:
            # Snapshot under the buffer lock: the HTTP threads must never see a
            # half-updated `parts` dict (M3 [perf] ring-buffer correctness).
            seg_data = buf.get_parts(cur)
            seg_idx = buf.add_segment(seg_data, cur)
            if debug_dir is not None:
                (debug_dir / f"seg{seg_idx}.m4s").write_bytes(seg_data)
            log.debug("segment %d ready (%dB)", seg_idx, len(seg_data))
            cur = []


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
_SIOCGIFADDR = 0x8915


def _iface_ipv4(name: str) -> str:
    """Return the IPv4 address of a network interface (Linux ioctl)."""
    try:
        import fcntl
    except ImportError:  # pragma: no cover - Linux-only project
        return ""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        packed = fcntl.ioctl(
            sock.fileno(), _SIOCGIFADDR, struct.pack("256s", name[:15].encode())
        )
        return socket.inet_ntoa(packed[20:24])
    except OSError:
        return ""
    finally:
        sock.close()


def _default_route_iface() -> str:
    """Parse /proc/net/route for the interface of the default route."""
    try:
        lines = Path("/proc/net/route").read_text(encoding="utf-8").splitlines()[1:]
    except OSError:
        return ""
    for line in lines:
        fields = line.split()
        if len(fields) < 4:
            continue
        if fields[1] == "00000000" and int(fields[3], 16) & 0x1:
            return fields[0]
    return ""


def _usable_ipv4(value: str) -> bool:
    try:
        addr = ipaddress.ip_address(value)
    except ValueError:
        return False
    return (
        addr.version == 4
        and not addr.is_loopback
        and not addr.is_link_local
        and not addr.is_unspecified
    )


def lan_ip() -> str:
    """Resolve a LAN-reachable IPv4 address without an external route probe.

    Collect candidates from the default-route interface and every other
    interface, then prefer a private (LAN) address. This avoids advertising a
    VPN/tunnel address when a real LAN interface exists. The old
    ``connect(("8.8.8.8", 80))`` probe remains only as a last resort (M3 [perf]).
    """
    candidates: list[str] = []

    def add(value: str) -> None:
        if _usable_ipv4(value) and value not in candidates:
            candidates.append(value)

    iface = _default_route_iface()
    if iface:
        add(_iface_ipv4(iface))

    try:
        for _index, name in socket.if_nameindex():
            if name == "lo":
                continue
            add(_iface_ipv4(name))
    except OSError:
        pass

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("8.8.8.8", 80))
        add(sock.getsockname()[0])
    except OSError:
        pass
    finally:
        sock.close()

    for addr in candidates:
        try:
            if ipaddress.ip_address(addr).is_private:
                return addr
        except ValueError:
            continue
    return candidates[0] if candidates else "127.0.0.1"


_ENCODER_CACHE: set[str] | None = None


def available_encoders() -> set[str]:
    """Cached set of ffmpeg encoder names (never fatal if ffmpeg is absent)."""
    global _ENCODER_CACHE
    if _ENCODER_CACHE is None:
        found: set[str] = set()
        try:
            result = subprocess.run(
                ["ffmpeg", "-hide_banner", "-encoders"],
                capture_output=True, text=True, timeout=10,
            )
            for line in (result.stdout or "").splitlines():
                parts = line.split()
                if len(parts) >= 2 and parts[0].startswith(("V", "A", "S")):
                    found.add(parts[1])
        except (OSError, subprocess.SubprocessError):
            found = set(CODEC_MAP.values())
        _ENCODER_CACHE = found
    return _ENCODER_CACHE


def resolve_codec(
    requested: str,
    log: logging.Logger,
    codecs: list[str] | None = None,
    host: str = "",
    device_uuid: str = "",
    allow_experimental_av1: bool = False,
) -> str:
    """Negotiate a per-device encoder (R3).

    ``auto`` prefers HEVC when the receiver advertises it, then H.264, then
    VP9; AV1 stays behind an explicit experimental flag. Unknown receivers get
    the safe H.264 default. An explicit request is still validated against the
    encoders this ffmpeg actually provides (M0 established that av1_vaapi and
    vp9_vaapi are absent on the reference hardware).
    """
    encoders = available_encoders()

    if requested != "auto":
        if requested == "av1" and not allow_experimental_av1:
            log.warning("AV1 requested without --experimental-av1; falling back")
            requested = "h264"
        encoder = CODEC_MAP.get(requested, "h264_vaapi")
        if encoder not in encoders:
            log.warning(
                "requested codec %s needs unavailable encoder %s; falling back to h264",
                requested, encoder,
            )
            return "h264_vaapi" if "h264_vaapi" in encoders else encoder
        return encoder

    if codecs is None:
        info = session_state.device_info(device_uuid, host)
        codecs = list(info.get("codecs") or [])
    if not codecs and host:
        codecs = probe_codecs(host)

    for codec in CODEC_ORDER:
        if codec not in codecs:
            continue
        if codec == "av1" and not allow_experimental_av1:
            continue
        encoder = CODEC_MAP[codec]
        if encoder in encoders:
            log.info(
                "codec auto -> %s (receiver codecs: %s)",
                encoder, ",".join(codecs) or "unknown",
            )
            return encoder
        log.warning("receiver advertises %s but encoder %s is unavailable", codec, encoder)

    log.info("codec auto -> h264_vaapi (no compatible negotiation; safe default)")
    return "h264_vaapi"


def detect_render_node(requested: str = "") -> str:
    """Pick a VAAPI render node (Spike L); never hardcode renderD128 blindly."""
    requested = str(requested or "").strip()
    if requested and requested != "auto":
        return requested
    env = os.environ.get("OMASCREENCAST_RENDER_NODE", "").strip()
    if env:
        return env
    try:
        nodes = sorted(str(p) for p in Path("/dev/dri").glob("renderD*"))
    except OSError:
        nodes = []
    return nodes[0] if nodes else "/dev/dri/renderD128"


def detect_capture_backend(
    requested: str, log: logging.Logger, ll_hls: bool = True
) -> str:
    """Pick a capture backend.

    LL-HLS needs one keyframe-aligned fragment per advertised part. `wf-recorder`
    lets us pin a short GOP (`-p g=...`); `wl-screenrec` does not expose one, so
    with LL-HLS enabled we prefer `wf-recorder`. `wl-screenrec` remains the
    fallback and is honoured when explicitly requested (or when LL parts are
    disabled).
    """
    requested = str(requested or "auto")
    if requested in ("wf-recorder", "wl-screenrec"):
        if shutil.which(requested):
            return requested
        log.warning("capture backend %s not found; using auto", requested)
    if ll_hls and shutil.which("wf-recorder"):
        return "wf-recorder"
    if shutil.which("wl-screenrec"):
        if ll_hls:
            log.warning(
                "wf-recorder not found; wl-screenrec cannot pin a GOP, so "
                "LL-HLS part timing may drift"
            )
        return "wl-screenrec"
    return "wf-recorder"


def tool_help(name: str) -> str:
    """Best-effort `--help` text used to probe supported capture flags."""
    try:
        result = subprocess.run(
            [name, "--help"], capture_output=True, text=True, timeout=5
        )
        return (result.stdout or "") + (result.stderr or "")
    except (OSError, subprocess.SubprocessError):
        return ""


def capture_command(
    backend: str,
    output: str,
    fps: int,
    codec: str,
    render_node: str,
    bitrate: str,
    gop: int,
    fifo: Path,
    help_text: str = "",
) -> list[str]:
    """Build the capture argv for the selected backend.

    wf-recorder sets B-frames to 0 and a short GOP for LL-HLS; wl-screenrec gets
    only flags it advertises in `--help` (its CLI differs between releases).
    """
    if backend == "wl-screenrec":
        command = [
            "wl-screenrec", "-o", output, "-f", str(fifo),
            "--max-fps", str(fps), "--ffmpeg-encoder", codec,
        ]
        if "--bitrate" in help_text:
            command += ["--bitrate", str(bitrate)]
        return command

    command = [
        "wf-recorder", "-D", "-r", str(fps), "-o", output, "-c", codec,
        "-b", "0",  # no B-frames: lower latency (R5)
    ]
    if codec.endswith("_vaapi"):
        command += ["-d", render_node]
    # MPEG-TS cannot carry VP9/AV1. Use Matroska for those encoders; ffmpeg
    # autodetects the container from the FIFO, so no remux change is needed.
    muxer = "mpegts" if codec in ("hevc_vaapi", "h264_vaapi") else "matroska"
    command += [
        "-p", f"g={gop}", "-p", f"b={bitrate}",
        "-m", muxer, "-f", str(fifo),
    ]
    return command


def _select_cast(casts, device_uuid: str, host: str):
    for cast in casts:
        info = cast.cast_info
        if device_uuid and getattr(info, "uuid", None) == device_uuid:
            return cast
        if host and info.host == host:
            return cast
    return None


def _cast_from_host(host: str, log: logging.Logger, uuid: str = "",
                    name: str = "", model: str = "", timeout: float = 10):
    """Connect to a receiver directly by IP, bypassing mDNS (R6).

    pychromecast has changed the ``get_chromecast_from_host`` signature across
    releases, so try the known forms and keep the first that accepts our
    arguments. Only ``TypeError`` (signature mismatch) is swallowed; network
    failures propagate to the caller.
    """
    import pychromecast

    factory = getattr(pychromecast, "get_chromecast_from_host", None)
    if factory is None:
        raise RuntimeError("pychromecast has no get_chromecast_from_host()")
    address = str(host or "").strip()
    if not address:
        raise ValueError("empty receiver host")
    attempts = (
        lambda: factory(
            (address, 8009, uuid or None, model or None, name or None),
            timeout=timeout,
        ),
        lambda: factory(
            address, port=8009, uuid=uuid or None,
            model_name=model or None, friendly_name=name or None, timeout=timeout,
        ),
        lambda: factory(
            address, 8009, uuid or None, model or None, name or None,
        ),
    )
    errors: list[str] = []
    for attempt in attempts:
        try:
            return attempt()
        except TypeError as exc:
            errors.append(str(exc))
    raise RuntimeError(
        "unsupported pychromecast get_chromecast_from_host signature: "
        + "; ".join(errors)
    )


def _teardown(cap, ff, srv, mc, run_dir: Path, log: logging.Logger,
              browser=None) -> None:
    if mc is not None:
        try:
            mc.stop()
        except Exception as exc:  # noqa: BLE001
            log.debug("media_controller.stop: %s", exc)
    if browser is not None:
        try:
            browser.stop()
        except Exception as exc:  # noqa: BLE001
            log.debug("chromecast browser.stop: %s", exc)
    for proc in (ff, cap):
        if proc is None:
            continue
        try:
            proc.send_signal(signal.SIGINT)
            proc.wait(timeout=5)
        except Exception:  # noqa: BLE001
            try:
                proc.kill()
            except Exception:  # noqa: BLE001
                pass
    if srv is not None:
        try:
            srv.shutdown()
            srv.server_close()
        except Exception:  # noqa: BLE001
            pass
    shutil.rmtree(run_dir, ignore_errors=True)


# ---------------------------------------------------------------------------
# Device discovery (mDNS via zeroconf, avahi-browse fallback, manual host)
#
# The receiver is found over mDNS (`_googlecast._tcp`). We deliberately avoid
# pychromecast's blocking `get_chromecasts(timeout=10)` per call: `--watch`
# keeps a single long-lived zeroconf browser alive and streams incremental
# snapshots as JSONL, so the panel updates without a 10 s stall (M1, [perf]).
# `avahi-browse -rtp` is the fallback when zeroconf is unavailable, and a
# manually typed IP is the last resort for mDNS-blocked / cross-VLAN receivers
# (R6). Per-device codec capabilities are probed once and cached so the engine
# can negotiate without re-querying (feeds M3, R3).
# ---------------------------------------------------------------------------
SERVICE_TYPE = "_googlecast._tcp.local."
DEVICE_CACHE = STATE_DIR / "devices.json"
DEFAULT_DISCOVERY_TIMEOUT = 3.0
DISCOVERY_SETTLE = 5.0
EUREKA_TIMEOUT = 1.5
DEFAULT_CODECS = ["h264"]  # R3: safe default when the receiver is unknown
CODEC_ORDER = ["hevc", "h264", "vp9", "av1"]


@dataclass
class Device:
    """A discovered (or manually entered) Chromecast receiver."""

    uuid: str = ""
    name: str = ""
    host: str = ""
    port: int = 8009
    model: str = ""
    online: bool = True
    codecs: list[str] = field(default_factory=list)

    def as_dict(self) -> dict:
        return {
            "uuid": self.uuid,
            "name": self.name,
            "host": self.host,
            "port": self.port,
            "model": self.model,
            "online": self.online,
            "codecs": list(self.codecs),
        }


def _clean_name(name: str, service_type: str = SERVICE_TYPE) -> str:
    """Strip the mDNS service suffix from a discovered instance name."""
    suffix = "." + service_type
    if name.endswith(suffix):
        name = name[: -len(suffix)]
    return name.strip().rstrip(".")


def _first_ipv4(addresses) -> str:
    """The receiver announces IPv4 and IPv6; the pipeline speaks IPv4."""
    for raw in addresses or []:
        try:
            addr = ipaddress.ip_address(str(raw))
        except ValueError:
            continue
        if addr.version == 4:
            return str(addr)
    return ""


def parse_txt(txt: str) -> dict:
    """Parse a DNS-SD TXT record into a dict.

    Handles both the zeroconf form (NUL-separated ``k=v``) and the avahi ``-p``
    form (each pair wrapped in double quotes).
    """
    props: dict[str, str] = {}
    text = str(txt or "").strip()
    if '"' in text:
        parts = [p for p in text.split('"') if p.strip()]
    else:
        parts = text.split("\x00")
    for chunk in parts:
        chunk = chunk.strip()
        if not chunk or "=" not in chunk:
            continue
        key, _, value = chunk.partition("=")
        props[key.strip()] = value.strip()
    return props


def _decode_props(properties) -> dict:
    """Decode a zeroconf ``ServiceInfo.properties`` mapping to str->str."""
    props: dict[str, str] = {}
    for key, value in (properties or {}).items():
        k = key.decode("utf-8", "replace") if isinstance(key, bytes) else str(key)
        v = value.decode("utf-8", "replace") if isinstance(value, bytes) else str(value)
        props[k] = v
    return props


def codecs_from_eureka(info: dict) -> list[str]:
    """Best-effort codec list from a Cast ``eureka_info`` payload.

    The receiver does not advertise codecs in a stable, documented field, so we
    look at the few places they have been observed and fall back to the safe
    H.264 default (R3) when nothing usable is found.
    """
    if not isinstance(info, dict):
        return list(DEFAULT_CODECS)
    detail = info.get("detail") if isinstance(info.get("detail"), dict) else info
    raw = None
    for key in ("supported_codecs", "codecs", "video_codecs"):
        if isinstance(detail.get(key), list):
            raw = detail[key]
            break
    if raw is None:
        return list(DEFAULT_CODECS)
    found: set[str] = set()
    for item in raw:
        name = str(item).lower()
        if "hevc" in name or "h265" in name or "h.265" in name:
            found.add("hevc")
        elif "h264" in name or "avc" in name or "h.264" in name:
            found.add("h264")
        elif "vp9" in name:
            found.add("vp9")
        elif "av1" in name:
            found.add("av1")
    ordered = [c for c in CODEC_ORDER if c in found]
    return ordered or list(DEFAULT_CODECS)


def parse_avahi(output: str) -> list[Device]:
    """Parse ``avahi-browse -rtp _googlecast._tcp`` output into devices.

    Resolved lines look like::

        =;wlan0;IPv4;Living Room;_googlecast._tcp;local;tv.local;192.168.1.5;8009;"id=...","md=Chromecast","fn=Living Room"
    """
    devices: dict[str, Device] = {}
    for line in str(output or "").splitlines():
        if not line.startswith("="):
            continue
        fields = line.split(";")
        if len(fields) < 10:
            continue
        name = fields[3]
        address = fields[7]
        try:
            port = int(fields[8])
        except ValueError:
            port = 8009
        props = parse_txt(fields[9])
        host = _first_ipv4([address]) or address.strip()
        if not host:
            continue
        uuid = props.get("id", "")
        key = uuid or host
        devices[key] = Device(
            uuid=uuid,
            name=props.get("fn") or _clean_name(name),
            host=host,
            port=port,
            model=props.get("md", ""),
            online=True,
            codecs=list(DEFAULT_CODECS),
        )
    return sorted(devices.values(), key=lambda d: (d.name.lower(), d.host))


def manual_device(host: str, name: str = "") -> Device:
    """Build a device from a manually typed IP (mDNS-blocked fallback, R6)."""
    host = str(host or "").strip()
    return Device(
        uuid="",
        name=name or host,
        host=host,
        port=8009,
        model="",
        online=True,
        codecs=list(DEFAULT_CODECS),
    )


def _load_cache() -> dict:
    if not DEVICE_CACHE.exists():
        return {}
    try:
        data = json.loads(DEVICE_CACHE.read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (json.JSONDecodeError, OSError):
        return {}


@contextmanager
def _file_lock(path: Path):
    """Advisory exclusive lock around a state/cache read-modify-write."""
    try:
        import fcntl
    except ImportError:  # pragma: no cover - Linux-only project
        yield
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(str(path) + ".lock", "a", encoding="utf-8") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX)
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def _save_cache(cache: dict) -> None:
    try:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        with _file_lock(DEVICE_CACHE):
            tmp = DEVICE_CACHE.with_suffix(".json.tmp")
            tmp.write_text(json.dumps(cache, indent=2) + "\n", encoding="utf-8")
            tmp.replace(DEVICE_CACHE)
    except OSError:
        pass


def _apply_cached_codecs(devices: list[Device], cache: dict) -> None:
    for dev in devices:
        cached = cache.get(dev.uuid or dev.host)
        if isinstance(cached, dict) and isinstance(cached.get("codecs"), list) and cached["codecs"]:
            dev.codecs = list(cached["codecs"])


def probe_codecs(host: str, timeout: float = EUREKA_TIMEOUT) -> list[str]:
    """Best-effort codec probe via the Cast ``eureka_info`` endpoint (R3)."""
    url = f"http://{host}:8008/setup/eureka_info?options=detail"
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            info = json.loads(resp.read().decode("utf-8", "replace"))
    except Exception:  # noqa: BLE001 - a network probe must never be fatal
        return list(DEFAULT_CODECS)
    return codecs_from_eureka(info)


def _zeroconf_discover(timeout: float, log: logging.Logger) -> list[Device]:
    """One-shot mDNS browse; returns whatever resolved within ``timeout``."""
    try:
        from zeroconf import ServiceBrowser, ServiceListener, Zeroconf
    except ImportError as exc:
        log.warning("zeroconf unavailable: %s", exc)
        return []

    found: dict[str, Device] = {}

    class _Listener(ServiceListener):
        def _resolve(self, zc, type_, name):
            info = zc.get_service_info(type_, name, timeout=int(timeout * 1000))
            if info is None:
                return
            props = _decode_props(info.properties)
            host = _first_ipv4(info.parsed_addresses())
            if not host:
                return
            uuid = props.get("id", "")
            found[uuid or host] = Device(
                uuid=uuid,
                name=props.get("fn") or _clean_name(name),
                host=host,
                port=info.port or 8009,
                model=props.get("md", ""),
                online=True,
                codecs=list(DEFAULT_CODECS),
            )

        def add_service(self, zc, type_, name):  # noqa: N802
            self._resolve(zc, type_, name)

        def update_service(self, zc, type_, name):  # noqa: N802
            self._resolve(zc, type_, name)

        def remove_service(self, zc, type_, name):  # noqa: N802
            pass

    zc = Zeroconf()
    browser = ServiceBrowser(zc, SERVICE_TYPE, _Listener())
    try:
        time.sleep(timeout)
    finally:
        browser.cancel()
        zc.close()
    return sorted(found.values(), key=lambda d: (d.name.lower(), d.host))


def _avahi_discover(log: logging.Logger) -> list[Device]:
    """Fallback discovery via ``avahi-browse`` when zeroconf is unavailable."""
    if shutil.which("avahi-browse") is None:
        return []
    try:
        result = subprocess.run(
            ["avahi-browse", "-rtp", SERVICE_TYPE],
            capture_output=True, text=True, timeout=6,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        log.warning("avahi-browse failed: %s", exc)
        return []
    return parse_avahi(result.stdout)


def discover_once(timeout: float, log: logging.Logger, host: str = "") -> list[Device]:
    """Resolve the current device list, probing + caching codecs once per device."""
    if host:
        return [manual_device(host)]
    devices = _zeroconf_discover(timeout, log)
    if not devices:
        log.info("zeroconf found nothing; trying avahi-browse fallback")
        devices = _avahi_discover(log)
    cache = _load_cache()
    _apply_cached_codecs(devices, cache)
    changed = False
    for dev in devices:
        key = dev.uuid or dev.host
        if key not in cache:
            dev.codecs = probe_codecs(dev.host)
            cache[key] = {
                "name": dev.name, "model": dev.model,
                "host": dev.host, "codecs": dev.codecs,
            }
            changed = True
    if changed:
        _save_cache(cache)
    return devices


def _watch(args: argparse.Namespace, log: logging.Logger) -> int:
    """Long-lived zeroconf browser streaming incremental snapshots as JSONL."""
    if args.host:
        emit({
            "type": "discovery",
            "devices": [manual_device(args.host).as_dict()],
            "browsing": False,
        })
        emit({"type": "phase", "phase": "idle", "detail": "manual host"})
        return 0

    try:
        from zeroconf import ServiceBrowser, ServiceListener, Zeroconf
    except ImportError as exc:
        log.error("zeroconf unavailable: %s", exc)
        emit({"type": "phase", "phase": "error", "detail": f"zeroconf not available: {exc}"})
        return 1

    found: dict[str, Device] = {}
    lock = threading.Lock()
    cache = _load_cache()
    started = time.monotonic()

    def publish() -> None:
        with lock:
            devices = sorted(found.values(), key=lambda d: (d.name.lower(), d.host))
        # `browsing` keeps the panel spinner honest: an empty early snapshot
        # does not mean discovery has finished.
        browsing = not devices and (time.monotonic() - started) < DISCOVERY_SETTLE
        emit({
            "type": "discovery",
            "devices": [d.as_dict() for d in devices],
            "browsing": browsing,
        })

    class _Listener(ServiceListener):
        def _resolve(self, zc, type_, name):
            info = zc.get_service_info(type_, name, timeout=2000)
            if info is None:
                return
            props = _decode_props(info.properties)
            host = _first_ipv4(info.parsed_addresses())
            if not host:
                return
            uuid = props.get("id", "")
            key = uuid or host
            dev = Device(
                uuid=uuid,
                name=props.get("fn") or _clean_name(name),
                host=host,
                port=info.port or 8009,
                model=props.get("md", ""),
                online=True,
                codecs=list(DEFAULT_CODECS),
            )
            cached = cache.get(key)
            if isinstance(cached, dict) and cached.get("codecs"):
                dev.codecs = list(cached["codecs"])
            else:
                dev.codecs = probe_codecs(dev.host)
                cache[key] = {
                    "name": dev.name, "model": dev.model,
                    "host": dev.host, "codecs": dev.codecs,
                }
                _save_cache(cache)
            with lock:
                found[key] = dev
            publish()

        def add_service(self, zc, type_, name):  # noqa: N802
            self._resolve(zc, type_, name)

        def update_service(self, zc, type_, name):  # noqa: N802
            self._resolve(zc, type_, name)

        def remove_service(self, zc, type_, name):  # noqa: N802
            clean = _clean_name(name)
            with lock:
                for dev in found.values():
                    if dev.name == clean:
                        dev.online = False
            publish()

    zc = Zeroconf()
    browser = ServiceBrowser(zc, SERVICE_TYPE, _Listener())
    emit({"type": "phase", "phase": "discovering", "detail": "browsing mDNS"})
    publish()  # initial, possibly empty, snapshot (still marked `browsing`)

    stop = threading.Event()
    settled = False

    def _on_signal(signum, _frame):
        log.info("discovery signal %s received", signum)
        stop.set()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)
    try:
        while not stop.is_set():
            if not settled and time.monotonic() - started >= DISCOVERY_SETTLE:
                settled = True
                publish()  # final empty snapshot with browsing=false
            time.sleep(0.5)
    finally:
        browser.cancel()
        zc.close()
    emit({"type": "phase", "phase": "idle", "detail": "discovery stopped"})
    return 0


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------
def cmd_discovery(args: argparse.Namespace) -> int:
    log = setup_logging(args.debug)
    if args.host:
        emit({"type": "discovery", "devices": [manual_device(args.host).as_dict()]})
        return 0
    if args.watch:
        return _watch(args, log)
    devices = discover_once(args.timeout, log)
    emit({"type": "discovery", "devices": [d.as_dict() for d in devices]})
    return 0


def cmd_stream(args: argparse.Namespace) -> int:
    log = setup_logging(args.debug)
    log.info(
        "stream start device=%s host=%s output=%s codec=%s fps=%s bitrate=%s port=%s",
        args.device, args.host, args.output, args.codec, args.fps, args.bitrate, args.port,
    )
    # Register early so the panel sees "starting" and `remove` can always find us.
    session_state.update_active({
        "pid": os.getpid(),
        "output": args.output,
        "phase": "starting",
    })

    if not args.device and not args.host:
        emit({"type": "phase", "phase": "error", "detail": "one of --device or --host is required"})
        session_state.mark_stopped()
        return 1

    try:
        import pychromecast
    except ImportError as exc:
        log.error("pychromecast unavailable: %s", exc)
        emit({"type": "phase", "phase": "error", "detail": f"pychromecast not available: {exc}"})
        session_state.mark_stopped()
        return 1

    emit({"type": "phase", "phase": "starting", "detail": "preparing pipeline"})

    device_info = session_state.device_info(args.device, args.host)
    device_codecs = list(device_info.get("codecs") or [])
    receiver_host = args.host or str(device_info.get("host") or "")
    codec = resolve_codec(
        args.codec, log, codecs=device_codecs,
        host=receiver_host, device_uuid=args.device,
        allow_experimental_av1=args.experimental_av1,
    )
    gop = args.gop or max(1, args.fps // 2)
    render_node = detect_render_node(args.render_node)
    backend = detect_capture_backend(args.backend, log, ll_hls=not args.no_ll)
    log.info("capture backend=%s render-node=%s codec=%s gop=%s", backend, render_node, codec, gop)

    # Install signal handlers before any child process or server exists, so a
    # disconnect during setup still tears everything down (M2/M3).
    stop = threading.Event()

    def _on_signal(signum, _frame):
        log.info("signal %s received", signum)
        stop.set()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    buf = HlsBuffer(
        part_dur=args.part_dur,
        seg_parts=args.seg_parts,
        list_parts=args.list_parts,
        keep_segments=args.keep_segments,
        ll_parts=not args.no_ll,
    )

    debug_dir = None
    if args.debug:
        debug_dir = DEBUG_DIR
        debug_dir.mkdir(parents=True, exist_ok=True)

    # The stream URL must point at this machine, so resolve the local LAN
    # address first and bind the HTTP server to exactly that interface rather
    # than 0.0.0.0 (M3 [perf]).
    ip = lan_ip()
    url = f"http://{ip}:{args.port}/stream.m3u8"
    handler = make_handler(buf, log)
    try:
        srv = http.server.ThreadingHTTPServer((ip, args.port), handler)
    except OSError as exc:
        log.error("cannot bind %s:%s: %s", ip, args.port, exc)
        emit({
            "type": "phase", "phase": "error",
            "detail": f"cannot bind {ip}:{args.port}: {exc}",
        })
        session_state.mark_stopped()
        return 1
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    log.info("serving %s", url)

    # Capture -> encode. The capture tool encodes; ffmpeg remuxes to fragmented
    # MP4. The FIFO is a pipe (not a buffer); segments themselves stay in RAM.
    run_dir = Path(tempfile.mkdtemp(prefix="omascreencast-"))
    fifo = run_dir / "capture.stream"
    os.mkfifo(fifo)

    cap = ff = None
    mc = None
    browser = None
    try:
        cap = subprocess.Popen(
            capture_command(
                backend, args.output, args.fps, codec, render_node,
                args.bitrate, gop, fifo,
                help_text=tool_help("wl-screenrec") if backend == "wl-screenrec" else "",
            ),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        ff = subprocess.Popen(
            [
                "ffmpeg", "-hide_banner", "-loglevel", "error", "-i", str(fifo),
                "-c", "copy",
                "-movflags", "frag_keyframe+empty_moov+default_base_moof",
                "-f", "mp4", "-",
            ],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        threading.Thread(
            target=segmenter, args=(ff, buf, log, debug_dir), daemon=True
        ).start()

        # Start casting as soon as the init segment and two parts exist
        # (condition wait, not a fixed polling loop): shorter time-to-first-frame.
        if not buf.wait_ready(timeout=10):
            log.error("pipeline produced no segments in time")
            emit({"type": "phase", "phase": "error", "detail": "no segments produced"})
            return 1

        emit({"type": "phase", "phase": "discovering", "detail": "locating receiver"})
        cast = None
        casts, browser = pychromecast.get_chromecasts(timeout=10)
        cast = _select_cast(casts, args.device, args.host)
        if cast is None and args.host:
            # mDNS-blocked / cross-VLAN fallback: connect straight to the IP
            # rather than requiring discovery to see the receiver (R6).
            try:
                cast = _cast_from_host(
                    args.host, log, uuid=args.device,
                    name=str(device_info.get("name") or ""),
                    model=str(device_info.get("model") or ""),
                )
            except Exception as exc:  # noqa: BLE001
                log.error("direct connect to %s failed: %s", args.host, exc)
        if cast is None:
            log.error("receiver not found device=%s host=%s", args.device, args.host)
            emit({"type": "phase", "phase": "error", "detail": "receiver not found"})
            return 1

        try:
            cast.wait(timeout=10)
        except TypeError:  # older pychromecast: no timeout argument
            cast.wait()
        mc = cast.media_controller
        mc.play_media(url, HLS_MIME)
        try:
            mc.block_until_active(timeout=20)
        except Exception as exc:  # noqa: BLE001
            log.warning("block_until_active: %s", exc)
        state = str(getattr(getattr(mc, "status", None), "player_state", "") or "")
        if state and state not in ("PLAYING", "BUFFERING"):
            log.error("receiver did not start playback (state=%s)", state)
            emit({
                "type": "phase", "phase": "error",
                "detail": f"receiver did not start playback (state={state})",
            })
            return 1

        session_state.update_active({
            "pid": os.getpid(),
            "deviceUuid": args.device,
            "url": url,
            "port": args.port,
            "codec": codec,
            "phase": "casting",
            "startedAt": _utc_now(),
        })
        emit({"type": "phase", "phase": "casting", "url": url, "device": args.device})
        log.info("casting %s", url)

        deadline = time.time() + args.duration if args.duration else None
        while not stop.is_set():
            if deadline is not None and time.time() >= deadline:
                break
            if cap.poll() is not None or ff.poll() is not None:
                log.warning("pipeline process exited early")
                emit({"type": "phase", "phase": "error", "detail": "capture/encode exited"})
                break
            time.sleep(0.5)
    finally:
        _teardown(cap, ff, srv, mc, run_dir, log, browser=browser)
        session_state.mark_stopped()
        log.info("stream stopped; buffer stats=%s", buf.stats())
        emit({"type": "phase", "phase": "idle", "detail": "stopped"})
    return 0


def cmd_stop(args: argparse.Namespace) -> int:
    log = setup_logging(False)
    active = session_state.load().get("active") or {}
    pid = active.get("pid")
    if not pid:
        emit({"type": "phase", "phase": "idle", "detail": "no active session"})
        return 0

    log.info("stopping pid %s (device=%s)", pid, active.get("deviceUuid"))
    if not session_state.stop_process(pid):
        log.error("pid %s did not exit", pid)
        emit({"type": "phase", "phase": "error", "detail": f"cannot stop pid {pid}"})
        return 1

    session_state.mark_stopped()
    emit({"type": "phase", "phase": "idle", "detail": "stopped"})
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    disc = sub.add_parser("discovery", help="discover Chromecast devices")
    disc.add_argument("--timeout", type=float, default=DEFAULT_DISCOVERY_TIMEOUT,
                      help="seconds to browse mDNS in one-shot mode")
    disc.add_argument("--watch", action="store_true",
                      help="keep a long-lived browser and stream incremental JSONL snapshots")
    disc.add_argument("--host", default="", help="manual receiver IP (skips mDNS, R6)")
    disc.add_argument("--debug", action="store_true", help="verbose logging")

    stream = sub.add_parser("stream", help="capture a virtual output and cast it")
    stream.add_argument("--device", default="", help="receiver uuid")
    stream.add_argument("--host", default="", help="receiver IP (alternative to --device)")
    stream.add_argument("--output", default="HEADLESS-1", help="HEADLESS-* output name")
    stream.add_argument("--res", default="1920x1080@60", help="WxH@Hz (used when creating the output, M2)")
    stream.add_argument("--codec", default="auto", choices=("auto", "hevc", "h264", "vp9", "av1"))
    stream.add_argument("--fps", type=int, default=30)
    stream.add_argument("--bitrate", default="6M", help="encoder bitrate, e.g. 6M")
    stream.add_argument("--render-node", default=DEFAULT_RENDER_NODE,
                        help="VAAPI render node ('auto' enumerates /dev/dri/renderD*)")
    stream.add_argument("--backend", default="auto",
                        choices=("auto", "wf-recorder", "wl-screenrec"),
                        help="capture backend (auto prefers wl-screenrec when installed)")
    stream.add_argument("--experimental-av1", action="store_true",
                        help="allow AV1 negotiation (off by default)")
    stream.add_argument("--port", type=int, default=DEFAULT_PORT, help="local HTTP port")
    stream.add_argument("--gop", type=int, default=0, help="encoder GOP in frames (0 = fps/2)")
    stream.add_argument("--part-dur", type=float, default=PART_DUR, help="seconds per partial segment")
    stream.add_argument("--seg-parts", type=int, default=SEG_PARTS, help="parts per full segment")
    stream.add_argument("--list-parts", type=int, default=LIST_PARTS, help="parts kept in the ring buffer")
    stream.add_argument("--keep-segments", type=int, default=KEEP_SEGMENTS, help="full segments kept")
    stream.add_argument("--no-ll", action="store_true", help="plain fMP4 HLS (no LL-HLS tags)")
    stream.add_argument("--duration", type=float, default=0.0, help="auto-stop after N seconds (0 = until signalled)")
    stream.add_argument("--debug", action="store_true",
                        help="verbose logging + dump segments to ~/.local/state/omascreencast/debug")

    stop = sub.add_parser("stop", help="stop the active cast")
    stop.add_argument("--device", default="", help="receiver uuid (default: active session)")

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {"discovery": cmd_discovery, "stream": cmd_stream, "stop": cmd_stop}
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
