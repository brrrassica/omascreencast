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
filesystem objects are a short-lived FIFO (a pipe, not a buffer) between
wf-recorder and ffmpeg, and the logs/debug dumps under
``~/.config/omascreencast``.

The receiver only accepts HLS (raw MPEG-TS and DASH are rejected) and requires
CORS headers; see ``plans/m0-spikes.md`` (Spikes H/J) and ``plans/milestones.md``
(M3, R2a-R2f).

Planned phases: discovering -> starting -> casting -> idle/error.
"""

from __future__ import annotations

import argparse
import http.server
import json
import logging
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

# ---------------------------------------------------------------------------
# Paths. Logs and opt-in debug dumps live under ~/.config/omascreencast (a
# deliberate project decision); runtime session info for `stop` lives there too.
# ---------------------------------------------------------------------------
CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "omascreencast"
LOG_FILE = CONFIG_DIR / "cast_engine.log"
DEBUG_DIR = CONFIG_DIR / "debug"
RUNTIME_FILE = CONFIG_DIR / "runtime.json"

DEFAULT_PORT = 7236
DEFAULT_RENDER_NODE = "/dev/dri/renderD128"

# LL-HLS tuning (mirrors the validated Spike J parameters).
PART_DUR = 0.5          # seconds per partial segment
SEG_PARTS = 4           # parts per full segment (2 s at 0.5 s parts)
LIST_PARTS = 6          # parts kept in the ring buffer
KEEP_SEGMENTS = 4       # full segments kept in the ring buffer

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
    """Configure file logging under ~/.config/omascreencast and return it."""
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    logger = logging.getLogger("omascreencast.cast_engine")
    logger.setLevel(logging.DEBUG if debug else logging.INFO)
    if not logger.handlers:
        handler = logging.FileHandler(LOG_FILE, encoding="utf-8")
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
        logger.addHandler(handler)
    return logger


def _utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# Runtime session info (used by `stop` to find the running stream process)
# ---------------------------------------------------------------------------
def _write_runtime(data: dict) -> None:
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    tmp = RUNTIME_FILE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    tmp.replace(RUNTIME_FILE)


def _read_runtime() -> dict | None:
    if not RUNTIME_FILE.exists():
        return None
    try:
        return json.loads(RUNTIME_FILE.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None


def _clear_runtime() -> None:
    try:
        RUNTIME_FILE.unlink()
    except OSError:
        pass


# ---------------------------------------------------------------------------
# In-memory LL-HLS ring buffer
# ---------------------------------------------------------------------------
class HlsBuffer:
    """Bounded, thread-safe, in-memory LL-HLS packager.

    Holds the fMP4 init segment, the most recent partial segments (parts), the
    most recent full segments, and the rendered playlist. Nothing touches disk.
    The segmenter thread writes; HTTP request threads read.
    """

    def __init__(
        self,
        part_dur: float = PART_DUR,
        seg_parts: int = SEG_PARTS,
        list_parts: int = LIST_PARTS,
        keep_segments: int = KEEP_SEGMENTS,
        ll_parts: bool = True,
    ) -> None:
        self._lock = threading.Lock()
        self.part_dur = part_dur
        self.seg_parts = seg_parts
        self.list_parts = list_parts
        self.keep_segments = keep_segments
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
        with self._lock:
            self.init = data

    def add_part(self, data: bytes) -> int:
        with self._lock:
            idx = self._part_seq
            self._part_seq += 1
            self.parts[idx] = data
            self.current_parts.append(idx)
            self._evict_locked()
            self._render_locked()
            return idx

    def add_segment(self, data: bytes, part_idxs: list[int]) -> int:
        with self._lock:
            idx = self._seg_seq
            self._seg_seq += 1
            self.segments[idx] = data
            self.seg_order.append((idx, list(part_idxs)))
            self.current_parts = [p for p in self.current_parts if p not in part_idxs]
            while len(self.seg_order) > self.keep_segments:
                self.seg_order.pop(0)
            self._evict_locked()
            self._render_locked()
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
        keep_segs = self.seg_order[-3:]
        lines = ["#EXTM3U", "#EXT-X-VERSION:9", "#EXT-X-TARGETDURATION:2"]
        if self.ll_parts:
            lines += [
                f"#EXT-X-PART-INF:PART-TARGET={self.part_dur}",
                "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=1.5",
            ]
        lines += [
            "#EXT-X-INDEPENDENT-SEGMENTS",
            '#EXT-X-MAP:URI="init.mp4"',
            f"#EXT-X-MEDIA-SEQUENCE:{keep_segs[0][0] if keep_segs else 0}",
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
        with self._lock:
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

    def ready(self) -> bool:
        with self._lock:
            return bool(self.init) and len(self.parts) >= 3

    def stats(self) -> dict:
        with self._lock:
            return {
                "initBytes": len(self.init),
                "parts": len(self.parts),
                "segments": len(self.segments),
                "playlistBytes": len(self.playlist),
            }


def _int_between(name: str, prefix: str, suffix: str) -> int | None:
    core = name[len(prefix):-len(suffix)]
    return int(core) if core.isdigit() else None


# ---------------------------------------------------------------------------
# HTTP server (serves the in-memory buffer; CORS required by the receiver)
# ---------------------------------------------------------------------------
def make_handler(buf: HlsBuffer, log: logging.Logger):
    class _Handler(http.server.BaseHTTPRequestHandler):
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

        def do_GET(self) -> None:  # noqa: N802
            name = os.path.basename(self.path.split("?")[0])
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
            seg_data = b"".join(buf.parts[p] for p in cur)
            seg_idx = buf.add_segment(seg_data, cur)
            if debug_dir is not None:
                (debug_dir / f"seg{seg_idx}.m4s").write_bytes(seg_data)
            log.debug("segment %d ready (%dB)", seg_idx, len(seg_data))
            cur = []


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def lan_ip() -> str:
    """Best-effort LAN IP (the receiver must be able to reach it)."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 80))
        return s.getsockname()[0]
    finally:
        s.close()


