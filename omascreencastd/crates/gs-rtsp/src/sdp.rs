//! SDP builder for the GameStream video stream (M2-T3).
//!
//! Moonlight reads the SDP returned by `DESCRIBE` to learn the payload type,
//! codec, clock rate, and the `a=control` stream id it must `SETUP`. This module
//! emits a single H.264 video stream, which is the only codec 0.2.0 accepts
//! (D14).
//!
//! # What is standard and what is ours
//!
//! `v=`, `o=`, `s=`, `t=`, `m=`, `a=rtpmap`, `a=fmtp`, `a=framerate`, and
//! `a=control` are standard SDP. The negotiated mode is echoed in the `i=`
//! session-information line and in a project-specific `a=x-omascreencast-mode`
//! attribute: a client that does not know the attribute ignores it, and a trace
//! replay can still assert the mode the host actually committed to. Nothing here
//! is transcribed from Sunshine or `moonlight-common-c`.

use gs_proto::StreamMode;

/// Payload type for H.264 video in the GameStream SDP.
pub const H264_PAYLOAD_TYPE: u8 = 96;

/// RTP clock rate for H.264, fixed by RFC 6184.
pub const H264_CLOCK_RATE: u32 = 90_000;

/// `profile-level-id` advertised for H.264.
///
/// `42e02a` is constrained-baseline (profile `0x42`, constraint flags `0xe0`)
/// at level 4.2 (`0x2a`), which covers 1080p60. It is a hint: the encoder's own
/// SPS in the bitstream is authoritative, and the client tolerates a mismatch.
pub const H264_PROFILE_LEVEL_ID: &str = "42e02a";

/// The `packetization-mode` advertised for H.264.
///
/// Mode 1 (non-interleaved) is what a low-latency host uses; mode 0 would forbid
/// the fragmentation the RTP packetiser relies on.
pub const H264_PACKETIZATION_MODE: u8 = 1;

/// A single H.264 video stream, ready to render as SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSdp {
    /// Host address used in the `o=` origin line.
    pub host: String,
    /// The negotiated mode, already clamped to the server caps.
    pub mode: StreamMode,
    /// RTP payload type.
    pub payload_type: u8,
    /// `profile-level-id` value.
    pub profile_level_id: String,
    /// `packetization-mode` value.
    pub packetization_mode: u8,
}

impl VideoSdp {
    /// Build a video SDP for `host` at `mode` with the 0.2.0 defaults.
    #[must_use]
    pub fn new(host: impl Into<String>, mode: StreamMode) -> Self {
        Self {
            host: host.into(),
            mode,
            payload_type: H264_PAYLOAD_TYPE,
            profile_level_id: H264_PROFILE_LEVEL_ID.to_owned(),
            packetization_mode: H264_PACKETIZATION_MODE,
        }
    }

    /// Render the SDP document, CRLF-terminated as the wire form requires.
    #[must_use]
    pub fn to_sdp(&self) -> String {
        let payload = self.payload_type;
        let mode = self.mode.to_wire();
        let mut lines = vec![
            String::from("v=0"),
            format!("o=- 0 0 IN IP4 {}", self.host),
            String::from("s=omascreencast"),
            format!("i=GameStream {mode}"),
            String::from("t=0 0"),
            String::from("a=control:*"),
            format!("m=video 0 RTP/AVP {payload}"),
            format!("a=rtpmap:{payload} H264/{H264_CLOCK_RATE}"),
            format!(
                "a=fmtp:{payload} packetization-mode={};profile-level-id={}",
                self.packetization_mode, self.profile_level_id
            ),
            format!("a=framerate:{}", self.mode.fps),
            format!("a=x-omascreencast-mode:{mode}"),
            String::from("a=control:streamid=0"),
        ];
        // SDP lines are CRLF-terminated; a trailing CRLF is part of the document.
        lines.push(String::new());
        lines.join("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode() -> StreamMode {
        StreamMode {
            width: 1920,
            height: 1080,
            fps: 60,
        }
    }

    #[test]
    fn sdp_has_the_lines_moonlight_reads() {
        let sdp = VideoSdp::new("192.168.1.2", mode()).to_sdp();
        for required in [
            "v=0\r\n",
            "m=video 0 RTP/AVP 96\r\n",
            "a=rtpmap:96 H264/90000\r\n",
            "a=control:streamid=0\r\n",
        ] {
            assert!(sdp.contains(required), "missing {required:?} in:\n{sdp}");
        }
    }

    #[test]
    fn sdp_carries_the_negotiated_mode_and_framerate() {
        let sdp = VideoSdp::new("10.0.0.5", mode()).to_sdp();
        assert!(sdp.contains("i=GameStream 1920x1080x60\r\n"));
        assert!(sdp.contains("a=framerate:60\r\n"));
        assert!(sdp.contains("a=x-omascreencast-mode:1920x1080x60\r\n"));
    }

    #[test]
    fn sdp_advertises_packetization_mode_one() {
        let sdp = VideoSdp::new("host", mode()).to_sdp();
        assert!(sdp.contains("packetization-mode=1"));
        assert!(sdp.contains("profile-level-id=42e02a"));
    }

    #[test]
    fn sdp_uses_crlf_line_endings() {
        let sdp = VideoSdp::new("host", mode()).to_sdp();
        // Every LF must be preceded by a CR.
        for (index, byte) in sdp.bytes().enumerate() {
            if byte == b'\n' {
                assert_eq!(sdp.as_bytes()[index - 1], b'\r', "bare LF at {index}");
            }
        }
    }
}
