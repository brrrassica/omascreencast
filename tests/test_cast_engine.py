"""Unit tests for the M3 pipeline helpers (no receiver or capture required).

Covers the performance/correctness work that is testable in isolation:
render-node detection, capture backend selection, codec negotiation, capture
argv construction, LL-HLS blocking reload, and the bounded ring buffer.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import importlib.util
import sys
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"


def _load(name: str = "cast_engine"):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class RenderNodeTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()

    def test_explicit_node_is_kept(self):
        self.assertEqual(self.cast.detect_render_node("/dev/dri/renderD129"),
                         "/dev/dri/renderD129")

    def test_env_override(self):
        with mock.patch.dict("os.environ", {"OMASCREENCAST_RENDER_NODE": "/dev/dri/renderD130"}):
            self.assertEqual(self.cast.detect_render_node("auto"), "/dev/dri/renderD130")

    def test_enumerates_dev_dri(self):
        fake = [Path("/dev/dri/renderD128"), Path("/dev/dri/renderD129")]
        with mock.patch.object(self.cast.Path, "glob", return_value=fake):
            self.assertEqual(self.cast.detect_render_node("auto"), "/dev/dri/renderD128")

    def test_falls_back_when_empty(self):
        with mock.patch.object(self.cast.Path, "glob", return_value=[]):
            self.assertEqual(self.cast.detect_render_node("auto"), "/dev/dri/renderD128")


class CaptureBackendTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()
        self.log = mock.MagicMock()

    def test_auto_prefers_wf_recorder_for_ll_hls(self):
        with mock.patch.object(self.cast.shutil, "which", return_value="/usr/bin/tool"):
            self.assertEqual(self.cast.detect_capture_backend("auto", self.log), "wf-recorder")

    def test_auto_uses_wl_screenrec_when_ll_hls_disabled(self):
        with mock.patch.object(self.cast.shutil, "which", return_value="/usr/bin/tool"):
            self.assertEqual(
                self.cast.detect_capture_backend("auto", self.log, ll_hls=False),
                "wl-screenrec",
            )

    def test_auto_falls_back_to_wf_recorder(self):
        with mock.patch.object(self.cast.shutil, "which",
                               side_effect=lambda n: "/usr/bin/wf-recorder" if n == "wf-recorder" else None):
            self.assertEqual(self.cast.detect_capture_backend("auto", self.log), "wf-recorder")

    def test_explicit_missing_backend_falls_back(self):
        with mock.patch.object(self.cast.shutil, "which", return_value=None):
            self.assertEqual(self.cast.detect_capture_backend("wl-screenrec", self.log), "wf-recorder")

    def test_wf_recorder_command_has_low_latency_flags(self):
        cmd = self.cast.capture_command(
            "wf-recorder", "HEADLESS-1", 30, "h264_vaapi",
            "/dev/dri/renderD128", "6M", 15, Path("/tmp/cap.ts"),
        )
        self.assertEqual(cmd[:2], ["wf-recorder", "-D"])
        self.assertIn("-b", cmd)
        self.assertEqual(cmd[cmd.index("-b") + 1], "0")  # no B-frames
        self.assertIn("g=15", cmd)
        self.assertIn("b=6M", cmd)
        self.assertIn("/dev/dri/renderD128", cmd)

    def test_vp9_uses_matroska_not_mpegts(self):
        cmd = self.cast.capture_command(
            "wf-recorder", "HEADLESS-1", 30, "libvpx-vp9",
            "/dev/dri/renderD128", "6M", 15, Path("/tmp/cap.stream"),
        )
        self.assertIn("matroska", cmd)
        self.assertNotIn("mpegts", cmd)

    def test_h264_keeps_mpegts(self):
        cmd = self.cast.capture_command(
            "wf-recorder", "HEADLESS-1", 30, "h264_vaapi",
            "/dev/dri/renderD128", "6M", 15, Path("/tmp/cap.stream"),
        )
        self.assertIn("mpegts", cmd)

    def test_wl_screenrec_command_only_uses_advertised_flags(self):
        cmd = self.cast.capture_command(
            "wl-screenrec", "HEADLESS-1", 30, "h264_vaapi",
            "/dev/dri/renderD128", "6M", 15, Path("/tmp/cap.ts"),
            help_text="--ffmpeg-encoder --max-fps",
        )
        self.assertEqual(cmd[:2], ["wl-screenrec", "-o"])
        self.assertNotIn("--bitrate", cmd)

        cmd = self.cast.capture_command(
            "wl-screenrec", "HEADLESS-1", 30, "h264_vaapi",
            "/dev/dri/renderD128", "6M", 15, Path("/tmp/cap.ts"),
            help_text="--ffmpeg-encoder --max-fps --bitrate",
        )
        self.assertIn("--bitrate", cmd)
        self.assertEqual(cmd[cmd.index("--bitrate") + 1], "6M")


class CodecNegotiationTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()
        self.log = mock.MagicMock()
        self.cast._ENCODER_CACHE = {"hevc_vaapi", "h264_vaapi", "libvpx-vp9", "av1_vaapi"}

    def test_auto_prefers_hevc_when_advertised(self):
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=["h264", "hevc"]),
            "hevc_vaapi",
        )

    def test_auto_falls_back_to_h264_when_unknown(self):
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=[]),
            "h264_vaapi",
        )

    def test_auto_skips_av1_unless_experimental(self):
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=["av1"]),
            "h264_vaapi",
        )
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=["av1"],
                                    allow_experimental_av1=True),
            "av1_vaapi",
        )

    def test_explicit_choice_is_mapped(self):
        self.assertEqual(self.cast.resolve_codec("vp9", self.log), "libvpx-vp9")

    def test_explicit_unavailable_encoder_falls_back(self):
        self.cast._ENCODER_CACHE = {"h264_vaapi"}
        self.assertEqual(self.cast.resolve_codec("hevc", self.log), "h264_vaapi")

    def test_explicit_av1_requires_experimental_flag(self):
        self.assertEqual(self.cast.resolve_codec("av1", self.log), "h264_vaapi")
        self.assertEqual(
            self.cast.resolve_codec("av1", self.log, allow_experimental_av1=True),
            "av1_vaapi",
        )

    def test_unavailable_encoder_is_skipped(self):
        self.cast._ENCODER_CACHE = {"h264_vaapi"}
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=["hevc", "h264"]),
            "h264_vaapi",
        )


class LanIpTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()

    def test_usable_ipv4_filters_special_addresses(self):
        self.assertTrue(self.cast._usable_ipv4("192.168.1.5"))
        self.assertFalse(self.cast._usable_ipv4("127.0.0.1"))
        self.assertFalse(self.cast._usable_ipv4("169.254.1.1"))
        self.assertFalse(self.cast._usable_ipv4("0.0.0.0"))
        self.assertFalse(self.cast._usable_ipv4("not-an-ip"))

    def test_lan_ip_uses_default_route_interface(self):
        with mock.patch.object(self.cast, "_default_route_iface", return_value="wlan0"), \
             mock.patch.object(self.cast, "_iface_ipv4", return_value="192.168.1.20"):
            self.assertEqual(self.cast.lan_ip(), "192.168.1.20")


class HlsBufferBlockingTest(unittest.TestCase):
    def _buffer(self, **kwargs):
        cast = _load()
        defaults = dict(part_dur=0.5, seg_parts=2, list_parts=4, keep_segments=2)
        defaults.update(kwargs)
        return cast, cast.HlsBuffer(**defaults)

    def test_wait_ready_returns_false_on_timeout(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        self.assertFalse(buf.wait_ready(0.05))

    def test_wait_ready_wakes_on_parts(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")

        def feed():
            time.sleep(0.05)
            buf.add_part(b"P0")
            buf.add_part(b"P1")

        threading.Thread(target=feed, daemon=True).start()
        self.assertTrue(buf.wait_ready(2.0))

    def test_blocking_playlist_waits_for_requested_part(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        buf.add_part(b"P0")
        # Requested msn 0 part 1 is not available yet.

        def feed():
            time.sleep(0.05)
            buf.add_part(b"P1")

        threading.Thread(target=feed, daemon=True).start()
        started = time.monotonic()
        body = buf.wait_playlist(0, 1, timeout=2.0)
        self.assertGreaterEqual(time.monotonic() - started, 0.04)
        self.assertIn(b"part1.m4s", body)

    def test_blocking_playlist_times_out_with_current_playlist(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        buf.add_part(b"P0")
        body = buf.wait_playlist(99, 0, timeout=0.05)
        self.assertIn(b"#EXTM3U", body)

    def test_part_hold_back_matches_validated_spike(self):
        _cast, buf = self._buffer()
        buf.set_init(b"INIT")
        buf.add_part(b"P0")
        body = buf.get("stream.m3u8")[0].decode()
        self.assertIn("PART-HOLD-BACK=1.5", body)

    def test_playlist_window_matches_keep_segments(self):
        cast, buf = self._buffer(seg_parts=1, keep_segments=2)
        buf.set_init(b"INIT")
        for i in range(5):
            p = buf.add_part(bytes([i]))
            buf.add_segment(bytes([i]), [p])
        buffer = buf.get("stream.m3u8")[0].decode()
        self.assertEqual(buffer.count("#EXTINF:"), 2)
        self.assertNotIn("seg0.m4s", buffer)
        self.assertIn("seg4.m4s", buffer)


class CastFromHostTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()

    def test_tries_the_known_signatures(self):
        calls = []

        def factory(*args, **kwargs):
            calls.append((args, kwargs))
            if len(calls) < 2:
                raise TypeError("wrong signature")
            return "cast-object"

        fake = mock.Mock()
        fake.get_chromecast_from_host = factory
        with mock.patch.dict(sys.modules, {"pychromecast": fake}):
            self.assertEqual(self.cast._cast_from_host("10.0.0.9", self.cast.logging.getLogger("t")),
                             "cast-object")
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0][0][0][0], "10.0.0.9")

    def test_missing_factory_is_an_error(self):
        class FakeModule:
            pass

        with mock.patch.dict(sys.modules, {"pychromecast": FakeModule()}):
            with self.assertRaises(RuntimeError):
                self.cast._cast_from_host("10.0.0.9", self.cast.logging.getLogger("t"))


if __name__ == "__main__":
    unittest.main()
