#!/usr/bin/env python3
"""omascreencast session state.

Tracks the active virtual-monitor/cast session so a shell restart or crash can
reconcile orphaned ``HEADLESS-*`` outputs and recorder/encoder processes.

State file: ``~/.local/state/omascreencast/session.json`` (atomic writes).

Skeleton (M0): state read/write and CLI are in place; create/remove and
reconciliation land in M2/M6.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "omascreencast"
STATE_FILE = STATE_DIR / "session.json"


def load() -> dict:
    if not STATE_FILE.exists():
        return {"active": None}
    try:
        return json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {"active": None}


def save(state: dict) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    tmp = STATE_FILE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    tmp.replace(STATE_FILE)


def cmd_status(args: argparse.Namespace) -> int:
    print(json.dumps(load(), separators=(",", ":")))
    return 0


def cmd_create(args: argparse.Namespace) -> int:
    # TODO(M2): hyprctl output create headless, record {name, deviceUuid, pid, url}.
    print(json.dumps({"ok": False, "error": "create pending M2"}))
    return 1


def cmd_remove(args: argparse.Namespace) -> int:
    # TODO(M2): stop recorder/server, hyprctl output remove, clear state. Idempotent.
    save({"active": None})
    print(json.dumps({"ok": True}))
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("status", help="print the current session state")
    create = sub.add_parser("create", help="create a virtual monitor session")
    create.add_argument("--device", required=True, help="receiver uuid")
    create.add_argument("--res", default="1920x1080@60", help="WxH@Hz")
    sub.add_parser("remove", help="tear down the active session")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {"status": cmd_status, "create": cmd_create, "remove": cmd_remove}
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
