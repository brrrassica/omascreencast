//! RTSP message framing, plaintext and `rtspenc`, and the SDP builder.
//!
//! M2-T1 lands the parser and serialiser ([`message`]), M2-T2 the encrypted
//! framing ([`encrypt`]), and M2-T3 the SDP builder ([`sdp`]). The crate is
//! deliberately pure: it performs no I/O, so it can be exercised against
//! captured traces with no sockets and no Wayland.
//!
//! # Clean-room note
//!
//! The message grammar is the published RTSP/1.0 grammar (RFC 2326), which is
//! HTTP/1.1-shaped. The `rtspenc` framing in [`encrypt`] is a **provisional**
//! construction behind a documented interface: the byte-exact key and IV
//! derivation are a capture fact that M0-T5 pins, exactly as the pairing KDF is
//! pinned by the same run. Nothing here is transcribed from Sunshine or
//! `moonlight-common-c`; see `THIRD_PARTY_NOTICES.md`.

#![deny(missing_docs)]

pub mod encrypt;
pub mod message;
pub mod replay;
pub mod sdp;

pub use encrypt::{RtspCipher, RtspCryptoError, RtspKey};
pub use message::{Headers, MAX_MESSAGE_BYTES, RTSP_VERSION, Request, Response, RtspError};
pub use sdp::VideoSdp;

/// RTSP methods the host accepts on the GameStream control connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `OPTIONS` — capability probe.
    Options,
    /// `DESCRIBE` — return the SDP.
    Describe,
    /// `SETUP` — bind the media transports.
    Setup,
    /// `PLAY` — start streaming.
    Play,
    /// `ANNOUNCE` — used before input becomes relevant.
    Announce,
    /// `TEARDOWN` — end the session.
    Teardown,
}

impl Method {
    /// The token as it appears on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Options => "OPTIONS",
            Self::Describe => "DESCRIBE",
            Self::Setup => "SETUP",
            Self::Play => "PLAY",
            Self::Announce => "ANNOUNCE",
            Self::Teardown => "TEARDOWN",
        }
    }

    /// Parse a method token, case-insensitively.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        [
            Self::Options,
            Self::Describe,
            Self::Setup,
            Self::Play,
            Self::Announce,
            Self::Teardown,
        ]
        .into_iter()
        .find(|method| method.as_str().eq_ignore_ascii_case(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_round_trip_through_their_token() {
        for method in [
            Method::Options,
            Method::Describe,
            Method::Setup,
            Method::Play,
            Method::Announce,
            Method::Teardown,
        ] {
            assert_eq!(Method::parse(method.as_str()), Some(method));
        }
    }

    #[test]
    fn method_parsing_is_case_insensitive() {
        assert_eq!(Method::parse("describe"), Some(Method::Describe));
        assert_eq!(Method::parse("nope"), None);
    }
}
