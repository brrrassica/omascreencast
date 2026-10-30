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
REQUIRED_BIN = ("hyprctl", "ffmpeg")
# Either capture tool is enough; the engine prefers wl-screenrec when present.
CAPTURE_BIN = ("wl-screenrec", "wf-recorder")
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "omascreencast"
VENV_DIR = STATE_DIR / "venv"

# The local stream port the receiver must be able to reach (R2c). `doctor`
# surfaces the exact firewall rule; it is never applied automatically.
DEFAULT_PORT = 7236
FAST_PATH = "pacman -S python-pychromecast python-zeroconf wf-recorder ffmpeg"


def _has_module(name: str) -> bool:
    try:
        return importlib.util.find_spec(name) is not None
    except (ImportError, ValueError):
        return False


def _capture_backend() -> str:
    for name in CAPTURE_BIN:
        if _which(name):
            return name
    return ""


def check() -> dict:
    """Report dependency status without changing anything."""
    missing_bin = [b for b in REQUIRED_BIN if not _which(b)]
    capture = _capture_backend()
    if not capture:
        missing_bin.append("wl-screenrec or wf-recorder")
    missing_py = [m for m in REQUIRED_PY if not _has_module(m)]
    return {
        "ok": not missing_bin and not missing_py,
        "missingBinaries": missing_bin,
        "missingPython": missing_py,
        "captureBackend": capture,
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


def _venv_python() -> Path:
    return VENV_DIR / "bin" / "python"


def _venv_has_modules(python: Path) -> bool:
    """True when the private venv can import every required Python module."""
    if not python.exists():
        return False
    try:
        result = subprocess.run(
            [str(python), "-c", "import " + ", ".join(REQUIRED_PY)],
            capture_output=True, timeout=30,
        )
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def doctor() -> dict:
    """Full dependency report for the panel's Doctor UI (R4, R9).

    Reports what is missing, which interpreter the engine should use (system
    python, or the private venv when it already has the modules), the exact
    firewall rule for the stream port, and the repo-install fast path. It never
    installs anything and never invokes sudo.

    ``pythonOk`` gates discovery only (mDNS needs pychromecast/zeroconf), while
    ``pipelineOk`` additionally requires the capture/encode binaries. The legacy
    ``ok`` is the conjunction of both for callers that want the whole pipeline.
    """
    missing_bin = [b for b in REQUIRED_BIN if not _which(b)]
    capture = _capture_backend()
    if not capture:
        missing_bin.append("wl-screenrec or wf-recorder")
    missing_py = [m for m in REQUIRED_PY if not _has_module(m)]
    venv_python = _venv_python()
    venv_ready = _venv_has_modules(venv_python)
    python = str(venv_python) if (missing_py and venv_ready) else sys.executable
    python_ok = not missing_py or venv_ready
    pipeline_ok = not missing_bin
    return {
        "ok": python_ok and pipeline_ok,
        "pythonOk": python_ok,
        "pipelineOk": pipeline_ok,
        "missingBinaries": missing_bin,
        "missingPython": missing_py,
        "captureBackend": capture,
        "venv": str(VENV_DIR),
        "venvExists": VENV_DIR.exists(),
        "venvReady": venv_ready,
        "python": python,
        "streamPort": DEFAULT_PORT,
        "fastPath": FAST_PATH,
        "firewallRule": f"sudo ufw allow {DEFAULT_PORT}/tcp",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "provision", "doctor"), help="what to do")
    args = parser.parse_args()
    if args.command == "doctor":
        result = doctor()
    elif args.command == "check":
        result = check()
    else:
        result = provision()
    print(json.dumps(result, separators=(",", ":")))
    if args.command == "provision":
        return 0
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
