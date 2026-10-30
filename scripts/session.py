#!/usr/bin/env python3
"""omascreencast session state and virtual-monitor lifecycle (M2).

This module is the **single source of truth** for the active cast session. The
QML service, ``cast_engine.py`` and the CLI all read/write the same file:

    ~/.local/state/omascreencast/session.json      (atomic writes)

The legacy ``~/.config/omascreencast/runtime.json`` is no longer written; it is
removed the next time state is saved so ``stop``, the panel and reconciliation
can never disagree (split-brain fix).

Lifecycle
---------
``create``   choose a free ``HEADLESS-*`` name, ask Hyprland for a headless
             output, place it adjacent (right of the primary display) with the
             requested mode/scale, then launch ``cast_engine.py stream`` and
             record ``{name, deviceUuid, deviceName, pid, url, ...}``.
``remove``   signal the stream process group, remove the output, clear state.
             Idempotent: removing with no active session succeeds.
``reconcile`` compared recorded state against reality: stale sessions whose
             output or process is gone are cleaned automatically; outputs in
             our owned registry (or still targeted by our stream process) are
             removed too. A genuinely foreign ``HEADLESS-*`` output is only
             *reported* so a user-created output is never destroyed behind
             their back.
``keep``      mark a receiver/network drop as ``disconnected`` while keeping the
             virtual output (M6 preference).
``teardown``  idempotently stop every owned stream and remove every owned
             output; used before plugin removal and exposed over IPC.

Everything here is userspace: ``hyprctl`` and a child Python process only.
Runtime monitor changes use ``hyprctl eval 'hl.monitor({...})'`` because this
Hyprland build rejects ``hyprctl keyword monitor`` (Spike I).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

# ---------------------------------------------------------------------------
# Paths and constants
# ---------------------------------------------------------------------------
STATE_VERSION = 1
STATE_DIR = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "omascreencast"
STATE_FILE = STATE_DIR / "session.json"
CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "omascreencast"
LEGACY_RUNTIME_FILE = CONFIG_DIR / "runtime.json"
DEVICE_CACHE = STATE_DIR / "devices.json"
STREAM_LOG = STATE_DIR / "stream.log"

SCRIPT_DIR = Path(__file__).resolve().parent
CAST_ENGINE = SCRIPT_DIR / "cast_engine.py"

HEADLESS_PREFIX = "HEADLESS-"
PREFERRED_MODE = "1920x1080@60"  # the only mode a Hyprland headless output offers
DEFAULT_RES = PREFERRED_MODE
DEFAULT_PORT = 7236
DEFAULT_FPS = 30
DEFAULT_BITRATE = "6M"
DEFAULT_CODEC = "auto"
STREAM_STOP_TIMEOUT = 6.0
CREATE_HEALTH_DELAY = 0.75
OUTPUT_WAIT_TIMEOUT = 6.0
OWNER = "tech.alatcerdas.omascreencast"

_NAME_RE = re.compile(r"^[A-Za-z0-9._-]+$")
_MODE_RE = re.compile(r"^\d{2,5}x\d{2,5}@\d{2,3}(?:\.\d+)?$")
_POSITION_RE = re.compile(r"^-?\d+x-?\d+$")


class SessionError(RuntimeError):
    """A lifecycle step failed with a user-actionable message."""


# ---------------------------------------------------------------------------
# State file (single source of truth; atomic writes)
# ---------------------------------------------------------------------------
def empty_state() -> dict:
    return {"version": STATE_VERSION, "active": None, "ownedOutputs": []}


def owned_outputs(state: dict | None = None) -> list[str]:
    """Outputs this plugin created and may therefore clean up later.

    An untracked ``HEADLESS-*`` output that is not in this registry is only ever
    reported, never removed: it may belong to the user (M6 guard rail).
    """
    source = load() if state is None else state
    names = source.get("ownedOutputs")
    if not isinstance(names, list):
        return []
    out: list[str] = []
    for name in names:
        if not isinstance(name, str):
            continue
        if not name.startswith(HEADLESS_PREFIX) or not _NAME_RE.match(name):
            continue
        if name not in out:
            out.append(name)
    return out


def _now() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load() -> dict:
    """Return the session state; a missing/corrupt file is an empty state."""
    if not STATE_FILE.exists():
        return empty_state()
    try:
        data = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return empty_state()
    if not isinstance(data, dict):
        return empty_state()
    data.setdefault("version", STATE_VERSION)
    data.setdefault("active", None)
    data.setdefault("ownedOutputs", [])
    return data


@contextmanager
def _state_lock():
    """Advisory exclusive lock shared by the CLI, QML and the engine."""
    try:
        import fcntl
    except ImportError:  # pragma: no cover - Linux-only project
        yield
        return
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    with open(str(STATE_FILE) + ".lock", "a", encoding="utf-8") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX)
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def _save_unlocked(state: dict) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    state.setdefault("version", STATE_VERSION)
    state.setdefault("active", None)
    state["ownedOutputs"] = owned_outputs(state)
    tmp = STATE_FILE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    tmp.replace(STATE_FILE)
    # Unify: the old split-brain source is retired as soon as we write state.
    try:
        LEGACY_RUNTIME_FILE.unlink()
    except OSError:
        pass


def save(state: dict) -> None:
    with _state_lock():
        _save_unlocked(state)


def update_active(fields: dict) -> dict | None:
    """Merge fields into the active session, creating one if the engine runs alone."""
    with _state_lock():
        state = load()
        active = state.get("active")
        if not isinstance(active, dict):
            active = {}
        active.update(fields)
        active["updatedAt"] = _now()
        state["active"] = active
        state["version"] = STATE_VERSION
        _save_unlocked(state)
    return active


def mark_stopped(phase: str = "stopped") -> None:
    """Mark the session terminated but keep its name so the output can be removed.

    Called by ``cast_engine.py`` on teardown. ``phase="disconnected"`` records a
    receiver/network drop so the service can keep or remove the output per the
    user's preference (M6). The output is intentionally left in place:
    ``session.py remove`` or reconciliation owns output removal.
    """
    state = load()
    active = state.get("active")
    if not isinstance(active, dict):
        return
    active.update({"phase": str(phase or "stopped"), "pid": None, "url": "",
                   "stoppedAt": _now()})
    state["active"] = active
    save(state)


def clear_active(remove_owned: str = "") -> None:
    """Clear the active session while preserving the output registry."""
    state = load()
    state["active"] = None
    state["version"] = STATE_VERSION
    owned = owned_outputs(state)
    if remove_owned:
        owned = [name for name in owned if name != remove_owned]
    state["ownedOutputs"] = owned
    save(state)


# ---------------------------------------------------------------------------
# Process helpers
# ---------------------------------------------------------------------------
def pid_alive(pid) -> bool:
    """True when ``pid`` exists and is signalable by this user."""
    try:
        value = int(pid)
    except (TypeError, ValueError):
        return False
    if value <= 0:
        return False
    try:
        os.kill(value, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def _signal_tree(pid: int, sig: int) -> bool:
    """Signal the stream's process group (it is spawned in a new session)."""
    try:
        pgid = os.getpgid(pid)
    except OSError:
        pgid = None
    try:
        if pgid is not None:
            os.killpg(pgid, sig)
        else:
            os.kill(pid, sig)
        return True
    except ProcessLookupError:
        return False
    except OSError:
        try:
            os.kill(pid, sig)
            return True
        except OSError:
            return False


