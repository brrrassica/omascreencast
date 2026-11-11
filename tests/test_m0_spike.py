"""Tests for the M0 spike tooling (``tools/m0-gamestream/fake_server.py``).

The spike is throwaway tooling, but it decides what the M0 trace corpus
records, so its route table, verbatim logging, and SDP shape are covered here.
No network beyond loopback and no TLS are required.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPIKE = ROOT / "tools" / "m0-gamestream" / "fake_server.py"


def _load_spike():
    spec = importlib.util.spec_from_file_location("m0_fake_server", SPIKE)
    module = importlib.util.module_from_spec(spec)
    sys.modules["m0_fake_server"] = module
    spec.loader.exec_module(module)
    return module


class SpikeServerTest(unittest.TestCase):
    def setUp(self):
        self.spike = _load_spike()
        self.tmpdir = tempfile.TemporaryDirectory()
        self.log_path = Path(self.tmpdir.name) / "capture.jsonl"
        self.config = self.spike.ServerConfig(
            bind="127.0.0.1",
            https_port=0,
            discovery_port=0,
            hostname="spike-test",
            server_uuid="00000000-0000-4000-8000-000000000000",
            pair_status=1,
            log_path=self.log_path,
            local_ip="127.0.0.1",
        )
        # single-port, plain HTTP for deterministic unit tests
        self.httpd = self.spike.make_server(self.config, discovery_only=False)
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=5)
        self.httpd.request_log.close()
        self.tmpdir.cleanup()

    def _get(self, path: str):
        url = f"http://127.0.0.1:{self.port}{path}"
        try:
            with urllib.request.urlopen(url, timeout=5) as response:
                return response.status, response.headers.get_content_type(), response.read()
        except urllib.error.HTTPError as exc:
            try:
                return exc.code, exc.headers.get_content_type(), exc.read()
            finally:
                exc.close()

    def _entries(self):
        with self.log_path.open(encoding="utf-8") as fh:
            return [json.loads(line) for line in fh if line.strip()]

    def _root(self, body: bytes) -> ET.Element:
        return ET.fromstring(body.decode("utf-8"))

    def test_serverinfo_xml_shape(self):
        status, content_type, body = self._get("/serverinfo")
        self.assertEqual(status, 200)
        self.assertEqual(content_type, "application/xml")
        root = self._root(body)
        self.assertEqual(root.get("status_code"), "200")
        self.assertEqual(root.findtext("hostname"), "spike-test")
        self.assertEqual(root.findtext("uniqueid"), "00000000-0000-4000-8000-000000000000")
        self.assertEqual(root.findtext("PairStatus"), "1")
        self.assertIsNotNone(root.findtext("appversion"))
        self.assertIsNotNone(root.findtext("GfeVersion"))
        self.assertIsNotNone(root.findtext("ServerCodecModeSupport"))
        self.assertEqual(root.findtext("LocalIP"), "127.0.0.1")

    def test_applist_has_single_desktop_app(self):
        status, _, body = self._get("/applist")
        self.assertEqual(status, 200)
        root = self._root(body)
        self.assertEqual(root.get("status_code"), "200")
        apps = root.findall("App")
        self.assertEqual(len(apps), 1)
        self.assertEqual(apps[0].findtext("AppTitle"), "Desktop")
        self.assertEqual(apps[0].findtext("ID"), "1")

    def test_launch_returns_session_url_and_records_query(self):
        status, _, body = self._get(
            "/launch?appid=1&mode=1920x1080x60&hdr=0&rikey=abc&rikeyid=7&localAudio=1&surroundAudioInfo=196610"
        )
        self.assertEqual(status, 200)
        root = self._root(body)
        self.assertEqual(root.findtext("Gamesession"), "1")
        self.assertIn("SessionUrl0", [c.tag for c in root])

        entry = self._entries()[-1]
        self.assertEqual(entry["method"], "GET")
        self.assertEqual(entry["path"], "/launch")
        self.assertEqual(entry["query"]["mode"], "1920x1080x60")
        self.assertEqual(entry["query"]["rikey"], "abc")
        self.assertIn("surroundAudioInfo=196610", entry["raw_query"])
        self.assertFalse(entry["tls"])

    def test_unknown_route_is_404_envelope_and_logged(self):
        status, content_type, body = self._get("/totally-unknown")
        self.assertEqual(status, 200)
        self.assertEqual(content_type, "application/xml")
        self.assertEqual(self._root(body).get("status_code"), "404")
        self.assertEqual(self._entries()[-1]["path"], "/totally-unknown")

    def test_sdp_includes_audio_by_default(self):
        _, _, body = self._get("/sdp")
        text = body.decode("utf-8")
        self.assertIn("m=video 0 RTP/AVP 96", text)
        self.assertIn("H264/90000", text)
        self.assertIn("m=audio 0 RTP/AVP 97", text)
        self.assertIn("OPUS/48000/2", text)

    def test_sdp_can_omit_audio_for_m0_t4(self):
        self.config.no_audio = True
        _, _, body = self._get("/sdp")
        text = body.decode("utf-8")
        self.assertIn("m=video 0 RTP/AVP 96", text)
        self.assertNotIn("m=audio", text)

    def test_log_records_request_line_and_headers(self):
        self._get("/serverinfo")
        entry = self._entries()[-1]
        self.assertTrue(entry["request_line"].startswith("GET /serverinfo HTTP/1.1"))
        header_names = {name.lower() for name, _ in entry["headers"]}
        self.assertIn("user-agent", header_names)
        self.assertEqual(entry["client"]["address"], "127.0.0.1")

    def test_no_log_prints_to_stderr(self):
        log = self.spike.RequestLog(None)
        log.record({"path": "/x"})
        log.close()


class DiscoveryPortTest(unittest.TestCase):
    def setUp(self):
        self.spike = _load_spike()
        self.tmpdir = tempfile.TemporaryDirectory()
        self.log_path = Path(self.tmpdir.name) / "capture.jsonl"
        self.config = self.spike.ServerConfig(
            bind="127.0.0.1",
            https_port=0,
            discovery_port=0,
            hostname="spike-test",
            server_uuid="00000000-0000-4000-8000-000000000000",
            log_path=self.log_path,
            local_ip="127.0.0.1",
        )
        self.discovery = self.spike.make_server(self.config, discovery_only=True)
        self.port = self.discovery.server_address[1]
        self.thread = threading.Thread(target=self.discovery.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.discovery.shutdown()
        self.discovery.server_close()
        self.thread.join(timeout=5)
        self.discovery.request_log.close()
        self.tmpdir.cleanup()

    def _get(self, path: str):
        url = f"http://127.0.0.1:{self.port}{path}"
        try:
            with urllib.request.urlopen(url, timeout=5) as response:
                return response.status, response.headers.get_content_type(), response.read()
        except urllib.error.HTTPError as exc:
            try:
                return exc.code, exc.headers.get_content_type(), exc.read()
            finally:
                exc.close()

    def test_discovery_serves_only_serverinfo(self):
        status, _, body = self._get("/serverinfo")
        self.assertEqual(status, 200)
        self.assertEqual(ET.fromstring(body).get("status_code"), "200")
        # every other path is a 404 envelope on the discovery socket
        for path in ("/applist", "/pair", "/launch", "/"):
            _, _, b = self._get(path)
            self.assertEqual(ET.fromstring(b).get("status_code"), "404", path)


class SdpBuilderTest(unittest.TestCase):
    def setUp(self):
        self.spike = _load_spike()

    def test_sdp_is_crlf_framed(self):
        config = self.spike.ServerConfig(local_ip="127.0.0.1")
        sdp = self.spike.build_sdp(config, "127.0.0.1")
        self.assertTrue(sdp.endswith("\r\n"))
        self.assertNotIn("\n\n", sdp.replace("\r\n", ""))
        self.assertIn("sprop-parameter-sets=", sdp)


if __name__ == "__main__":
    unittest.main()
