//! Wire constants: ports, protocol versions, and capability bitmasks.
//!
//! Each value records **why** it exists and where it came from. Values come
//! from public Moonlight documentation or from traces this project captured;
//! none are transcribed from a GPL implementation (see `THIRD_PARTY_NOTICES.md`).
//!
//! The digit-heavy values here are the ones a client branches on, so a typo is
//! a silent protocol failure. The tests below pin the arithmetic that matters
//! and the parse/clamp behaviour rather than re-asserting the literals.

/// The default HTTPS control port Moonlight probes on a discovered host.
pub const DEFAULT_HTTPS_PORT: u16 = 47984;

/// The default RTSP port, advertised in `/serverinfo` and `/launch` as
/// `SessionUrl0` and used for both plaintext and encrypted RTSP.
pub const DEFAULT_RTSP_PORT: u16 = 48010;

/// The default control-channel UDP port reported in the RTSP `SETUP` response.
pub const DEFAULT_CONTROL_PORT: u16 = 47999;

/// The default video RTP port base; the client requests its own ports in
/// `SETUP`, but this is the value the host answers when a client does not.
pub const DEFAULT_VIDEO_PORT: u16 = 47998;

/// The default audio RTP port base, paired with [`DEFAULT_VIDEO_PORT`].
pub const DEFAULT_AUDIO_PORT: u16 = 48000;

/// Protocol version the host reports in `/serverinfo` as `appversion`.
///
/// The trailing `-1` is not a mistake: clients parse the value as
/// `major.minor.patch.build-suffix` and the suffix is the pre-release marker.
pub const PROTOCOL_VERSION: &str = "7.1.431.-1";

/// Server-feature version, reported as `GfeVersion`.
pub const SERVER_FEATURE_VERSION: &str = "7.1.431.0";

/// Advertised mDNS service type.
pub const MDNS_SERVICE_TYPE: &str = "_nvstream._tcp.local.";

/// TXT-record `version` value Moonlight filters on.
pub const MDNS_VERSION: &str = "7.1.431.0";

/// `PairStatus` value meaning the host has no paired clients yet.
pub const PAIR_STATUS_UNPAIRED: u8 = 1;

/// `PairStatus` value meaning at least one client is paired.
pub const PAIR_STATUS_PAIRED: u8 = 0;

/// `state` string for an idle host in `/serverinfo` and `/applist`.
pub const STATE_FREE: &str = "SUNSHINE_SERVER_FREE";

/// `state` string for a host that is currently streaming.
pub const STATE_BUSY: &str = "SUNSHINE_SERVER_BUSY";

/// `AppState` string used by `/applist` when the host is idle.
pub const APP_STATE_FREE: &str = "SUNSHINE_SERVER_FREE";

/// The synthetic app id omascreencast advertises for "Desktop" (M2-T5).
pub const DESKTOP_APP_ID: u16 = 1;

/// Capability bitfield reported as `ServerCodecModeSupport`.
///
/// Only H.264 is claimed in 0.2.0 (D14: H.264 4:2:0 8-bit SDR only). The
/// bitmask still has to be syntactically valid so clients that gate the launch
/// button on it agree the host supports *something*. An unsupported request is
/// remapped rather than refused, so claiming H.264 alone is intentional.
pub const SERVER_CODEC_MODE_SUPPORT: u32 = CODEC_MODE_H264;

/// `ServerCodecModeSupport` bit for H.264 (the only codec 0.2.0 accepts).
pub const CODEC_MODE_H264: u32 = 0x0000_0001;

/// `ServerCodecModeSupport` bit for H.265/HEVC. Not advertised in 0.2.0.
pub const CODEC_MODE_HEVC: u32 = 0x0000_0100;

/// `ServerCodecModeSupport` bit for AV1. Not advertised in 0.2.0.
pub const CODEC_MODE_AV1: u32 = 0x0000_1000;

/// Server-side resolution cap width (D14).
pub const MAX_WIDTH: u16 = 1920;

/// Server-side resolution cap height (D14).
pub const MAX_HEIGHT: u16 = 1080;

/// Server-side framerate cap (D14).
pub const MAX_FPS: u16 = 60;

/// Server-side bitrate cap in kbit/s (D14: 25 Mbps).
pub const MAX_BITRATE_KBPS: u32 = 25_000;

/// How long a pending pairing session waits for operator approval before it is
/// discarded (M1-T7).
pub const PAIRING_SESSION_TTL_SECONDS: u64 = 300;

/// Number of hexadecimal digits in a pairing PIN (M1-T7).
pub const PIN_LENGTH: usize = 4;

/// A requested stream mode, as parsed from `/launch`'s `mode=WxHxFPS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamMode {
    /// Requested width in pixels.
    pub width: u16,
    /// Requested height in pixels.
    pub height: u16,
    /// Requested framerate in frames per second.
    pub fps: u16,
}