def stop_process(pid, timeout: float = STREAM_STOP_TIMEOUT) -> bool:
    """Stop a stream process tree gracefully, then forcefully; True when gone."""
    try:
        value = int(pid)
    except (TypeError, ValueError):
        return True
    if value <= 0 or not pid_alive(value):
        return True

    _signal_tree(value, signal.SIGTERM)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not pid_alive(value):
            return True
        time.sleep(0.15)

    _signal_tree(value, signal.SIGKILL)
    for _ in range(20):
        if not pid_alive(value):
            return True
        time.sleep(0.1)
    return not pid_alive(value)


def spawn_stream(command: list[str]) -> int:
    """Start the cast engine detached; return its pid."""
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    with open(STREAM_LOG, "ab") as log:
        proc = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            cwd=str(SCRIPT_DIR),
        )
    return proc.pid


def stream_command(
    name: str,
    *,
    device: str = "",
    host: str = "",
    res: str = DEFAULT_RES,
    fps: int = DEFAULT_FPS,
    bitrate: str = DEFAULT_BITRATE,
    port: int = DEFAULT_PORT,
    codec: str = DEFAULT_CODEC,
) -> list[str]:
    """Build the ``cast_engine.py stream`` argv for the session output."""
    command = [
        sys.executable, str(CAST_ENGINE), "stream",
        "--output", name,
        "--res", res,
        "--fps", str(fps),
        "--bitrate", str(bitrate),
        "--port", str(port),
        "--codec", str(codec),
    ]
    if device:
        command += ["--device", device]
    if host:
        command += ["--host", host]
    return command


