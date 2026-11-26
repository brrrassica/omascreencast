"""Unit tests for the M3 pipeline helpers (no receiver or capture required).

Covers the performance/correctness work that is testable in isolation:
render-node detection, capture backend selection, codec negotiation, capture
argv construction, LL-HLS blocking reload, and the bounded ring buffer.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import argparse
import contextlib
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

    def test_no_capture_backend_fails_fast(self):
        with mock.patch.object(self.cast.shutil, "which", return_value=None):
            with self.assertRaisesRegex(RuntimeError, "no capture backend"):
                self.cast.detect_capture_backend("auto", self.log)
            with self.assertRaisesRegex(RuntimeError, "no capture backend"):
                self.cast.detect_capture_backend("wl-screenrec", self.log)

    def test_explicit_missing_backend_falls_back_to_installed_tool(self):
        with mock.patch.object(self.cast.shutil, "which",
                               side_effect=lambda n: "/usr/bin/wf-recorder" if n == "wf-recorder" else None):
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

    def test_auto_uses_available_encoder_when_h264_vaapi_is_missing(self):
        # #95: the safe default must still be checked against ffmpeg.
        self.cast._ENCODER_CACHE = {"libvpx-vp9"}
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=[]),
            "libvpx-vp9",
        )

    def test_explicit_unavailable_encoder_uses_another_installed_encoder(self):
        self.cast._ENCODER_CACHE = {"libvpx-vp9"}
        self.assertEqual(self.cast.resolve_codec("hevc", self.log), "libvpx-vp9")

    def test_no_encoders_available_is_reported(self):
        self.cast._ENCODER_CACHE = set()
        self.assertEqual(
            self.cast.resolve_codec("auto", self.log, codecs=[]),
            "h264_vaapi",
        )
        self.log.error.assert_called_once()


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


class ReceiverDropListenerTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()

    class _Status:
        def __init__(self, state):
            self.player_state = state

    def test_idle_after_playing_reports_one_drop(self):
        drops = []
        listener = self.cast._ReceiverDropListener(drops.append)
        listener.new_media_status(self._Status("PLAYING"))
        listener.new_media_status(self._Status("IDLE"))
        listener.new_media_status(self._Status("IDLE"))
        self.assertEqual(drops, ["receiver idle"])

    def test_idle_before_playing_is_not_a_drop(self):
        drops = []
        listener = self.cast._ReceiverDropListener(drops.append)
        listener.new_media_status(self._Status("IDLE"))
        self.assertEqual(drops, [])

    def test_load_failure_is_a_drop(self):
        drops = []
        listener = self.cast._ReceiverDropListener(drops.append)
        listener.load_media_failed(None, None)
        self.assertEqual(drops, ["receiver load failed"])


class ForceTakeoverTest(unittest.TestCase):
    """A native cast app (Spotify) must not swallow the LOAD (Spike M)."""

    def setUp(self):
        self.cast = _load()
        self.log = mock.MagicMock()

    class _FakeCast:
        def __init__(self, app_id, after=None, raises=False):
            self.app_id = app_id
            self._after = after
            self._raises = raises
            self.started = []

        def start_app(self, app_id):
            self.started.append(app_id)
            if self._raises:
                raise RuntimeError("start_app failed")
            if self._after is not None:
                self.app_id = self._after

    def test_already_on_receiver_is_a_noop(self):
        fake = self._FakeCast(self.cast.MEDIA_RECEIVER_APP)
        self.assertTrue(self.cast.force_takeover(fake, self.log))
        self.assertEqual(fake.started, [])

    def test_launches_receiver_and_reports_takeover(self):
        fake = self._FakeCast("AndroidNativeApp", after=self.cast.MEDIA_RECEIVER_APP)
        self.assertTrue(self.cast.force_takeover(fake, self.log))
        self.assertEqual(fake.started, [self.cast.MEDIA_RECEIVER_APP])

    def test_start_app_failure_is_soft(self):
        fake = self._FakeCast("AndroidNativeApp", raises=True)
        self.assertFalse(self.cast.force_takeover(fake, self.log))

    def test_timeout_returns_false(self):
        fake = self._FakeCast("AndroidNativeApp")
        self.assertFalse(self.cast.force_takeover(fake, self.log, timeout=0))

    def test_uses_the_default_media_receiver_app_id(self):
        self.assertEqual(self.cast.MEDIA_RECEIVER_APP, "CC1AD845")


class WaitForPlaybackTest(unittest.TestCase):
    """The LOAD is still IDLE when block_until_active returns (Spike M)."""

    def setUp(self):
        self.cast = _load()

    class _Status:
        def __init__(self, state, reason=None):
            self.player_state = state
            self.idle_reason = reason

    class _FakeMC:
        def __init__(self, statuses):
            self._statuses = list(statuses)
            self._index = 0
            self._last = None

        @property
        def status(self):
            if self._index < len(self._statuses):
                self._last = self._statuses[self._index]
                self._index += 1
            return self._last

    def test_playing_is_success(self):
        mc = self._FakeMC([self._Status("PLAYING")])
        self.assertEqual(self.cast.wait_for_playback(mc, timeout=1), "PLAYING")

    def test_buffering_counts_as_started(self):
        mc = self._FakeMC([self._Status("IDLE"), self._Status("BUFFERING")])
        self.assertEqual(
            self.cast.wait_for_playback(mc, timeout=1, poll=0.01), "BUFFERING"
        )

    def test_idle_error_fails_fast(self):
        mc = self._FakeMC([self._Status("IDLE", "ERROR")])
        self.assertEqual(self.cast.wait_for_playback(mc, timeout=5), "IDLE")

    def test_timeout_returns_last_state(self):
        mc = self._FakeMC([self._Status("IDLE")])
        self.assertEqual(
            self.cast.wait_for_playback(mc, timeout=0.05, poll=0.01), "IDLE"
        )


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


class MainSpawnFailureTest(unittest.TestCase):
    def setUp(self):
        self.cast = _load()

    def test_spawn_failure_emits_error_phase(self):
        emitted = []

        def _explode(args):
            raise FileNotFoundError(2, "No such file or directory: 'wl-screenrec'")

        fake_args = mock.Mock(command="stream")
        with mock.patch.object(self.cast, "parse_args", return_value=fake_args), \
                mock.patch.object(self.cast, "cmd_stream", side_effect=_explode), \
                mock.patch.object(self.cast, "emit", side_effect=emitted.append):
            code = self.cast.main()
        self.assertEqual(code, 1)
        self.assertEqual(len(emitted), 1)
        event = emitted[0]
        self.assertEqual(event["type"], "phase")
        self.assertEqual(event["phase"], "error")
        self.assertIn("stream failed", event["detail"])
        self.assertIn("wl-screenrec", event["detail"])

    def test_success_path_is_not_wrapped(self):
        fake_args = mock.Mock(command="stop")
        with mock.patch.object(self.cast, "parse_args", return_value=fake_args), \
                mock.patch.object(self.cast, "cmd_stop", return_value=0) as stop, \
                mock.patch.object(self.cast, "emit") as emit:
            code = self.cast.main()
        self.assertEqual(code, 0)
        stop.assert_called_once_with(fake_args)
        emit.assert_not_called()


class SegmenterDebugDumpTest(unittest.TestCase):
    """A failed debug write must never kill the segmenter thread (#87)."""

    def setUp(self):
        self.cast = _load()

    def test_oserror_disables_further_dumps(self):
        path = mock.Mock()
        path.name = "part0.m4s"
        path.write_bytes.side_effect = OSError(28, "No space left on device")
        log = mock.Mock()
        state = {"ok": True}

        self.cast._debug_dump(path, b"first", log, state)
        self.assertFalse(state["ok"])
        log.warning.assert_called_once()
        self.cast._debug_dump(path, b"second", log, state)
        path.write_bytes.assert_called_once()


class StreamSetupUnwindTest(unittest.TestCase):
    """Setup failures inside cmd_stream must unwind the server and state (#80)."""

    def setUp(self):
        self.cast = _load()

    def test_mkdtemp_failure_shuts_down_server_and_marks_stopped(self):
        server = mock.Mock()
        emitted = []
        args = argparse.Namespace(
            debug=False, device="device-uuid", host="", output="HEADLESS-1",
            codec="auto", fps=30, bitrate="6M", port=7236,
            render_node="auto", backend="auto", experimental_av1=False,
            gop=0, part_dur=1.0, seg_parts=5, list_parts=10,
            keep_segments=10, no_ll=False, duration=0.0,
        )
        with mock.patch.object(self.cast, "setup_logging", return_value=mock.Mock()), \
                mock.patch.dict(sys.modules, {"pychromecast": mock.Mock()}), \
                mock.patch.object(self.cast.signal, "signal"), \
                mock.patch.object(self.cast.session_state, "update_active"), \
                mock.patch.object(self.cast.session_state, "device_info",
                                  return_value={"host": "192.0.2.10"}), \
                mock.patch.object(self.cast.session_state,
                                  "mark_stopped") as mark_stopped, \
                mock.patch.object(self.cast, "resolve_codec", return_value="h264"), \
                mock.patch.object(self.cast, "detect_render_node", return_value=""), \
                mock.patch.object(self.cast, "detect_capture_backend",
                                  return_value="wf-recorder"), \
                mock.patch.object(self.cast, "lan_ip", return_value="192.0.2.10"), \
                mock.patch.object(self.cast.http.server, "ThreadingHTTPServer",
                                  return_value=server), \
                mock.patch.object(self.cast.threading, "Thread"), \
                mock.patch.object(self.cast.tempfile, "mkdtemp",
                                  side_effect=OSError(28, "No space left on device")), \
                mock.patch.object(self.cast, "emit", side_effect=emitted.append):
            code = self.cast.cmd_stream(args)

        self.assertEqual(code, 1)
        server.shutdown.assert_called_once()
        server.server_close.assert_called_once()
        mark_stopped.assert_called_once_with("error")
        self.assertTrue(any(e.get("phase") == "error" for e in emitted))


class StreamTerminalPhaseTest(unittest.TestCase):
    """#94/#97: error exits must not be reported as a clean stop afterwards."""

    def setUp(self):
        self.cast = _load()

    def _run(self, *, ready=True, playback="PLAYING", select_cast="default", cap_exit=False):
        emitted = []
        server = mock.Mock()
        cap = mock.Mock()
        cap.poll.return_value = 1 if cap_exit else None
        ff = mock.Mock()
        ff.poll.return_value = None
        receiver = mock.Mock()
        args = argparse.Namespace(
            debug=False, device="device-uuid", host="", output="HEADLESS-1",
            codec="auto", fps=30, bitrate="6M", port=7236,
            render_node="auto", backend="auto", experimental_av1=False,
            gop=0, part_dur=1.0, seg_parts=5, list_parts=10,
            keep_segments=10, no_ll=False, duration=0.0,
        )
        buf = mock.Mock()
        buf.wait_ready.return_value = ready
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(self.cast, "setup_logging",
                                                  return_value=mock.Mock()))
            pychromecast = mock.Mock()
            pychromecast.get_chromecasts.return_value = ([], None)
            stack.enter_context(mock.patch.dict(sys.modules, {"pychromecast": pychromecast}))
            stack.enter_context(mock.patch.object(self.cast.signal, "signal"))
            stack.enter_context(mock.patch.object(self.cast.session_state, "update_active"))
            stack.enter_context(mock.patch.object(self.cast.session_state, "device_info",
                                                  return_value={"host": "192.0.2.10"}))
            mark_stopped = stack.enter_context(
                mock.patch.object(self.cast.session_state, "mark_stopped"))
            stack.enter_context(mock.patch.object(self.cast, "resolve_codec",
                                                  return_value="h264"))
            stack.enter_context(mock.patch.object(self.cast, "detect_render_node",
                                                  return_value=""))
            stack.enter_context(mock.patch.object(self.cast, "detect_capture_backend",
                                                  return_value="wf-recorder"))
            stack.enter_context(mock.patch.object(self.cast, "lan_ip",
                                                  return_value="192.0.2.10"))
            stack.enter_context(mock.patch.object(self.cast.http.server,
                                                  "ThreadingHTTPServer", return_value=server))
            stack.enter_context(mock.patch.object(self.cast.threading, "Thread"))
            stack.enter_context(mock.patch.object(self.cast.tempfile, "mkdtemp",
                                                  return_value="/tmp/omascreencast-test"))
            stack.enter_context(mock.patch.object(self.cast.os, "mkfifo"))
            stack.enter_context(mock.patch.object(self.cast.subprocess, "Popen",
                                                  side_effect=[cap, ff]))
            stack.enter_context(mock.patch.object(self.cast, "segmenter"))
            stack.enter_context(mock.patch.object(self.cast, "HlsBuffer", return_value=buf))
            stack.enter_context(mock.patch.object(self.cast, "_select_cast",
                                                  return_value=receiver if select_cast == "default" else select_cast))
            stack.enter_context(mock.patch.object(self.cast, "force_takeover",
                                                  return_value=True))
            stack.enter_context(mock.patch.object(self.cast, "wait_for_playback",
                                                  return_value=playback))
            stack.enter_context(mock.patch.object(self.cast.time, "sleep"))
            stack.enter_context(mock.patch.object(self.cast, "emit", side_effect=emitted.append))
            code = self.cast.cmd_stream(args)
        return code, mark_stopped, emitted

    def _assert_error_exit(self, code, mark_stopped, emitted):
        self.assertEqual(code, 1)
        mark_stopped.assert_called_once_with("error")
        self.assertTrue(any(e.get("phase") == "error" for e in emitted))
        self.assertFalse(any(e.get("phase") == "idle" for e in emitted))

    def test_no_segments_returns_error_terminal_phase(self):
        self._assert_error_exit(*self._run(ready=False))

    def test_receiver_not_found_returns_error_terminal_phase(self):
        self._assert_error_exit(*self._run(select_cast=None))

    def test_playback_failure_returns_error_terminal_phase(self):
        self._assert_error_exit(*self._run(playback="IDLE"))

    def test_pipeline_exit_returns_error_terminal_phase(self):
        self._assert_error_exit(*self._run(cap_exit=True))

    def test_missing_capture_backend_fails_before_binding(self):
        args = argparse.Namespace(
            debug=False, codec="auto", fps=30, render_node="auto", backend="auto",
            experimental_av1=False, device="device-uuid", host="", no_ll=False,
            output="HEADLESS-1", bitrate="6M", port=7236, gop=0,
        )
        emitted = []
        with mock.patch.object(self.cast, "setup_logging", return_value=mock.Mock()), \
                mock.patch.dict(sys.modules, {"pychromecast": mock.Mock()}), \
                mock.patch.object(self.cast.session_state, "device_info",
                                  return_value={}), \
                mock.patch.object(self.cast.session_state, "mark_stopped") as mark_stopped, \
                mock.patch.object(self.cast, "resolve_codec", return_value="h264"), \
                mock.patch.object(self.cast, "detect_render_node", return_value=""), \
                mock.patch.object(self.cast, "detect_capture_backend",
                                  side_effect=RuntimeError("no capture backend")), \
                mock.patch.object(self.cast.http.server, "ThreadingHTTPServer") as server, \
                mock.patch.object(self.cast, "emit", side_effect=emitted.append):
            code = self.cast.cmd_stream(args)
        self.assertEqual(code, 1)
        mark_stopped.assert_called_once_with("error")
        server.assert_not_called()
        self.assertTrue(any(e.get("phase") == "error" for e in emitted))


if __name__ == "__main__":
    unittest.main()
