//! RTSP message framing, plaintext and `rtspenc`, and the SDP builder.
//!
//! **Scaffold only.** The parser and serialiser land in M2-T1, encrypted
//! framing in M2-T2, and the SDP builder in M2-T3.

#![deny(missing_docs)]

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