def _tail(path: Path, limit: int = 400) -> str:
    try:
        data = path.read_bytes()[-limit:]
        return data.decode("utf-8", "replace").strip()
    except OSError:
        return ""


def stream_log_offset() -> int:
    """Byte offset of the current end of the stream log (per-run error tail)."""
    try:
        return STREAM_LOG.stat().st_size
    except OSError:
        return 0


def _tail_since(path: Path, offset: int, limit: int = 2000) -> str:
    """Read only what this run appended, so old errors are never reported."""
    try:
        with open(path, "rb") as handle:
            handle.seek(max(0, int(offset)))
            data = handle.read(limit)
        return data.decode("utf-8", "replace").strip()
    except OSError:
        return ""


def _read_appended(path: Path, offset: int) -> tuple[str, int]:
    """Read everything appended since ``offset``; reset after a log truncation."""
    try:
        size = path.stat().st_size
    except OSError:
        return "", max(0, int(offset))
    start = max(0, int(offset))
    if size < start:
        start = 0
    if size == start:
        return "", start
    try:
        with open(path, "rb") as handle:
            handle.seek(start)
            data = handle.read()
    except OSError:
        return "", start
    return data.decode("utf-8", "replace"), start + len(data)


# ---------------------------------------------------------------------------
# hyprctl helpers
# ---------------------------------------------------------------------------
def _run(command: list[str], timeout: float = 5.0) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except FileNotFoundError as exc:
        raise SessionError(
            "`hyprctl` was not found — is this an Omarchy/Hyprland session?"
        ) from exc
    except subprocess.SubprocessError as exc:
        raise SessionError(f"`{command[0]}` failed: {exc}") from exc


def _stderr(result: subprocess.CompletedProcess) -> str:
    return (result.stderr or result.stdout or "").strip()


def list_monitors() -> list[dict]:
    """Return ``hyprctl monitors all -j`` as a list of dicts."""
    result = _run(["hyprctl", "monitors", "all", "-j"])
    if result.returncode != 0:
        raise SessionError(
            "Hyprland did not report monitors "
            f"(`hyprctl monitors` exit {result.returncode}): {_stderr(result)}"
        )
    try:
        data = json.loads(result.stdout or "[]")
    except json.JSONDecodeError as exc:
        raise SessionError("could not parse the Hyprland monitor list") from exc
    if not isinstance(data, list):
        raise SessionError("could not parse the Hyprland monitor list")
    return [m for m in data if isinstance(m, dict)]


def _monitor_names(monitors: list[dict]) -> set[str]:
    return {str(m.get("name", "")) for m in monitors}


def find_monitor(name: str, monitors: list[dict]) -> dict | None:
    for monitor in monitors:
        if str(monitor.get("name", "")) == name:
            return monitor
    return None


def next_headless_name(monitors: list[dict], prefix: str = HEADLESS_PREFIX) -> str:
    """Pick the first unused ``HEADLESS-<n>`` name (deterministic teardown/reconcile)."""
    used = _monitor_names(monitors)
    index = 1
    while f"{prefix}{index}" in used:
        index += 1
    return f"{prefix}{index}"


def logical_size(monitor: dict) -> tuple[int, int]:
    """Return the monitor size in layout (logical) pixels."""
    try:
        scale = float(monitor.get("scale") or 1) or 1.0
    except (TypeError, ValueError):
        scale = 1.0
    width = int(round((int(monitor.get("width") or 0)) / scale))
    height = int(round((int(monitor.get("height") or 0)) / scale))
    return width, height


def primary_monitor(monitors: list[dict]) -> dict | None:
    """The focused display, else the first enabled physical one."""
    for monitor in monitors:
        if monitor.get("focused") is True:
            return monitor
    for monitor in monitors:
        name = str(monitor.get("name", ""))
        if not name.startswith(HEADLESS_PREFIX) and monitor.get("disabled") is not True:
            return monitor
    for monitor in monitors:
        if monitor.get("disabled") is not True:
            return monitor
    return monitors[0] if monitors else None


def position_right_of(primary: dict | None) -> str:
    """Place a new output immediately right of ``primary`` (adjacency = cursor).

    Spike I: the pointer can only enter an adjacent virtual output, so this is the
    correct default arrangement rather than Hyprland's diagonal auto-placement.
    """
    if not primary:
        return "0x0"
    width, _height = logical_size(primary)
    x = int(primary.get("x") or 0) + width
    y = int(primary.get("y") or 0)
    return f"{x}x{y}"


def _fmt_number(value: float) -> str:
    return str(int(value)) if float(value).is_integer() else str(value)