def resolve_codec(requested: str, log: logging.Logger) -> str:
    """Map a user codec choice to a wf-recorder/ffmpeg encoder name."""
    if requested == "auto":
        # R3: prefer HEVC when the receiver advertises it; H.264 when unknown.
        # Per-device negotiation is not wired yet, so default to H.264.
        log.info("codec auto -> h264_vaapi (device codecs unknown; TODO negotiate)")
        return "h264_vaapi"
    return CODEC_MAP.get(requested, "h264_vaapi")


def _select_cast(casts, device_uuid: str, host: str):
    for cast in casts:
        info = cast.cast_info
        if device_uuid and getattr(info, "uuid", None) == device_uuid:
            return cast
        if host and info.host == host:
            return cast
    return None


def _teardown(cap, ff, srv, mc, run_dir: Path, log: logging.Logger) -> None:
    if mc is not None:
        try:
            mc.stop()
        except Exception as exc:  # noqa: BLE001
            log.debug("media_controller.stop: %s", exc)
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
        except Exception:  # noqa: BLE001
            pass
    shutil.rmtree(run_dir, ignore_errors=True)


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------
def cmd_discovery(args: argparse.Namespace) -> int:
    # TODO(M1): pychromecast/zeroconf discovery, avahi-browse fallback,
    # manual host entry. Shape: [{uuid,name,host,port,model,online,codecs}].
    emit({"ok": True, "type": "discovery", "devices": []})
    return 0


