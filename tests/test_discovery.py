"""Unit tests for discovery + capability parsing (M1).

These exercise the pure parsing helpers in ``scripts/cast_engine.py`` against
fixtures, so no network or live receiver is required.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"
FIXTURES = Path(__file__).resolve().parent / "fixtures"


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def _fixture(name: str) -> str:
    return (FIXTURES / name).read_text(encoding="utf-8")


class TxtParsingTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load("cast_engine")

    def test_avahi_quoted_form(self):
        props = self.cast.parse_txt('"id=abc" "md=Chromecast" "fn=Living Room"')
        self.assertEqual(props["id"], "abc")
        self.assertEqual(props["md"], "Chromecast")
        self.assertEqual(props["fn"], "Living Room")

    def test_zeroconf_nul_form(self):
        props = self.cast.parse_txt("id=abc\x00md=Chromecast")
        self.assertEqual(props, {"id": "abc", "md": "Chromecast"})

    def test_empty(self):
        self.assertEqual(self.cast.parse_txt(""), {})


class AvahiParsingTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load("cast_engine")

    def test_parses_fixture(self):
        devices = self.cast.parse_avahi(_fixture("avahi_googlecast.txt"))
        self.assertEqual(len(devices), 2)
        by_name = {d.name: d for d in devices}
        bravia = by_name["BRAVIA VU2"]
        self.assertEqual(bravia.uuid, "d76350b215881b463b6cf307697864a6")
        self.assertEqual(bravia.host, "192.168.100.80")
        self.assertEqual(bravia.port, 8009)
        self.assertEqual(bravia.model, "BRAVIA VU2")
        self.assertTrue(bravia.online)
        self.assertEqual(bravia.codecs, ["h264"])

    def test_ignores_unresolved_and_garbage(self):
        self.assertEqual(self.cast.parse_avahi("+;wlan0;IPv4;X;_googlecast._tcp;local"), [])
        self.assertEqual(self.cast.parse_avahi("not a record"), [])
        self.assertEqual(self.cast.parse_avahi(""), [])


class CodecCapabilityTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load("cast_engine")

    def test_eureka_fixture_prefers_hevc(self):
        info = json.loads(_fixture("eureka_info.json"))
        self.assertEqual(self.cast.codecs_from_eureka(info), ["hevc", "h264", "vp9"])

    def test_unknown_defaults_to_h264(self):
        self.assertEqual(self.cast.codecs_from_eureka({}), ["h264"])
        self.assertEqual(self.cast.codecs_from_eureka({"detail": {}}), ["h264"])
        self.assertEqual(self.cast.codecs_from_eureka(None), ["h264"])

    def test_avc_alias_maps_to_h264(self):
        info = {"supported_codecs": ["avc1.640028"]}
        self.assertEqual(self.cast.codecs_from_eureka(info), ["h264"])


class DeviceShapeTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load("cast_engine")

    def test_as_dict_keys(self):
        dev = self.cast.manual_device("192.168.1.50")
        d = dev.as_dict()
        for key in ("uuid", "name", "host", "port", "model", "online", "codecs"):
            self.assertIn(key, d)
        self.assertEqual(d["host"], "192.168.1.50")
        self.assertEqual(d["name"], "192.168.1.50")
        self.assertEqual(d["codecs"], ["h264"])

    def test_clean_name(self):
        self.assertEqual(
            self.cast._clean_name("Living Room._googlecast._tcp.local."),
            "Living Room",
        )

    def test_first_ipv4_skips_ipv6(self):
        self.assertEqual(
            self.cast._first_ipv4(["fe80::1", "192.168.1.5"]),
            "192.168.1.5",
        )
        self.assertEqual(self.cast._first_ipv4(["fe80::1"]), "")


class DiscoverOnceTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load("cast_engine")

    def test_manual_host_short_circuits(self):
        import logging

        devices = self.cast.discover_once(0.1, logging.getLogger("test"), host="10.0.0.9")
        self.assertEqual(len(devices), 1)
        self.assertEqual(devices[0].host, "10.0.0.9")

    def test_cached_codecs_are_applied(self):
        dev = self.cast.manual_device("10.0.0.9")
        cache = {"10.0.0.9": {"codecs": ["hevc", "h264"]}}
        self.cast._apply_cached_codecs([dev], cache)
        self.assertEqual(dev.codecs, ["hevc", "h264"])


class DiscoveryEventShapeTest(unittest.TestCase):
    """The JSONL contract the QML SplitParser consumes."""

    def setUp(self):
        self.cast = _load("cast_engine")

    def test_fixture_lines_are_valid_json(self):
        lines = [ln for ln in _fixture("discovery.jsonl").splitlines() if ln.strip()]
        self.assertGreaterEqual(len(lines), 3)
        for line in lines:
            msg = json.loads(line)
            self.assertIn("type", msg)
        snapshots = [json.loads(ln) for ln in lines if json.loads(ln)["type"] == "discovery"]
        self.assertEqual(len(snapshots[-1]["devices"]), 2)


JS_HARNESS = r"""
const fs = require("fs");
let src = fs.readFileSync(process.argv[1], "utf8").replace(/^\.pragma library\s*$/m, "");
const mod = { exports: {} };
const run = new Function("module", "exports", "console", src +
  ";module.exports={parseMonitors,parseDevices,parseDiscoveryEvent,parseDoctor," +
  "deviceLabel,deviceCodecLabel,monitorLabel,parseSession,sessionTargetLabel," +
  "sessionMatchesDevice,monitorSessionLabel,sessionPhaseLabel};");