def lua_monitor_rule(name: str, mode: str, position: str, scale: float) -> str:
    """Render a literal ``hl.monitor({...})`` rule (never arbitrary Lua)."""
    return (
        'hl.monitor({ output="%s", mode="%s", position="%s", scale=%s })'
        % (name, mode, position, _fmt_number(scale))
    )


def apply_arrangement(name: str, mode: str, position: str, scale: float = 1.0) -> None:
    """Apply a runtime arrangement via ``hyprctl eval`` (Spike I).

    ``hyprctl keyword monitor`` is rejected by this build's Lua parser; ``eval``
    is the supported form.
    """
    if not _NAME_RE.match(name):
        raise SessionError(f"invalid output name: {name!r}")
    if not _MODE_RE.match(mode):
        raise SessionError(f"invalid mode: {mode!r} (expected WxH@Hz)")
    if not _POSITION_RE.match(position):
        raise SessionError(f"invalid position: {position!r} (expected XxY)")
    try:
        scale_value = float(scale)
    except (TypeError, ValueError) as exc:
        raise SessionError(f"invalid scale: {scale!r}") from exc
    if scale_value <= 0:
        raise SessionError(f"invalid scale: {scale!r}")

    result = _run(["hyprctl", "eval", lua_monitor_rule(name, mode, position, scale_value)])
    message = _stderr(result)
    lowered = message.lower()
    if result.returncode != 0 or "error" in lowered or "invalid" in lowered:
        raise SessionError(f"could not arrange {name}: {message or 'unknown error'}")


def create_headless_output(name: str | None = None) -> tuple[str, list[dict]]:
    """Create a headless output and return its final name plus the monitor list.

    A custom name is preferred (Spike A), with a fallback to Hyprland's
    auto-generated ``HEADLESS-N`` when a build rejects the argument.
    """
    before = _monitor_names(list_monitors())
    result = _run(["hyprctl", "output", "create", "headless"] + ([name] if name else []))
    ok = result.returncode == 0 and "ok" in (result.stdout or "").lower()
    if not ok and name:
        result = _run(["hyprctl", "output", "create", "headless"])
        ok = result.returncode == 0 and "ok" in (result.stdout or "").lower()
    if not ok:
        detail = _stderr(result) or "no headless backend reported"
        raise SessionError(f"could not create a virtual output: {detail}")

    monitors = wait_for_new_output(before)
    created = _monitor_names(monitors) - before
    if not created:
        raise SessionError(
            "Hyprland accepted the command but reported no new output — "
            "the headless backend may be unavailable."
        )
    # Prefer the requested name when it actually appeared.
    if name and name in created:
        return name, monitors
    return sorted(created)[0], monitors


def wait_for_new_output(before: set[str], timeout: float = OUTPUT_WAIT_TIMEOUT) -> list[dict]:
    """Poll the monitor list until an output not in ``before`` appears."""
    deadline = time.monotonic() + timeout
    monitors = list_monitors()
    while time.monotonic() < deadline:
        if _monitor_names(monitors) - before:
            return monitors
        time.sleep(0.1)
        monitors = list_monitors()
    return monitors


def remove_output(name: str) -> None:
    """Remove a virtual output; a missing output is not an error (idempotent)."""
    if not name:
        return
    result = _run(["hyprctl", "output", "remove", name])
    if result.returncode == 0:
        return
    message = _stderr(result).lower()
    if "not found" in message or "no such" in message or "doesn't exist" in message:
        return
    raise SessionError(f"could not remove {name}: {_stderr(result)}")


