"""Smoke tests for the omascreencast helper scripts.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import http.client
import http.server
import importlib.util
import logging
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class SessionStateTest(unittest.TestCase):
    def test_load_default_shape(self):
        session = _load("session")
        state = session.load()
        self.assertIn("active", state)


class DisplayConfigTest(unittest.TestCase):
    def test_read_missing_file_is_empty(self):
        display = _load("display_config")
        self.assertEqual(display.read_config(Path("/nonexistent/monitors.lua")), "")


class BootstrapTest(unittest.TestCase):
    def test_check_returns_keys(self):
        bootstrap = _load("bootstrap")
        status = bootstrap.check()
        for key in ("ok", "missingBinaries", "missingPython", "venv", "venvExists"):
            self.assertIn(key, status)


class HlsBufferTest(unittest.TestCase):
    def _buffer(self, **kwargs):
        cast = _load("cast_engine")
        defaults = dict(part_dur=0.5, seg_parts=2, list_parts=4, keep_segments=2)
        defaults.update(kwargs)
        return cast, cast.HlsBuffer(**defaults)

    def test_playlist_and_lookup(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        p0 = buf.add_part(b"P0")
        p1 = buf.add_part(b"P1")
        self.assertEqual((p0, p1), (0, 1))
        self.assertEqual(buf.add_segment(b"P0P1", [p0, p1]), 0)

        body, ctype = buf.get("stream.m3u8")
        self.assertIn(b"#EXTM3U", body)
        self.assertIn(b"seg0.m4s", body)
        self.assertEqual(ctype, "application/vnd.apple.mpegurl")
        self.assertEqual(buf.get("init.mp4")[0], b"INIT")
        self.assertEqual(buf.get("part0.m4s")[0], b"P0")
        self.assertEqual(buf.get("seg0.m4s")[0], b"P0P1")
        self.assertIsNone(buf.get("missing.m4s"))

    def test_buffer_is_bounded(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        for i in range(50):
            buf.add_part(bytes([i % 256]))
        # The live edge is bounded to list_parts even with no segments formed.
        self.assertLessEqual(len(buf.parts), 4)

    def test_eviction_keeps_referenced_parts(self):
        _cast, buf = self._buffer(list_parts=2)
        buf.set_init(b"INIT")
        for i in range(12):
            part = buf.add_part(bytes([i]))
            if i % 2 == 1:
                buf.add_segment(bytes([i]), [i - 1, i])
        # Parts referenced by kept segments must still be served.
        for _seg_idx, part_idxs in buf.seg_order:
            for part in part_idxs:
                self.assertIsNotNone(buf.get(f"part{part}.m4s"))


class HlsHandlerTest(unittest.TestCase):
    def test_serves_playlist_with_cors(self):
        cast = _load("cast_engine")
        buf = cast.HlsBuffer(part_dur=0.5, seg_parts=2, list_parts=4, keep_segments=2)
        buf.set_init(b"INIT")
        p0 = buf.add_part(b"P0")
        p1 = buf.add_part(b"P1")
        buf.add_segment(b"P0P1", [p0, p1])

        handler = cast.make_handler(buf, logging.getLogger("test"))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            conn.request("GET", "/stream.m3u8")
            resp = conn.getresponse()
            body = resp.read()
            self.assertEqual(resp.status, 200)
            self.assertEqual(resp.getheader("Access-Control-Allow-Origin"), "*")
            self.assertEqual(resp.getheader("Content-Type"), "application/vnd.apple.mpegurl")
            self.assertIn(b"#EXTM3U", body)

            conn.request("GET", "/missing.m4s")
            self.assertEqual(conn.getresponse().status, 404)
            conn.close()
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
