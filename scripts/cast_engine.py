#!/usr/bin/env python3
"""omascreencast cast engine.

Discovery, capture -> encode -> serve, and Chromecast playback. Emits one JSON
object per line (JSONL) so the QML side can stream structured phase updates.

Skeleton (M0): command surface and output contracts are defined; the actual
mDNS discovery, ffmpeg pipeline, HTTP server, and pychromecast calls land in
M1 (discovery) and M3 (stream).

Planned phases: discovering -> starting -> casting -> idle/error.
"""

from __future__ import annotations

import argparse
import json
import sys


def emit(payload: dict) -> None:
    print(json.dumps(payload, separators=(",", ":")), flush=True)


def cmd_discovery(args: argparse.Namespace) -> int:
    # TODO(M1): pychromecast/zeroconf discovery, avahi-browse fallback,
    # manual host entry. Shape: [{uuid,name,host,port,model,online,codecs}].
    emit({"ok": True, "type": "discovery", "devices": []})
    return 0


def cmd_stream(args: argparse.Namespace) -> int:
    # TODO(M3): wf-recorder -o <output> -> ffmpeg (hevc_vaapi|h264_vaapi) ->
    # HLS temp dir -> LAN HTTP server -> pychromecast play_media.
    emit({"type": "phase", "phase": "starting", "detail": "not implemented (M3)"})
    emit({"type": "phase", "phase": "error", "detail": "stream pipeline pending M3"})
    return 1


def cmd_stop(args: argparse.Namespace) -> int:
    # TODO(M3): terminate ffmpeg/wf-recorder, close the HTTP server, stop the
    # cast on the device, and clear the session-state entry.
    emit({"type": "phase", "phase": "idle", "detail": "not implemented (M3)"})
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("discovery", help="discover Chromecast devices")

    stream = sub.add_parser("stream", help="create + capture a virtual output and cast it")
    stream.add_argument("--device", required=True, help="receiver uuid")
    stream.add_argument("--output", default="", help="HEADLESS-* output name")
    stream.add_argument("--res", default="1920x1080@60", help="WxH@Hz")
    stream.add_argument("--codec", default="auto", choices=("auto", "hevc", "h264", "vp9", "av1"))
    stream.add_argument("--fps", type=int, default=30)
    stream.add_argument("--bitrate", default="6M")

    stop = sub.add_parser("stop", help="stop the active cast")
    stop.add_argument("--device", default="", help="receiver uuid (default: active session)")

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {"discovery": cmd_discovery, "stream": cmd_stream, "stop": cmd_stop}
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