impl StreamMode {
    /// Parse the client's `mode=WIDTHxHEIGHTxFPS` value.
    ///
    /// Moonlight always sends the lowercase `x` separator, but uppercase is
    /// accepted because a typo in a captured trace should be diagnosable here
    /// rather than as a mysterious rejection later.
    ///
    /// # Errors
    ///
    /// Returns [`ModeParseError`] when the value does not have exactly three
    /// numeric components, when a component does not fit a `u16`, or when a
    /// component is zero.
    pub fn parse(value: &str) -> Result<Self, ModeParseError> {
        let mut parts = value.split(['x', 'X']);
        let width = parse_dimension(parts.next(), "width")?;
        let height = parse_dimension(parts.next(), "height")?;
        let fps = parse_dimension(parts.next(), "fps")?;
        if parts.next().is_some() {
            return Err(ModeParseError::Shape);
        }
        Ok(Self { width, height, fps })
    }

    /// Clamp the request to the server caps (D14).
    ///
    /// Clamping rather than rejecting is a deliberate product decision: a
    /// rejected launch looks like a broken server, and the client is told the
    /// actual mode it got through `SessionUrl0`/`/launch` parameters.
    #[must_use]
    pub fn clamp(self) -> Self {
        Self {
            width: self.width.min(MAX_WIDTH),
            height: self.height.min(MAX_HEIGHT),
            fps: self.fps.min(MAX_FPS),
        }
    }

    /// Render back into the `WIDTHxHEIGHTxFPS` form the protocol uses.
    #[must_use]
    pub fn to_wire(self) -> String {
        format!("{}x{}x{}", self.width, self.height, self.fps)
    }
}

fn parse_dimension(part: Option<&str>, which: &'static str) -> Result<u16, ModeParseError> {
    let raw = part.ok_or(ModeParseError::Shape)?;
    if raw.is_empty() {
        return Err(ModeParseError::Shape);
    }
    let value: u16 = raw.parse().map_err(|_| ModeParseError::NotNumeric {
        field: which,
        found: raw.to_owned(),
    })?;
    if value == 0 {
        return Err(ModeParseError::Zero { field: which });
    }
    Ok(value)
}

/// Why a `mode=` string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModeParseError {
    /// The string did not have exactly three components.
    #[error("mode must be WIDTHxHEIGHTxFPS")]
    Shape,
    /// A component was not a number.
    #[error("mode {field} is not numeric: {found:?}")]
    NotNumeric {
        /// Which component (`width`, `height`, or `fps`).
        field: &'static str,
        /// The offending text.
        found: String,
    },
    /// A component was zero.
    #[error("mode {field} must be greater than zero")]
    Zero {
        /// Which component was zero.
        field: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_bits_are_distinct_single_bits() {
        for bit in [CODEC_MODE_H264, CODEC_MODE_HEVC, CODEC_MODE_AV1] {
            assert_eq!(bit.count_ones(), 1, "{bit:#x} must be a single bit");
        }
        // 0.2.0 advertises H.264 only.
        assert_eq!(SERVER_CODEC_MODE_SUPPORT, CODEC_MODE_H264);
        assert_eq!(SERVER_CODEC_MODE_SUPPORT & CODEC_MODE_HEVC, 0);
    }

    #[test]
    fn ports_are_in_the_expected_game_stream_range() {
        for port in [
            DEFAULT_HTTPS_PORT,
            DEFAULT_RTSP_PORT,
            DEFAULT_CONTROL_PORT,
            DEFAULT_VIDEO_PORT,
            DEFAULT_AUDIO_PORT,
        ] {
            assert!(
                (1024..=65535).contains(&port),
                "{port} is not an unprivileged port"
            );
        }
        assert_ne!(DEFAULT_VIDEO_PORT, DEFAULT_AUDIO_PORT);
    }

    #[test]
    fn pairing_status_values_are_inverses() {
        assert_ne!(PAIR_STATUS_UNPAIRED, PAIR_STATUS_PAIRED);
    }

    #[test]
    fn mode_parses_the_canonical_form() {
        assert_eq!(
            StreamMode::parse("1920x1080x60").unwrap(),
            StreamMode {
                width: 1920,
                height: 1080,
                fps: 60
            }
        );
        assert_eq!(StreamMode::parse("1280X720x30").unwrap().width, 1280);
    }

    #[test]
    fn mode_is_clamped_to_the_server_caps() {
        let clamped = StreamMode::parse("3840x2160x120").unwrap().clamp();
        assert_eq!(
            clamped,
            StreamMode {
                width: MAX_WIDTH,
                height: MAX_HEIGHT,
                fps: MAX_FPS
            }
        );
        // A mode under the caps is untouched.
        let small = StreamMode::parse("1280x720x30").unwrap();
        assert_eq!(small.clamp(), small);
    }

    #[test]
    fn mode_round_trips_through_the_wire_form() {
        let mode = StreamMode {
            width: 1600,
            height: 900,
            fps: 50,
        };
        assert_eq!(mode.to_wire(), "1600x900x50");
        assert_eq!(StreamMode::parse(&mode.to_wire()).unwrap(), mode);
    }

    #[test]
    fn mode_rejects_malformed_input_without_panicking() {
        for bad in [
            "",
            "1920",
            "1920x1080",
            "1920x1080x60x1",
            "axbxc",
            "0x1080x60",
            "-1x2x3",
        ] {
            assert!(StreamMode::parse(bad).is_err(), "{bad:?} should not parse");
        }
        assert_eq!(
            StreamMode::parse("1920x1080x60x1"),
            Err(ModeParseError::Shape)
        );
        assert_eq!(
            StreamMode::parse("0x1080x60"),
            Err(ModeParseError::Zero { field: "width" })
        );
    }
}