def cmd_stream(args: argparse.Namespace) -> int:
    log = setup_logging(args.debug)
    log.info(
        "stream start device=%s host=%s output=%s codec=%s fps=%s bitrate=%s port=%s",
        args.device, args.host, args.output, args.codec, args.fps, args.bitrate, args.port,
    )

    if not args.device and not args.host:
        emit({"type": "phase", "phase": "error", "detail": "one of --device or --host is required"})
        return 1

    try:
        import pychromecast
    except ImportError as exc:
        log.error("pychromecast unavailable: %s", exc)
        emit({"type": "phase", "phase": "error", "detail": f"pychromecast not available: {exc}"})
        return 1

    emit({"type": "phase", "phase": "starting", "detail": "preparing pipeline"})

    codec = resolve_codec(args.codec, log)
    gop = args.gop or max(1, args.fps // 2)

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

    # Local HTTP server bound to the LAN interface (not loopback-only).
    handler = make_handler(buf, log)
    try:
        srv = http.server.ThreadingHTTPServer(("0.0.0.0", args.port), handler)
    except OSError as exc:
        log.error("cannot bind port %s: %s", args.port, exc)
        emit({"type": "phase", "phase": "error", "detail": f"cannot bind port {args.port}: {exc}"})
        return 1
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    ip = args.host or lan_ip()
    url = f"http://{ip}:{args.port}/stream.m3u8"
    log.info("serving %s", url)

    # Capture -> encode. wf-recorder encodes; ffmpeg remuxes to fragmented MP4.
    # The FIFO is a pipe (not a buffer); segments themselves stay in RAM.
    run_dir = Path(tempfile.mkdtemp(prefix="omascreencast-"))
    fifo = run_dir / "capture.ts"
    os.mkfifo(fifo)

    cap = ff = None
    mc = None
    try:
        cap = subprocess.Popen(
            [
                "wf-recorder", "-D", "-r", str(args.fps), "-o", args.output,
                "-c", codec, "-d", args.render_node,
                "-p", f"g={gop}", "-p", f"b={args.bitrate}", "-b", "0",
                "-m", "mpegts", "-f", str(fifo),
            ],
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

        # Wait for the init segment and a few parts before casting.
        for _ in range(40):
            if buf.ready():
                break
            time.sleep(0.25)
        if not buf.ready():
            log.error("pipeline produced no segments in time")
            emit({"type": "phase", "phase": "error", "detail": "no segments produced"})
            return 1

        emit({"type": "phase", "phase": "discovering", "detail": "locating receiver"})
        casts, _browser = pychromecast.get_chromecasts(timeout=10)
        cast = _select_cast(casts, args.device, args.host)
        if cast is None:
            log.error("receiver not found device=%s host=%s", args.device, args.host)
            emit({"type": "phase", "phase": "error", "detail": "receiver not found"})
            return 1

        cast.wait()
        mc = cast.media_controller
        mc.play_media(url, HLS_MIME)
        try:
            mc.block_until_active(timeout=20)
        except Exception as exc:  # noqa: BLE001
            log.warning("block_until_active: %s", exc)

        _write_runtime({
            "pid": os.getpid(),
            "device": args.device,
            "host": ip,
            "url": url,
            "port": args.port,
            "codec": codec,
            "startedAt": _utc_now(),
        })
        emit({"type": "phase", "phase": "casting", "url": url, "device": args.device})
        log.info("casting %s", url)

        stop = threading.Event()

        def _on_signal(signum, _frame):
            log.info("signal %s received", signum)
            stop.set()

        signal.signal(signal.SIGINT, _on_signal)
        signal.signal(signal.SIGTERM, _on_signal)

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
        _teardown(cap, ff, srv, mc, run_dir, log)
        _clear_runtime()
        log.info("stream stopped; buffer stats=%s", buf.stats())
        emit({"type": "phase", "phase": "idle", "detail": "stopped"})
    return 0


def cmd_stop(args: argparse.Namespace) -> int:
    log = setup_logging(False)
    runtime = _read_runtime()
    if not runtime or not runtime.get("pid"):
        emit({"type": "phase", "phase": "idle", "detail": "no active session"})
        return 0

    pid = runtime["pid"]
    log.info("stopping pid %s (device=%s)", pid, runtime.get("device"))
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        log.info("pid %s already gone", pid)
    except OSError as exc:
        log.error("cannot signal pid %s: %s", pid, exc)
        emit({"type": "phase", "phase": "error", "detail": f"cannot stop pid {pid}: {exc}"})
        return 1

    for _ in range(20):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.25)

    _clear_runtime()
    emit({"type": "phase", "phase": "idle", "detail": "stopped"})
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("discovery", help="discover Chromecast devices")

    stream = sub.add_parser("stream", help="capture a virtual output and cast it")
    stream.add_argument("--device", default="", help="receiver uuid")
    stream.add_argument("--host", default="", help="receiver IP (alternative to --device)")
    stream.add_argument("--output", default="HEADLESS-1", help="HEADLESS-* output name")
    stream.add_argument("--res", default="1920x1080@60", help="WxH@Hz (used when creating the output, M2)")
    stream.add_argument("--codec", default="auto", choices=("auto", "hevc", "h264", "vp9", "av1"))
    stream.add_argument("--fps", type=int, default=30)
    stream.add_argument("--bitrate", default="6M", help="encoder bitrate, e.g. 6M")
    stream.add_argument("--render-node", default=DEFAULT_RENDER_NODE, help="VAAPI render node")
    stream.add_argument("--port", type=int, default=DEFAULT_PORT, help="local HTTP port")
    stream.add_argument("--gop", type=int, default=0, help="encoder GOP in frames (0 = fps/2)")
    stream.add_argument("--part-dur", type=float, default=PART_DUR, help="seconds per partial segment")
    stream.add_argument("--seg-parts", type=int, default=SEG_PARTS, help="parts per full segment")
    stream.add_argument("--list-parts", type=int, default=LIST_PARTS, help="parts kept in the ring buffer")
    stream.add_argument("--keep-segments", type=int, default=KEEP_SEGMENTS, help="full segments kept")
    stream.add_argument("--no-ll", action="store_true", help="plain fMP4 HLS (no LL-HLS tags)")
    stream.add_argument("--duration", type=float, default=0.0, help="auto-stop after N seconds (0 = until signalled)")
    stream.add_argument("--debug", action="store_true", help="verbose logging + dump segments to ~/.config/omascreencast/debug")

    stop = sub.add_parser("stop", help="stop the active cast")
    stop.add_argument("--device", default="", help="receiver uuid (default: active session)")

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {"discovery": cmd_discovery, "stream": cmd_stream, "stop": cmd_stop}
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