# ---------------------------------------------------------------------------
# Device metadata (cached by discovery in M1)
# ---------------------------------------------------------------------------
def _read_json(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (json.JSONDecodeError, OSError):
        return {}


def device_info(uuid: str = "", host: str = "") -> dict:
    """Look up a receiver's cached name/model/host (best effort, never fatal)."""
    cache = _read_json(DEVICE_CACHE)
    for key in (uuid, host):
        entry = cache.get(key) if key else None
        if isinstance(entry, dict):
            return {
                "name": str(entry.get("name") or ""),
                "model": str(entry.get("model") or ""),
                "host": str(entry.get("host") or host or ""),
                "codecs": list(entry.get("codecs") or []),
            }
    return {"name": "", "model": "", "host": host, "codecs": []}


# ---------------------------------------------------------------------------
# Reconciliation
# ---------------------------------------------------------------------------
def _cleanup_active(active: dict) -> None:
    pid = active.get("pid")
    if pid:
        stop_process(pid)
    name = str(active.get("name") or "")
    # Only remove an output we recorded as ours. A legacy record without the
    # marker is still treated as ours; anything else is left alone.
    owned = active.get("ownedBy") in (OWNER, None)
    if name and owned:
        try:
            remove_output(name)
        except SessionError:
            pass


def reconcile(
    monitors: list[dict] | None = None,
    alive=pid_alive,
    stop=stop_process,
    remove=remove_output,
    streams=None,
) -> dict:
    """Converge recorded state with reality (M6 startup reconciliation).

    Stale sessions are cleaned automatically. Orphaned ``HEADLESS-*`` outputs
    we recorded as ours are removed too, and so are outputs still targeted by
    one of our own ``cast_engine.py stream`` processes (a recorder left behind
    by a crash before state was written). A genuinely foreign headless output
    is only reported in ``orphans`` — never removed behind the user's back.

    All collaborators are injectable so the suite runs without Hyprland.
    """
    monitors = list_monitors() if monitors is None else monitors
    names = _monitor_names(monitors)
    streams = find_stream_processes if streams is None else streams
    state = load()
    active = state.get("active")
    owned = owned_outputs(state)
    result = {
        "ok": True,
        "cleaned": False,
        "reasons": [],
        "active": None,
        "orphans": [],
        "cleanedOrphans": [],
    }

    if isinstance(active, dict) and active:
        reasons: list[str] = []
        name = str(active.get("name") or "")
        pid = active.get("pid")
        phase = str(active.get("phase") or "")
        if name and name not in names:
            reasons.append("output missing")
        if pid and not alive(pid):
            reasons.append("stream not running")
        if phase == "stopped" and not pid:
            reasons.append("cast stopped")

        if reasons:
            removed = True
            if pid and alive(pid):
                stop(pid)
            if name and name in names:
                try:
                    remove(name)
                except SessionError:
                    removed = False
            if name and removed:
                owned = [item for item in owned if item != name]
            state["active"] = None
            result["cleaned"] = True
            result["reasons"] = reasons
        else:
            result["active"] = active

    keep = str((result["active"] or {}).get("name") or "")
    # Forget names whose output no longer exists; otherwise a later user-created
    # output that happens to reuse the name could be mistaken for ours.
    owned = [name for name in owned if name in names or name == keep]
    orphans: list[str] = []
    cleaned_orphans: list[str] = []
    for name in sorted(names):
        if not name.startswith(HEADLESS_PREFIX) or name == keep:
            continue
        try:
            pids = list(streams(name) or [])
        except (OSError, SessionError):
            pids = []
        if name not in owned and not pids:
            # Not provably ours: report it for an explicit user action.
            orphans.append(name)
            continue
        for pid in pids:
            try:
                stop(pid)
            except (OSError, SessionError):
                pass
        try:
            remove(name)
        except SessionError:
            orphans.append(name)
            continue
        cleaned_orphans.append(name)
        owned = [item for item in owned if item != name]

    state["ownedOutputs"] = owned
    if result["cleaned"] or cleaned_orphans or state.get("active") is None:
        save(state)
    result["orphans"] = orphans
    result["cleanedOrphans"] = cleaned_orphans
    return result


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------
def _emit(payload: dict) -> None:
    print(json.dumps(payload, separators=(",", ":")), flush=True)


def _validate_choice(args: argparse.Namespace) -> None:
    if not args.device and not args.host:
        raise SessionError("one of --device or --host is required")
    if args.name and not _NAME_RE.match(args.name):
        raise SessionError(f"invalid output name: {args.name!r}")
    if not _MODE_RE.match(args.res):
        raise SessionError(f"invalid --res {args.res!r} (expected WxH@Hz)")
    if args.position and not _POSITION_RE.match(args.position):
        raise SessionError(f"invalid --position {args.position!r} (expected XxY)")
    try:
        scale = float(args.scale)
    except (TypeError, ValueError) as exc:
        raise SessionError(f"invalid --scale {args.scale!r}") from exc
    if scale <= 0:
        raise SessionError(f"invalid --scale {args.scale!r}")


def _create(args: argparse.Namespace) -> int:
    _validate_choice(args)

    state = load()
    existing = state.get("active")
    if isinstance(existing, dict) and existing:
        running = bool(existing.get("pid")) and pid_alive(existing.get("pid"))
        if running and existing.get("phase") != "stopped":
            name = existing.get("name") or "a virtual output"
            raise SessionError(f"a session is already active ({name}); run 'remove' first")
        # Stale leftover (crashed shell, stopped cast): clean it before reusing.
        stale_name = str(existing.get("name") or "")
        _cleanup_active(existing)
        clear_active(remove_owned=stale_name)

    monitors = list_monitors()
    name = args.name or next_headless_name(monitors)
    position = args.position or position_right_of(primary_monitor(monitors))
    mode = args.res
    info = device_info(args.device, args.host)
    device_host = args.host or info.get("host") or ""

    created_name = ""
    pid = 0
    try:
        created_name, _monitors_after = create_headless_output(name)
        try:
            apply_arrangement(created_name, mode, position, args.scale)
        except SessionError:
            if mode == PREFERRED_MODE:
                raise
            # The headless backend only offers one mode (Spike A); retry with it.
            mode = PREFERRED_MODE
            apply_arrangement(created_name, mode, position, args.scale)

        monitor = find_monitor(created_name, list_monitors()) or {}
        # The engine's JSONL phase events go to STREAM_LOG (its stdout). Record
        # the log position now so `events` can follow only this run's output.
        log_offset = stream_log_offset()
        active = {
            "name": created_name,
            "ownedBy": OWNER,
            "deviceUuid": args.device,
            "deviceName": args.device_name or info.get("name") or "",
            "deviceHost": device_host,
            "res": mode,
            "position": position,
            "scale": float(args.scale),
            "width": int(monitor.get("width") or 0),
            "height": int(monitor.get("height") or 0),
            "pid": None,
            "url": "",
            "port": args.port,
            "codec": args.codec,
            "phase": "starting",
            "startedAt": _now(),
            "logOffset": log_offset,
        }
        owned = owned_outputs()
        if created_name not in owned:
            owned.append(created_name)
        save({"version": STATE_VERSION, "active": active, "ownedOutputs": owned})

        command = stream_command(
            created_name,
            device=args.device,
            host=device_host,
            res=mode,
            fps=args.fps,
            bitrate=args.bitrate,
            port=args.port,
            codec=args.codec,
        )
        pid = spawn_stream(command)
        if not pid:
            raise SessionError("could not start the cast engine")
        active = update_active({"pid": pid}) or active

        # A missing pychromecast / bad port fails within this short window.
        time.sleep(CREATE_HEALTH_DELAY)
        if not pid_alive(pid):
            tail = _tail_since(STREAM_LOG, log_offset)
            raise SessionError(
                "the cast engine exited immediately" + (f": {tail}" if tail else "")
            )

        _emit({"ok": True, "active": active})
        return 0
    except SessionError:
        if pid:
            stop_process(pid, timeout=3.0)
        if created_name:
            try:
                remove_output(created_name)
            except SessionError:
                pass
        clear_active(remove_owned=created_name)
        raise


def cmd_create(args: argparse.Namespace) -> int:
    try:
        return _create(args)
    except SessionError as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1


def cmd_remove(args: argparse.Namespace) -> int:
    state = load()
    active = state.get("active")
    if not isinstance(active, dict) or not active:
        _emit({"ok": True, "active": None, "removed": False})
        return 0

    name = str(active.get("name") or "")
    pid = active.get("pid")
    warnings: list[str] = []
    removed = True

    if pid and not stop_process(pid):
        warnings.append(f"stream pid {pid} did not exit")

    if name:
        try:
            remove_output(name)
        except SessionError as exc:
            warnings.append(str(exc))
            removed = False

    # Keep the name in the owned registry when removal failed so the next
    # reconciliation retries it instead of leaking the output forever.
    clear_active(remove_owned=name if removed else "")
    payload: dict = {"ok": True, "active": None, "removed": True, "name": name}
    if warnings:
        payload["warnings"] = warnings
    _emit(payload)
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    state = load()
    active = state.get("active")
    alive = False
    output_present = None
    if isinstance(active, dict) and active:
        alive = bool(active.get("pid")) and pid_alive(active.get("pid"))
        name = str(active.get("name") or "")
        if name:
            try:
                output_present = name in _monitor_names(list_monitors())
            except SessionError:
                output_present = None
    _emit({
        "ok": True,
        "active": active,
        "alive": alive,
        "outputPresent": output_present,
    })
    return 0


def cmd_reconcile(args: argparse.Namespace) -> int:
    try:
        _emit(reconcile())
        return 0
    except SessionError as exc:
        _emit({"ok": False, "error": str(exc), "cleaned": False, "orphans": []})
        return 1


def cmd_keep(args: argparse.Namespace) -> int:
    """Keep a virtual output after a receiver/network drop (M6 preference)."""
    active = load().get("active")
    if not isinstance(active, dict) or not active:
        _emit({"ok": True, "active": None, "kept": False})
        return 0
    mark_stopped("disconnected")
    _emit({"ok": True, "active": load().get("active"), "kept": True})
    return 0


# ---------------------------------------------------------------------------
# Live phase stream (M5) and hotplug rearrangement
# ---------------------------------------------------------------------------
TERMINAL_PHASES = {"idle", "error", "stopped", "disconnected"}


def cmd_events(args: argparse.Namespace) -> int:
    """Follow the engine's JSONL log and re-emit its phase events on stdout.

    The QML service keeps this process alive while a cast is active. State is
    driven by the engine's own ``type: phase`` events instead of the panel
    polling the session file. The process exits on a terminal phase, when the
    engine dies, or when the session is torn down.
    """
    interval = max(0.05, float(getattr(args, "interval", 0.25)))
    active = load().get("active")
    if not isinstance(active, dict) or not active:
        _emit({"type": "phase", "phase": "idle", "detail": "no active session"})
        return 0
    try:
        offset = int(active.get("logOffset") or 0)
    except (TypeError, ValueError):
        offset = 0
    pid = active.get("pid")
    buffer = ""
    while True:
        chunk, offset = _read_appended(STREAM_LOG, offset)
        buffer += chunk
        while "\n" in buffer:
            line, buffer = buffer.split("\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(message, dict) or message.get("type") != "phase":
                continue
            phase = str(message.get("phase") or "")
            if phase == "":
                continue
            _emit(message)
            if phase in TERMINAL_PHASES:
                return 0

        current = load().get("active")
        if not isinstance(current, dict) or not current:
            _emit({"type": "phase", "phase": "idle", "detail": "session ended"})
            return 0
        current_pid = current.get("pid")
        if current_pid:
            pid = current_pid
        if pid and not pid_alive(pid):
            _emit({"type": "phase", "phase": "stopped", "detail": "stream exited"})
            return 0
        if str(current.get("phase") or "") == "stopped" and not current.get("pid"):
            _emit({"type": "phase", "phase": "idle", "detail": "stopped"})
            return 0
        time.sleep(interval)


def rearrange_active() -> dict:
    """Re-place the active virtual output immediately right of the primary.

    Called after a physical hotplug so unplugging a monitor never strands or
    overlaps the virtual output. The virtual output itself is never removed.
    """
    state = load()
    active = state.get("active")
    if not isinstance(active, dict) or not active:
        return {"ok": True, "active": None, "rearranged": False}
    name = str(active.get("name") or "")
    monitors = list_monitors()
    if name not in _monitor_names(monitors):
        return {
            "ok": False,
            "error": f"{name or 'the virtual output'} is not present",
            "active": active,
            "rearranged": False,
        }
    others = [m for m in monitors if str(m.get("name", "")) != name]
    position = position_right_of(primary_monitor(others))
    mode = str(active.get("res") or PREFERRED_MODE)
    try:
        scale = float(active.get("scale") or 1)
    except (TypeError, ValueError):
        scale = 1.0
    apply_arrangement(name, mode, position, scale)
    updated = update_active({"position": position}) or active
    return {
        "ok": True,
        "active": updated,
        "rearranged": True,
        "position": position,
    }


def cmd_rearrange(args: argparse.Namespace) -> int:
    try:
        _emit(rearrange_active())
        return 0
    except SessionError as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1


def _argv_output(argv: list[str]) -> str:
    for index, value in enumerate(argv):
        if value == "--output" and index + 1 < len(argv):
            return argv[index + 1]
        if value.startswith("--output="):
            return value.split("=", 1)[1]
    return ""


def iter_stream_processes() -> list[tuple[int, str]]:
    """Return ``(pid, output)`` for every running ``cast_engine.py stream``."""
    found: list[tuple[int, str]] = []
    proc_root = Path("/proc")
    if not proc_root.is_dir():
        return found
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid == os.getpid():
            continue
        try:
            raw = (entry / "cmdline").read_bytes()
        except OSError:
            continue
        argv = [part.decode("utf-8", "replace") for part in raw.split(b"\0") if part]
        if not argv or not any(Path(part).name == "cast_engine.py" for part in argv):
            continue
        if "stream" not in argv:
            continue
        output = _argv_output(argv)
        if output:
            found.append((pid, output))
    return found


def find_stream_processes(output: str) -> list[int]:
    """Find our ``cast_engine.py stream --output <output>`` processes."""
    return [pid for pid, target in iter_stream_processes() if target == output]


def teardown() -> dict:
    """Idempotently stop every owned stream and remove every owned output (M6).

    This is the manual/plugin-disable path: it does not depend on a live
    session record. Outputs are only removed when they are in our owned
    registry or still targeted by one of our stream processes, so a
    user-created ``HEADLESS-*`` output is never destroyed.
    """
    state = load()
    active = state.get("active")
    names = set(owned_outputs(state))
    stopped: list[int] = []

    if isinstance(active, dict) and active:
        name = str(active.get("name") or "")
        if name:
            names.add(name)
        pid = active.get("pid")
        if pid and pid_alive(pid) and stop_process(pid):
            stopped.append(int(pid))

    for pid, output in iter_stream_processes():
        if not output.startswith(HEADLESS_PREFIX):
            continue
        names.add(output)
        if stop_process(pid):
            stopped.append(pid)

    try:
        present = _monitor_names(list_monitors())
    except SessionError:
        present = set()

    removed: list[str] = []
    for name in sorted(names):
        if name not in present:
            continue
        try:
            remove_output(name)
            removed.append(name)
        except SessionError:
            continue

    # Keep the names whose removal failed so the next reconciliation retries.
    remaining = sorted(name for name in names if name in present and name not in removed)
    save({"version": STATE_VERSION, "active": None, "ownedOutputs": remaining})
    return {
        "ok": True,
        "removed": removed,
        "stopped": sorted(set(stopped)),
    }


def cmd_teardown(args: argparse.Namespace) -> int:
    try:
        _emit(teardown())
        return 0
    except (SessionError, OSError) as exc:
        _emit({"ok": False, "error": str(exc), "removed": [], "stopped": []})
        return 1


def cmd_cleanup(args: argparse.Namespace) -> int:
    """Remove a single reported orphan (guarded to our own header prefix)."""
    name = str(args.name or "")
    if not name.startswith(HEADLESS_PREFIX) or not _NAME_RE.match(name):
        _emit({"ok": False, "error": f"refusing to remove {name!r}"})
        return 1
    try:
        monitors = list_monitors()
    except SessionError as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1
    if name not in _monitor_names(monitors):
        _emit({"ok": True, "removed": False, "name": name})
        return 0
    try:
        remove_output(name)
    except SessionError as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1
    # The output is gone, but a lost-state engine may still be running. Stop
    # any stream process that was casting exactly this output.
    stopped: list[int] = []
    for pid in find_stream_processes(name):
        if stop_process(pid):
            stopped.append(pid)
    state = load()
    state["ownedOutputs"] = [item for item in owned_outputs(state) if item != name]
    save(state)
    payload: dict = {"ok": True, "removed": True, "name": name}
    if stopped:
        payload["stopped"] = stopped
    _emit(payload)
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("status", help="print the current session state")
    sub.add_parser("reconcile", help="converge state with Hyprland reality")
    sub.add_parser("rearrange", help="place the active virtual output next to the primary display")
    sub.add_parser("keep", help="keep the virtual output after a receiver/network drop")
    sub.add_parser("teardown", help="idempotently stop every owned stream and remove its output")

    events = sub.add_parser(
        "events", help="follow the active engine's JSONL phase events")
    events.add_argument(
        "--interval", type=float, default=0.25,
        help="poll interval for new log lines (seconds)")

    create = sub.add_parser("create", help="create a virtual monitor and cast it")
    create.add_argument("--device", default="", help="receiver uuid")
    create.add_argument("--host", default="", help="receiver IP (manual / mDNS fallback)")
    create.add_argument("--device-name", default="", help="receiver label for the panel")
    create.add_argument("--name", default="", help="output name (default: next HEADLESS-N)")
    create.add_argument("--res", default=DEFAULT_RES, help="WxH@Hz")
    create.add_argument("--scale", default="1", help="display scale")
    create.add_argument("--position", default="", help="XxY (default: right of primary)")
    create.add_argument("--port", type=int, default=DEFAULT_PORT, help="stream HTTP port")
    create.add_argument("--fps", type=int, default=DEFAULT_FPS)
    create.add_argument("--bitrate", default=DEFAULT_BITRATE, help="encoder bitrate, e.g. 6M")
    create.add_argument("--codec", default=DEFAULT_CODEC,
                        choices=("auto", "hevc", "h264", "vp9", "av1"))

    sub.add_parser("remove", help="tear down the active session (idempotent)")

    cleanup = sub.add_parser("cleanup", help="remove one reported orphaned HEADLESS output")
    cleanup.add_argument("--name", required=True)

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {
        "status": cmd_status,
        "create": cmd_create,
        "remove": cmd_remove,
        "reconcile": cmd_reconcile,
        "rearrange": cmd_rearrange,
        "keep": cmd_keep,
        "teardown": cmd_teardown,
        "events": cmd_events,
        "cleanup": cmd_cleanup,
    }
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
