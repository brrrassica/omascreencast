//! GameStream HTTPS server: TLS listener with client-certificate verification,
//! a route table, and the exact error responses the client expects.
//!
//! **Scaffold only.** The implementation lands in M1-T5 (`/serverinfo` in
//! M1-T6, `/pair` in M1-T7). This file exists so the crate boundary and its
//! dependencies are fixed before the server is written, per the M1-T1 task.

#![deny(missing_docs)]

/// Routes the HTTPS server is responsible for, in the order Moonlight uses them.
///
/// Declared now so the route table is reviewable before any handler exists.
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
    fn every_route_looks_up_by_its_own_path() {
        for route in [
            Route::ServerInfo,
            Route::AppList,
            Route::Pair,
            Route::Unpair,
            Route::ServerCertificate,
            Route::ClientChallenge,
            Route::ServerChallengeResponse,
            Route::ClientPairingSecret,
            Route::Launch,
            Route::Resume,
            Route::Cancel,
        ] {
            assert_eq!(Route::from_path(route.path()), Some(route));
        }
    }

    #[test]
    fn trailing_slashes_and_unknown_paths_are_handled() {
        assert_eq!(Route::from_path("/serverinfo/"), Some(Route::ServerInfo));
        assert_eq!(Route::from_path("/nope"), None);
        assert_eq!(Route::from_path(""), None);
    }
}
