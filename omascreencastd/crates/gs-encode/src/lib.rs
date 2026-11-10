//! Encoder backends behind one trait.
//!
//! **Scaffold only.** The trait and the `ffmpeg` child land in M3-T4, tuning in
//! M3-T5, and in-process FFmpeg is post-0.2.0 (D4).

#![deny(missing_docs)]

/// The encoder backend selected for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Spawn an `ffmpeg` child and read Annex-B from its stdout. Shippable in
    /// 0.2.0 (D4).
    FfmpegChild,
    /// In-process FFmpeg. Post-0.2.0; declared so the switch is a value, not a
    /// rewrite.
    InProcessFfmpeg,
    /// A backend that emits no data, used to prove the protocol without a GPU
    /// (M2-T7 pairs this with the test-pattern capture).
    Null,
}

/// The only codec/chroma/dynamic-range combination 0.2.0 accepts (D14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeProfile {
    /// Backend to use.
    pub backend: Backend,
    /// Target bitrate in kbit/s.
    pub bitrate_kbps: u32,
    /// Group-of-pictures length in frames.
    pub gop_frames: u16,
}

impl EncodeProfile {
    /// The D14 default profile: the `ffmpeg` child at 25 Mbps with a short GOP
    /// for low latency.
    #[must_use]
    pub const fn default_gamestream() -> Self {
        Self {
            backend: Backend::FfmpegChild,
            bitrate_kbps: gs_proto::MAX_BITRATE_KBPS,
            gop_frames: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_matches_the_server_caps() {
        let profile = EncodeProfile::default_gamestream();
        assert_eq!(profile.backend, Backend::FfmpegChild);
        assert_eq!(profile.bitrate_kbps, gs_proto::MAX_BITRATE_KBPS);
        assert!(profile.gop_frames > 0);
    }
}
