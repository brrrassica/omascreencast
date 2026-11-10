//! The route table.
//!
//! Declared separately from the handlers so the set of paths the daemon accepts
//! is reviewable in one place and a trace replay can assert it.

/// Routes the HTTPS server is responsible for, in the order Moonlight uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// `GET /serverinfo` — host capabilities and pairing state (M1-T6).
    ServerInfo,
    /// `GET /applist` — advertised apps (M2-T5).
    AppList,
    /// `GET /pair` — begin pairing (M1-T7).
    Pair,
    /// `GET /unpair` — forget a paired client (M1-T8).
    Unpair,
    /// `GET /getservercert` — serve the host certificate (M1-T4).
    ServerCertificate,
    /// `POST /clientchallenge` — pairing phase 1 (M1-T4).
    ClientChallenge,
    /// `POST /serverchallengeresp` — pairing phase 2 (M1-T4).
    ServerChallengeResponse,
    /// `POST /clientpairingsecret` — pairing phase 3 (M1-T4).
    ClientPairingSecret,
    /// `GET /launch` — create the output and return the RTSP URL (M2-T6).
    Launch,
    /// `GET /resume` — resume a paused session (M2-T8).
    Resume,
    /// `GET /cancel` — tear a session down (M2-T8).
    Cancel,
}

impl Route {
    /// The path component as it appears on the wire.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::ServerInfo => "/serverinfo",
            Self::AppList => "/applist",
            Self::Pair => "/pair",
            Self::Unpair => "/unpair",
            Self::ServerCertificate => "/getservercert",
            Self::ClientChallenge => "/clientchallenge",
            Self::ServerChallengeResponse => "/serverchallengeresp",
            Self::ClientPairingSecret => "/clientpairingsecret",
            Self::Launch => "/launch",
            Self::Resume => "/resume",
            Self::Cancel => "/cancel",
        }
    }

    /// Whether the route must be answered over HTTPS only.
    ///
    /// Every route in this build is HTTPS; the method exists so a future
    /// plaintext diagnostic endpoint has to opt in explicitly.
    #[must_use]
    pub const fn requires_tls(self) -> bool {
        true
    }

    /// Whether a request to this route is allowed before the client is paired.
    ///
    /// The pairing exchange itself, plus the discovery calls Moonlight makes
    /// before it shows a host, must work for an untrusted client. Everything
    /// else requires an entry in the trust store.
    #[must_use]
    pub const fn allows_unpaired(self) -> bool {
        matches!(
            self,
            Self::ServerInfo
                | Self::AppList
                | Self::Pair
                | Self::ServerCertificate
                | Self::ClientChallenge
                | Self::ServerChallengeResponse
                | Self::ClientPairingSecret
        )
    }

    /// Look up a route by its request path.
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        let trimmed = path.trim_end_matches('/');
        [
            Self::ServerInfo,
            Self::AppList,
            Self::Pair,
            Self::Unpair,
            Self::ServerCertificate,
            Self::ClientChallenge,
            Self::ServerChallengeResponse,
            Self::ClientPairingSecret,
            Self::Launch,
            Self::Resume,
            Self::Cancel,
        ]
        .into_iter()
        .find(|route| route.path() == trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pairing_exchange_and_discovery_work_unpaired() {
        assert!(Route::ServerInfo.allows_unpaired());
        assert!(Route::ServerCertificate.allows_unpaired());
        assert!(Route::ClientChallenge.allows_unpaired());
        assert!(!Route::Launch.allows_unpaired());
        assert!(!Route::Unpair.allows_unpaired());
        assert!(!Route::Cancel.allows_unpaired());
    }
}
