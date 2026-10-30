"""Unit tests for the session lifecycle (M2).

Everything here runs against temporary directories and injected fakes, so no
Hyprland session, receiver, or real child process is required.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"


def _load(name: str = "session"):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class _TempPaths(unittest.TestCase):
    """Point every path in the module at a throwaway directory."""

    def setUp(self):
        self.session = _load()
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.session.STATE_DIR = base / "state"
        self.session.STATE_FILE = self.session.STATE_DIR / "session.json"
        self.session.CONFIG_DIR = base / "config"
        self.session.LEGACY_RUNTIME_FILE = self.session.CONFIG_DIR / "runtime.json"
        self.session.DEVICE_CACHE = self.session.STATE_DIR / "devices.json"
        self.session.STREAM_LOG = self.session.STATE_DIR / "stream.log"
        self.addCleanup(self._tmp.cleanup)

    def emit_command(self, func, args) -> dict:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            func(args)
        return json.loads(out.getvalue())


class StateFileTest(_TempPaths):
    def test_missing_state_is_empty(self):
        self.assertEqual(self.session.load(), {"version": 1, "active": None})

    def test_roundtrip_and_update_merge(self):
        self.session.save({"version": 1, "active": {"name": "HEADLESS-1", "phase": "starting"}})
        active = self.session.update_active({"pid": 42, "phase": "casting"})
        loaded = self.session.load()["active"]
        self.assertEqual(loaded["name"], "HEADLESS-1")
        self.assertEqual(loaded["pid"], 42)
        self.assertEqual(loaded["phase"], "casting")
        self.assertIn("updatedAt", loaded)
        self.assertEqual(active["name"], "HEADLESS-1")

    def test_mark_stopped_keeps_name_for_teardown(self):
        self.session.save({"version": 1, "active": {"name": "HEADLESS-2", "pid": 7, "url": "u"}})
        self.session.mark_stopped()
        active = self.session.load()["active"]
        self.assertEqual(active["name"], "HEADLESS-2")
        self.assertEqual(active["phase"], "stopped")
        self.assertIsNone(active["pid"])
        self.assertEqual(active["url"], "")

    def test_clear_active(self):
        self.session.save({"version": 1, "active": {"name": "HEADLESS-1"}})
        self.session.clear_active()
        self.assertIsNone(self.session.load()["active"])

    def test_save_retires_legacy_runtime_file(self):
        self.session.LEGACY_RUNTIME_FILE.parent.mkdir(parents=True, exist_ok=True)
        self.session.LEGACY_RUNTIME_FILE.write_text("{}", encoding="utf-8")
        self.session.save({"version": 1, "active": None})
        self.assertFalse(self.session.LEGACY_RUNTIME_FILE.exists())


class NamingAndGeometryTest(_TempPaths):
    def test_next_headless_name_skips_used(self):
        monitors = [{"name": "eDP-1"}, {"name": "HEADLESS-1"}, {"name": "HEADLESS-2"}]
        self.assertEqual(self.session.next_headless_name(monitors), "HEADLESS-3")

    def test_next_headless_name_when_empty(self):
        self.assertEqual(self.session.next_headless_name([]), "HEADLESS-1")

    def test_logical_size_accounts_for_scale(self):
        monitor = {"width": 3840, "height": 2160, "scale": 2}
        self.assertEqual(self.session.logical_size(monitor), (1920, 1080))

    def test_position_right_of_primary(self):
        primary = {"x": 13, "y": 1080, "width": 1920, "height": 1080, "scale": 1}
        self.assertEqual(self.session.position_right_of(primary), "1933x1080")

    def test_position_right_of_scaled_primary(self):
        primary = {"x": 0, "y": 0, "width": 3840, "height": 2160, "scale": 2}
        self.assertEqual(self.session.position_right_of(primary), "1920x0")

    def test_position_without_primary(self):
        self.assertEqual(self.session.position_right_of(None), "0x0")

    def test_primary_prefers_focused(self):
        monitors = [
            {"name": "eDP-1", "focused": False},
            {"name": "DP-1", "focused": True},
        ]
        self.assertEqual(self.session.primary_monitor(monitors)["name"], "DP-1")

    def test_primary_skips_virtual_when_unfocused(self):
        monitors = [{"name": "HEADLESS-1"}, {"name": "eDP-1"}]
        self.assertEqual(self.session.primary_monitor(monitors)["name"], "eDP-1")

    def test_lua_rule_is_literal_and_formatted(self):
        rule = self.session.lua_monitor_rule("HEADLESS-1", "1920x1080@60", "1920x0", 1.0)
        self.assertEqual(
            rule,
            'hl.monitor({ output="HEADLESS-1", mode="1920x1080@60", position="1920x0", scale=1 })',
        )
        rule = self.session.lua_monitor_rule("HEADLESS-1", "1920x1080@60", "1920x0", 1.5)
        self.assertIn("scale=1.5", rule)

    def test_apply_arrangement_rejects_injection(self):
        with self.assertRaises(self.session.SessionError):
            self.session.apply_arrangement('X", scale=1 }); os.exit()', "1920x1080@60", "0x0")
        with self.assertRaises(self.session.SessionError):
            self.session.apply_arrangement("HEADLESS-1", "not-a-mode", "0x0")
        with self.assertRaises(self.session.SessionError):
            self.session.apply_arrangement("HEADLESS-1", "1920x1080@60", "0x0", 0)


class ApplyArrangementTest(_TempPaths):
    def test_eval_error_message_is_a_failure_even_with_exit_zero(self):
        class Result:
            returncode = 0
            stdout = "error: unknown monitor"
            stderr = ""

        self.session._run = lambda command, timeout=5: Result()
        with self.assertRaises(self.session.SessionError):
            self.session.apply_arrangement("HEADLESS-1", "1920x1080@60", "0x0")


class StatusTest(_TempPaths):
    def test_status_reports_liveness_and_output(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 123, "phase": "casting",
        }})
        self.session.pid_alive = lambda pid: True
        self.session.list_monitors = lambda: [{"name": "HEADLESS-1"}]
        payload = self.emit_command(self.session.cmd_status, argparse.Namespace())
        self.assertTrue(payload["alive"])
        self.assertTrue(payload["outputPresent"])

    def test_status_reports_dead_pid(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 123, "phase": "casting",
        }})
        self.session.pid_alive = lambda pid: False
        self.session.list_monitors = lambda: [{"name": "HEADLESS-1"}]
        payload = self.emit_command(self.session.cmd_status, argparse.Namespace())
        self.assertFalse(payload["alive"])


class CleanupTest(_TempPaths):
    def test_cleanup_stops_the_matching_stream_process(self):
        stopped = []
        self.session.list_monitors = lambda: [{"name": "HEADLESS-7"}]
        self.session.remove_output = lambda name: None
        self.session.find_stream_processes = lambda name: [777]
        self.session.stop_process = lambda pid, timeout=6.0: stopped.append(pid) or True
        payload = self.emit_command(
            self.session.cmd_cleanup, argparse.Namespace(name="HEADLESS-7"))
        self.assertTrue(payload["ok"])
        self.assertEqual(stopped, [777])


class StreamCommandTest(_TempPaths):
    def test_device_command(self):
        cmd = self.session.stream_command(
            "HEADLESS-1", device="uuid-1", host="192.168.1.5",
            res="1920x1080@60", fps=30, bitrate="6M", port=7236,
        )
        self.assertIn("stream", cmd)
        self.assertIn("--output", cmd)
        self.assertEqual(cmd[cmd.index("--output") + 1], "HEADLESS-1")
        self.assertEqual(cmd[cmd.index("--device") + 1], "uuid-1")
        self.assertEqual(cmd[cmd.index("--host") + 1], "192.168.1.5")

    def test_manual_host_command_has_no_device(self):
        cmd = self.session.stream_command("HEADLESS-1", host="10.0.0.9")
        self.assertNotIn("--device", cmd)
        self.assertEqual(cmd[cmd.index("--host") + 1], "10.0.0.9")


class ReconcileTest(_TempPaths):
    def _active(self, **fields):
        active = {"name": "HEADLESS-1", "pid": 123, "phase": "casting"}
        active.update(fields)
        self.session.save({"version": 1, "active": active})

    def test_valid_session_is_kept(self):
        self._active()
        removed, stopped = [], []
        result = self.session.reconcile(
            monitors=[{"name": "eDP-1"}, {"name": "HEADLESS-1"}],
            alive=lambda _pid: True,
            stop=stopped.append,
            remove=removed.append,
        )
        self.assertFalse(result["cleaned"])
        self.assertEqual(result["active"]["name"], "HEADLESS-1")
        self.assertEqual(removed, [])
        self.assertEqual(stopped, [])
        self.assertEqual(result["orphans"], [])

    def test_missing_output_is_cleaned(self):
        self._active(pid=None)
        removed = []
        result = self.session.reconcile(
            monitors=[{"name": "eDP-1"}],
            alive=lambda _pid: True,
            remove=removed.append,
        )
        self.assertTrue(result["cleaned"])
        self.assertIn("output missing", result["reasons"])
        self.assertIsNone(result["active"])
        self.assertIsNone(self.session.load()["active"])
        self.assertEqual(removed, [])

    def test_dead_stream_is_stopped_and_output_removed(self):
        self._active()
        removed, stopped = [], []
        result = self.session.reconcile(
            monitors=[{"name": "HEADLESS-1"}],
            alive=lambda _pid: False,
            stop=stopped.append,
            remove=removed.append,
        )
        self.assertTrue(result["cleaned"])
        self.assertIn("stream not running", result["reasons"])
        self.assertEqual(removed, ["HEADLESS-1"])
        self.assertEqual(stopped, [])  # already dead: nothing to signal

    def test_stopped_session_is_cleaned(self):
        self._active(pid=None, phase="stopped")
        removed = []
        result = self.session.reconcile(
            monitors=[{"name": "HEADLESS-1"}],
            alive=lambda _pid: True,
            remove=removed.append,
        )
        self.assertTrue(result["cleaned"])
        self.assertIn("cast stopped", result["reasons"])
        self.assertEqual(removed, ["HEADLESS-1"])

    def test_orphans_are_reported_not_removed(self):
        result = self.session.reconcile(
            monitors=[{"name": "eDP-1"}, {"name": "HEADLESS-4"}],
            alive=lambda _pid: True,
        )
        self.assertEqual(result["orphans"], ["HEADLESS-4"])
        self.assertFalse(result["cleaned"])

    def test_orphans_exclude_the_active_output(self):
        self._active()
        result = self.session.reconcile(
            monitors=[{"name": "HEADLESS-1"}, {"name": "HEADLESS-2"}],
            alive=lambda _pid: True,
        )
        self.assertEqual(result["orphans"], ["HEADLESS-2"])


class CreateLifecycleTest(_TempPaths):
    def _args(self, **overrides) -> argparse.Namespace:
        values = dict(
            device="uuid-1", host="", device_name="", name="",
            res="1920x1080@60", scale="1", position="", port=7236,
            fps=30, bitrate="6M", codec="auto",
        )
        values.update(overrides)
        return argparse.Namespace(**values)

    def test_create_records_state_and_spawns_stream(self):
        self.session.CREATE_HEALTH_DELAY = 0
        self.session.STATE_DIR.mkdir(parents=True, exist_ok=True)
        self.session.DEVICE_CACHE.write_text(
            json.dumps({"uuid-1": {"name": "Sony Bravia", "host": "192.168.1.5"}}),
            encoding="utf-8",
        )
        spawned = []

        self.session.list_monitors = lambda: [
            {"name": "eDP-1", "focused": True, "x": 0, "y": 0,
             "width": 1920, "height": 1080, "scale": 1},
            {"name": "HEADLESS-1", "width": 1920, "height": 1080},
        ]
        self.session.create_headless_output = lambda name: ("HEADLESS-1", [])
        self.session.apply_arrangement = lambda *a, **k: None
        self.session.spawn_stream = lambda cmd: spawned.append(cmd) or 4242
        self.session.pid_alive = lambda pid: True

        payload = self.emit_command(self.session.cmd_create, self._args())
        self.assertTrue(payload["ok"])
        active = payload["active"]
        self.assertEqual(active["name"], "HEADLESS-1")
        self.assertEqual(active["deviceName"], "Sony Bravia")
        self.assertEqual(active["position"], "1920x0")
        self.assertEqual(active["pid"], 4242)
        self.assertEqual(len(spawned), 1)
        self.assertEqual(self.session.load()["active"]["pid"], 4242)

    def test_create_rolls_back_when_stream_dies(self):
        self.session.CREATE_HEALTH_DELAY = 0
        removed = []

        self.session.list_monitors = lambda: [
            {"name": "eDP-1", "focused": True, "x": 0, "y": 0,
             "width": 1920, "height": 1080, "scale": 1},
            {"name": "HEADLESS-1"},
        ]
        self.session.create_headless_output = lambda name: ("HEADLESS-1", [])
        self.session.apply_arrangement = lambda *a, **k: None
        self.session.spawn_stream = lambda cmd: 5150
        self.session.pid_alive = lambda pid: False
        self.session.remove_output = removed.append
        self.session.stop_process = lambda pid, timeout=0: True

        payload = self.emit_command(self.session.cmd_create, self._args())
        self.assertFalse(payload["ok"])
        self.assertIn("cast engine exited", payload["error"])
        self.assertEqual(removed, ["HEADLESS-1"])
        self.assertIsNone(self.session.load()["active"])

    def test_duplicate_running_session_is_rejected(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 9, "phase": "casting",
        }})
        self.session.pid_alive = lambda pid: True
        payload = self.emit_command(self.session.cmd_create, self._args())
        self.assertFalse(payload["ok"])
        self.assertIn("already active", payload["error"])


class EventsTest(_TempPaths):
    def test_events_emits_only_new_lines_and_stops_on_terminal_phase(self):
        self.session.STATE_DIR.mkdir(parents=True, exist_ok=True)
        prefix = '{"type":"phase","phase":"starting"}\n'
        rest = (
            '{"type":"phase","phase":"casting","url":"http://x/s.m3u8"}\n'
            '{"type":"phase","phase":"idle","detail":"stopped"}\n'
        )
        self.session.STREAM_LOG.write_text(prefix + rest, encoding="utf-8")
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 123, "phase": "casting",
            "logOffset": len(prefix),
        }})
        self.session.pid_alive = lambda pid: True
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.session.cmd_events(argparse.Namespace(interval=0.01))
        messages = [json.loads(line) for line in out.getvalue().splitlines() if line]
        self.assertEqual([m["phase"] for m in messages], ["casting", "idle"])
        self.assertEqual(messages[0]["url"], "http://x/s.m3u8")

    def test_events_without_session_exits_idle(self):
        payload = self.emit_command(
            self.session.cmd_events, argparse.Namespace(interval=0.01))
        self.assertEqual(payload["phase"], "idle")

    def test_events_reports_dead_stream(self):
        self.session.STATE_DIR.mkdir(parents=True, exist_ok=True)
        self.session.STREAM_LOG.write_text("", encoding="utf-8")
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 999, "phase": "starting", "logOffset": 0,
        }})
        self.session.pid_alive = lambda pid: False
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.session.cmd_events(argparse.Namespace(interval=0.01))
        messages = [json.loads(line) for line in out.getvalue().splitlines() if line]
        self.assertEqual(messages[-1]["phase"], "stopped")


class RearrangeTest(_TempPaths):
    def test_rearrange_places_virtual_right_of_primary(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "res": "1920x1080@60", "scale": 1,
            "position": "0x0", "phase": "casting",
        }})
        self.session.list_monitors = lambda: [
            {"name": "eDP-1", "focused": True, "x": 0, "y": 0,
             "width": 1920, "height": 1080, "scale": 1},
            {"name": "HEADLESS-1", "x": 0, "y": 0, "width": 1920, "height": 1080},
        ]
        applied = []
        self.session.apply_arrangement = (
            lambda name, mode, position, scale: applied.append((name, mode, position, scale)))
        result = self.session.rearrange_active()
        self.assertTrue(result["ok"])
        self.assertTrue(result["rearranged"])
        self.assertEqual(applied[0], ("HEADLESS-1", "1920x1080@60", "1920x0", 1.0))
        self.assertEqual(self.session.load()["active"]["position"], "1920x0")

    def test_rearrange_reports_missing_output(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "res": "1920x1080@60", "phase": "casting",
        }})
        self.session.list_monitors = lambda: [{"name": "eDP-1"}]
        result = self.session.rearrange_active()
        self.assertFalse(result["ok"])
        self.assertIn("HEADLESS-1", result["error"])


class RemoveTest(_TempPaths):
    def test_remove_is_idempotent(self):
        payload = self.emit_command(self.session.cmd_remove, argparse.Namespace())
        self.assertTrue(payload["ok"])
        self.assertFalse(payload["removed"])

    def test_remove_stops_stream_and_output(self):
        self.session.save({"version": 1, "active": {
            "name": "HEADLESS-1", "pid": 321, "phase": "casting",
        }})
        stopped, removed = [], []
        self.session.stop_process = lambda pid, timeout=0: stopped.append(pid) or True
        self.session.remove_output = removed.append
        payload = self.emit_command(self.session.cmd_remove, argparse.Namespace())
        self.assertTrue(payload["ok"])
        self.assertTrue(payload["removed"])
        self.assertEqual(stopped, [321])
        self.assertEqual(removed, ["HEADLESS-1"])
        self.assertIsNone(self.session.load()["active"])


if __name__ == "__main__":
    unittest.main()
