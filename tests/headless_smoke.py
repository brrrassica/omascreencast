#!/usr/bin/env python3
"""Headless end-to-end smoke test for the cast packager and HTTP server.

No display, Chromecast, or VAAPI is required. It encodes a synthetic source
with ffmpeg into fragmented MP4, parses it with the real `read_box`/`segmenter`
code, serves the resulting LL-HLS playlist over the real handler, and fetches
the playlist, init segment, and a media segment as a receiver would.

Exit code 0 on success; non-zero with a clear message otherwise. Run directly
or through `scripts/smoke.sh`.
"""

from __future__ import annotations

import http.client
import http.server
import importlib.util
import logging
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def fail(message: str) -> int:
    print(f"headless smoke: FAIL: {message}", file=sys.stderr)
    return 1


def main() -> int:
    cast = _load("cast_engine")
    log = logging.getLogger("headless-smoke")
    logging.basicConfig(level=logging.INFO, format="%(message)s")

    buf = cast.HlsBuffer(part_dur=0.5, seg_parts=2, list_parts=6, keep_segments=3)
    ffmpeg = [
        "ffmpeg", "-hide_banner", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc=size=320x180:rate=30",
        "-t", "4",
        "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency",
        "-g", "15", "-keyint_min", "15", "-sc_threshold", "0", "-bf", "0",
        "-pix_fmt", "yuv420p",
        "-movflags", "frag_keyframe+empty_moov+default_base_moof",
        "-f", "mp4", "-",
    ]
    try:
        proc = subprocess.Popen(ffmpeg, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except FileNotFoundError:
        return fail("ffmpeg not found on PATH")

    worker = threading.Thread(target=cast.segmenter, args=(proc, buf, log), daemon=True)
    worker.start()

    deadline = time.monotonic() + 20
    try:
        while time.monotonic() < deadline and not buf.segments:
            time.sleep(0.1)
        if not buf.segments:
            err = ""
            if proc.poll() is not None:
                err = proc.stderr.read().decode("utf-8", "replace").strip()
            return fail(f"no media segment produced within 20s{': ' + err if err else ''}")
        if not buf.init:
            return fail("init segment was never produced")

        handler = cast.make_handler(buf, log)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            conn.request("GET", "/stream.m3u8")
            resp = conn.getresponse()
            playlist = resp.read()
            if resp.status != 200:
                return fail(f"playlist HTTP {resp.status}")
            if resp.getheader("Content-Type") != "application/vnd.apple.mpegurl":
                return fail("playlist has the wrong Content-Type")
            if resp.getheader("Access-Control-Allow-Origin") != "*":
                return fail("playlist is missing the required CORS header")
            for marker in (b"#EXTM3U", b"#EXT-X-PART:", b"seg", b"init.mp4"):
                if marker not in playlist:
                    return fail(f"playlist is missing {marker!r}")

            names = [b"init.mp4"]
            for line in playlist.splitlines():
                if line.startswith(b"seg") and line.endswith(b".m4s"):
                    names.append(line.strip())
                    break
            for name in names:
                conn.request("GET", "/" + name.decode())
                part = conn.getresponse()
                body = part.read()
                if part.status != 200 or not body:
                    return fail(f"{name.decode()} did not serve a body")
                if part.getheader("Content-Type") != "video/mp4":
                    return fail(f"{name.decode()} has the wrong Content-Type")
            conn.request("GET", "/missing.m4s")
            if conn.getresponse().status != 404:
                return fail("unknown path did not return 404")
            conn.close()
        finally:
            server.shutdown()
            server.server_close()
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        worker.join(timeout=5)

    print("headless smoke: PASS (segmenter + CORS LL-HLS HTTP server)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
