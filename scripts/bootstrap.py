#!/usr/bin/env python3
"""omascreencast dependency bootstrap (rootless).

Resolves the Python dependencies the cast engine needs without root, matching
how `omarchy plugin add` behaves: the plugin is only cloned, never installed.
Strategy, in order:

1. Use a system-installed `pychromecast` / `zeroconf` if importable.
2. Otherwise create a private virtualenv under
   ``~/.local/share/omascreencast/venv`` and ``pip install --user`` into it.
3. Never run sudo, and never touch system packages.

Skeleton (M0): the resolution logic and CLI are in place; M1 wires it into the
service's `doctor` surface.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

REQUIRED_PY = ("pychromecast", "zeroconf")
REQUIRED_BIN = ("hyprctl", "wf-recorder", "ffmpeg")
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "omascreencast"
VENV_DIR = STATE_DIR / "venv"


def _has_module(name: str) -> bool:
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ValueError):
        return False


def check() -> dict:
    """Report dependency status without changing anything."""
    missing_bin = [b for b in REQUIRED_BIN if not _which(b)]
    missing_py = [m for m in REQUIRED_PY if not _has_module(m)]
    return {
        "ok": not missing_bin and not missing_py,
        "missingBinaries": missing_bin,
        "missingPython": missing_py,
        "venv": str(VENV_DIR),
        "venvExists": VENV_DIR.exists(),
    }


def _which(executable: str) -> str | None:
    from shutil import which

    return which(executable)


def provision() -> dict:
    """Create/refresh the private venv when system packages are unavailable."""
    status = check()
    if not status["missingPython"]:
        status["provisioned"] = False
        status["reason"] = "system packages present"
        return status

    STATE_DIR.mkdir(parents=True, exist_ok=True)
    if not VENV_DIR.exists():
        subprocess.run([sys.executable, "-m", "venv", str(VENV_DIR)], check=True)
    pip = VENV_DIR / "bin" / "pip"
    subprocess.run(
        [str(pip), "install", "--upgrade", "pychromecast", "zeroconf"],
        check=True,
    )
    status["provisioned"] = True
    status["reason"] = "installed into private venv"
    return status


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "provision"), help="what to do")
    args = parser.parse_args()
    result = check() if args.command == "check" else provision()
    print(json.dumps(result, separators=(",", ":")))
    return 0 if result.get("ok") or args.command == "provision" else 1


if __name__ == "__main__":
    raise SystemExit(main())
