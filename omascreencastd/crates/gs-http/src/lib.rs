//! GameStream server: a trusted HTTPS listener, a plaintext discovery listener,
//! a route table, and the exact error responses the client expects.
//!
//! # Two listeners, as captured
//!
//! A real Sunshine host answers discovery on **plaintext** port 47989 and
//! requires a trusted client certificate on **every** route of HTTPS port 47984.
//! This crate mirrors that: [`DiscoveryServer`] answers only `/serverinfo` and
//! returns a `404` envelope for everything else, while [`HttpServer`] carries
//! the pairing and session routes behind the trust store.
//!
//! # Wire format, as captured
//!
//! Control responses are **XML**, wrapped in `<root status_code="…">`. A
//! protocol failure is HTTP `200`; the client reads `status_code`, not the HTTP
//! status. This corrected the earlier JSON assumption taken from the M0 fake
//! responder; see `omascreencastd/tests/fixtures/m0/README.md`.
//!
//! # Milestone scope
//!
//! This crate implements M1-T5 (`gs-http` server), M1-T6 (`/serverinfo`),
//! M1-T7 (`/pair` and the pairing phases), and the listener half of M1-T8
//! (trusted fingerprints enforced on the routes that need them). `/applist` is
//! served because Moonlight asks for it immediately after pairing and its shape
//! already exists in [`gs_proto::dto`]; `/launch` and `/resume` stay unimplemented
//! until M2.
//!
//! # Authorization model
//!
//! The TLS layer **records** the client certificate but never rejects on it.
//! That is deliberate: an unpaired client must still reach `/serverinfo` and
//! `/getservercert`, and a client that has not yet generated a certificate must
//! be able to start the exchange. Authorization is enforced per route against
//! the trust store (D16: pairing is the authorisation mechanism, not an IP
//! allow-list).
//!
//! # Crypto status
//!
//! The pairing byte-level exchange is still the provisional
//! [`gs_crypto::ReferenceCrypto`]. A real Moonlight client will not complete
//! pairing until M0-T5 pins the real bytes (M1-T4 remains unticked for that
//! reason). The HTTP shape, state transitions, IPC events, trust-store writes,
//! and authorization decisions here are the parts that survive that change.
//!
//! # Clean-room note
//!
//! Built on the public OpenSSL API and the documented GameStream endpoint
//! shape. No Sunshine or `moonlight-common-c` source is transcribed.

#![deny(missing_docs)]

mod discovery;
mod handlers;
mod query;
mod request;
mod route;
mod server;
mod state;
mod xml;

pub mod replay;
pub mod session;

pub use discovery::{DiscoveryError, DiscoveryServer};
pub use handlers::parse_pin;
pub use route::Route;
pub use server::{HttpServer, HttpServerError, local_ipv4};
pub use session::{LaunchOutcome, NullSessions, SessionController, SessionError};
pub use state::{
    DEFAULT_HOSTNAME, DEFAULT_UNIQUEID, DaemonState, EventSink, HttpError, NullEventSink,
    ServerConfig,
};

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
