#!/usr/bin/env python3
"""omascreencast display configuration.

Reads and updates Omarchy Quattro's Hyprland monitor rules in
``~/.config/hypr/monitors.lua``, which use the Lua form:

    hl.monitor({ output = "eDP-1", mode = "1920x1080@60", position = "0x0",
                 scale = 1, transform = 0, disabled = false, mirror = "" })

Design rules (M4):
- Literal-only editing. Never evaluate user Lua; only rewrite literal field
  values or append new standalone ``hl.monitor({...})`` calls.
- Always back up the original file before writing, and write atomically.
- Runtime changes are applied separately via ``hyprctl keyword monitor``.

Skeleton (M0): CLI and file handling are in place; the rule parser lands in M4.
"""

from __future__ import annotations

import argparse
import json
import shutil
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_CONFIG = Path.home() / ".config" / "hypr" / "monitors.lua"


def read_config(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.exists() else ""


def backup_config(path: Path) -> str:
    """Copy the current config aside with a UTC timestamp; return the backup path."""
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup = path.with_suffix(path.suffix + f".omascreencast-{stamp}.bak")
    shutil.copy2(path, backup)
    return str(backup)


def cmd_show(args: argparse.Namespace) -> int:
    print(read_config(Path(args.config)))
    return 0


def cmd_backup(args: argparse.Namespace) -> int:
    path = Path(args.config)
    if not path.exists():
        print(json.dumps({"ok": False, "error": f"not found: {path}"}))
        return 1
    print(json.dumps({"ok": True, "backup": backup_config(path)}))
    return 0


def cmd_apply(args: argparse.Namespace) -> int:
    # TODO(M4): parse literal hl.monitor({...}) rules, apply the requested
    # per-output field updates, back up, then write atomically.
    print(json.dumps({"ok": False, "error": "apply pending M4"}))
    return 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=str(DEFAULT_CONFIG), help="path to monitors.lua")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("show", help="print the current monitor config")
    sub.add_parser("backup", help="back up the monitor config")
    apply = sub.add_parser("apply", help="apply arrangement changes")
    apply.add_argument("--rule", required=True, help="JSON rule, e.g. {\"output\":\"HEADLESS-1\",\"position\":\"1920x0\"}")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {"show": cmd_show, "backup": cmd_backup, "apply": cmd_apply}
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