run(mod, mod.exports, { warn: function() {}, log: console.log });
const M = mod.exports;

const monitors = M.parseMonitors(fs.readFileSync(process.argv[2], "utf8"));
const ev = M.parseDiscoveryEvent(JSON.stringify({ type: "discovery", devices: [
  { uuid: "u", name: "BRAVIA", host: "1.2.3.4", port: 8009, model: "m", online: true, codecs: ["hevc", "h264"] }
]}));
const doc = M.parseDoctor(JSON.stringify({ ok: true, streamPort: 7236, firewallRule: "ufw" }));
const sess = M.parseSession(JSON.stringify({ ok: true, active: {
  name: "HEADLESS-1", deviceUuid: "u", deviceName: "", deviceHost: "1.2.3.4",
  phase: "casting", pid: 42, res: "1920x1080@60", position: "1920x0"
}}));
const bogus = M.parseSession("not json");
process.stdout.write(JSON.stringify({
  monitorCount: monitors.length,
  virtual: monitors.length > 1 ? monitors[1].virtual : false,
  monitorLabel: monitors.length > 1 ? M.monitorLabel(monitors[1]) : "",
  eventType: ev.type,
  deviceCount: ev.devices.length,
  deviceLabel: ev.devices.length ? M.deviceLabel(ev.devices[0]) : "",
  codecLabel: ev.devices.length ? M.deviceCodecLabel(ev.devices[0]) : "",
  doctorOk: doc.ok,
  doctorPort: doc.streamPort,
  badJsonSafe: M.parseDevices("not json").length === 0,
  sessionName: sess.active ? sess.active.name : "",
  sessionPhase: M.sessionPhaseLabel(sess.active),
  sessionTarget: M.sessionTargetLabel(sess.active, ev.devices),
  sessionMatches: M.sessionMatchesDevice(sess.active, ev.devices[0]),
  monitorSuffix: monitors.length > 1
    ? M.monitorSessionLabel(monitors[1], sess.active, ev.devices) : "",
  bogusSessionSafe: bogus.active === null && bogus.ok === false
}));
"""


@unittest.skipUnless(shutil.which("node"), "node not installed")
class ModelJsTest(unittest.TestCase):
    """Exercise Model.js helpers through node, using the same fixtures."""

    def test_helpers(self):
        result = subprocess.run(
            ["node", "-e", JS_HARNESS, "Model.js", str(FIXTURES / "monitors.json")],
            cwd=str(ROOT), capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = json.loads(result.stdout)
        self.assertEqual(out["monitorCount"], 2)
        self.assertTrue(out["virtual"])
        self.assertEqual(out["monitorLabel"], "HEADLESS-1 · 1920×1080")
        self.assertEqual(out["eventType"], "discovery")
        self.assertEqual(out["deviceCount"], 1)
        self.assertEqual(out["deviceLabel"], "BRAVIA · 1.2.3.4")
        self.assertEqual(out["codecLabel"], "HEVC · H.264")
        self.assertTrue(out["doctorOk"])
        self.assertEqual(out["doctorPort"], 7236)
        self.assertTrue(out["badJsonSafe"])
        self.assertEqual(out["sessionName"], "HEADLESS-1")
        self.assertEqual(out["sessionPhase"], "CASTING")
        self.assertEqual(out["sessionTarget"], "BRAVIA")
        self.assertTrue(out["sessionMatches"])
        self.assertEqual(out["monitorSuffix"], " → BRAVIA")
        self.assertTrue(out["bogusSessionSafe"])


if __name__ == "__main__":
    unittest.main()
