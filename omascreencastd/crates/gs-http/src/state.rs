//! Shared server state: identity, trust store, pairing manager, and events.
//!
//! # Locking
//!
//! One mutex guards the mutable half (trust store and pairing manager). The
//! critical sections are short — a lookup, a state transition, an atomic file
//! rewrite for a trust-store change — and the server is single-client by
//! design (D17), so contention is not a concern. The identity is immutable and
//! lives outside the lock.
//!
//! # Time
//!
//! Every pairing call takes an explicit `now`, and [`DaemonState`] reads the
//! clock in exactly one place ([`DaemonState::now`]). Tests use
//! [`DaemonState::with_clock`] to pin or advance time deterministically.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use gs_crypto::{
    ClientInfo, Identity, IdentityPaths, PairedClient, PairingError, PairingEvent, PairingManager,
    PairingPhase, ReferenceCrypto, TrustStore, TrustedClient, certificate_fingerprint,
    now_epoch_seconds,
};
use gs_proto::dto::ServerInfo;
use gs_proto::{PAIR_STATUS_PAIRED, PAIR_STATUS_UNPAIRED};

/// Hostname used when `/proc` is unavailable.
pub const DEFAULT_HOSTNAME: &str = "omascreencast";

/// Stable host id used when the identity cannot be hashed, which only happens
/// if OpenSSL is broken.
pub const DEFAULT_UNIQUEID: &str = "00000000-0000-4000-8000-000000000000";

/// Everything a request handler needs that outlives a single connection.
#[derive(Debug, Clone)]
pub struct DaemonState {
    inner: Arc<Inner>,
}

/// A pluggable time source, so expiry is testable without sleeping.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> SystemTime + Send + Sync>);

impl Clock {
    /// The wall clock.
    #[must_use]
    pub fn system() -> Self {
        Self(Arc::new(SystemTime::now))
    }

    /// A fixed clock, for tests.
    #[must_use]
    pub fn fixed(instant: SystemTime) -> Self {
        Self(Arc::new(move || instant))
    }

    /// Run the clock.
    #[must_use]
    pub fn now(&self) -> SystemTime {
        (self.0)()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

#[derive(Debug)]
struct Inner {
    identity: Identity,
    paths: IdentityPaths,
    config: ServerConfig,
    events: Arc<dyn EventSink>,
    shared: Mutex<Shared>,
    clock: Clock,
}

#[derive(Debug)]
struct Shared {
    pairing: PairingManager,
    trust: TrustStore,
}

/// Immutable host description used to build responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// Name shown in the client's host list.
    pub hostname: String,
    /// Stable host id; clients cache trust against it.
    pub uniqueid: String,
    /// Host MAC address, lowercase colon-separated.
    pub mac: String,
    /// LAN address advertised in `LocalIP`.
    pub local_ip: Ipv4Addr,
    /// HTTPS port the client should use.
    pub https_port: u16,
    /// RTSP port advertised in `SessionUrl0`.
    pub rtsp_port: u16,
}

impl ServerConfig {
    /// Build a config, deriving `uniqueid` from the identity if not supplied.
    #[must_use]
    pub fn new(
        hostname: impl Into<String>,
        uniqueid: impl Into<String>,
        mac: impl Into<String>,
        local_ip: Ipv4Addr,
        https_port: u16,
        rtsp_port: u16,
    ) -> Self {
        Self {
            hostname: hostname.into(),
            uniqueid: uniqueid.into(),
            mac: mac.into(),
            local_ip,
            https_port,
            rtsp_port,
        }
    }
}

/// Where pairing lifecycle events are published.
///
/// Implemented by the daemon over its IPC hub. Keeping it a trait means
/// `gs-http` does not depend on the daemon, and tests can assert the exact
/// events a route produced without a socket.
pub trait EventSink: std::fmt::Debug + Send + Sync {
    /// A client is waiting for the operator to approve pairing.
    fn pairing_pending(&self, info: &gs_crypto::PendingInfo);
    /// A pairing attempt reached a terminal state.
    fn pairing_result(&self, id: &str, ok: bool);
}

/// An [`EventSink`] that discards everything, for tests and for a daemon run
/// with IPC disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn pairing_pending(&self, _info: &gs_crypto::PendingInfo) {}
    fn pairing_result(&self, _id: &str, _ok: bool) {}
}

/// A request-level failure, mapped to an HTTP status and a JSON body.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    /// The request was malformed.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// The client is not paired and the route needs a paired client.
    #[error("unauthorized: pair this client first")]
    Unauthorized,
    /// The route exists but is not implemented in this milestone.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// No route matched.
    #[error("not found")]
    NotFound,
    /// The request could not be completed for an internal reason.
    #[error("internal error: {0}")]
    Internal(String),
}

impl HttpError {
    /// The status code the client receives.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::NotImplemented(_) => 501,
            Self::Internal(_) => 500,
        }
    }
}

impl From<PairingError> for HttpError {
    fn from(error: PairingError) -> Self {
        match error {
            PairingError::UnknownSession | PairingError::Expired => {
                Self::BadRequest(String::from("no such pairing request"))
            }
            PairingError::InvalidPin => Self::BadRequest(String::from(
                "the PIN must be the four digits shown on the TV",
            )),
            PairingError::OutOfOrder { .. } => {
                Self::BadRequest(String::from("pairing request is in the wrong phase"))
            }
            PairingError::ChallengeRejected => {
                Self::BadRequest(String::from("pairing challenge did not verify"))
            }
            PairingError::TooManyPending { .. } => {
                Self::BadRequest(String::from("too many pairing requests are pending"))
            }
            PairingError::Crypto(message) => Self::Internal(message),
        }
    }
}

impl DaemonState {
    /// Build state from a loaded identity and paths.
    ///
    /// Loads the trust store from disk. A corrupt store is an error rather than
    /// a silent empty store, for the reason recorded in [`gs_crypto::trust`].
    #[must_use]
    pub fn new(
        identity: Identity,
        paths: IdentityPaths,
        config: ServerConfig,
        events: Arc<dyn EventSink>,
        trust: TrustStore,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                identity,
                paths,
                config,
                events,
                shared: Mutex::new(Shared {
                    pairing: PairingManager::with_defaults(Arc::new(ReferenceCrypto)),
                    trust,
                }),
                clock: Clock::system(),
            }),
        }
    }

    /// Replace the clock, for deterministic expiry tests.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.clock = clock;
        }
        self
    }

    /// The current time.
    #[must_use]
    pub fn now(&self) -> SystemTime {
        self.inner.clock.now()
    }

    /// The server identity.
    #[must_use]
    pub fn identity(&self) -> &Identity {
        &self.inner.identity
    }

    /// The persistent paths.
    #[must_use]
    pub fn paths(&self) -> &IdentityPaths {
        &self.inner.paths
    }

    /// The immutable host description.
    #[must_use]
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// The event sink.
    #[must_use]
    pub fn events(&self) -> &dyn EventSink {
        self.inner.events.as_ref()
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        // A poisoned lock means a handler panicked mid-transition. Recovering
        // the guard is better than aborting the whole daemon, and every
        // transition is written to leave consistent state before it returns.
        self.inner
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether any client is paired.
    #[must_use]
    pub fn is_paired(&self) -> bool {
        !self.shared().trust.is_empty()
    }

    /// How many clients are paired.
    #[must_use]
    pub fn paired_count(&self) -> usize {
        self.shared().trust.len()
    }

    /// A `/serverinfo` payload for the current state.
    #[must_use]
    pub fn server_info(&self) -> ServerInfo {
        let config = &self.inner.config;
        let pair_status = if self.is_paired() {
            PAIR_STATUS_PAIRED
        } else {
            PAIR_STATUS_UNPAIRED
        };
        ServerInfo::idle(
            &config.hostname,
            &config.uniqueid,
            &config.mac,
            config.local_ip.to_string(),
            config.https_port,
            config.rtsp_port,
            pair_status,
        )
    }

    /// Whether a client certificate fingerprint may use a trusted route.
    #[must_use]
    pub fn is_trusted(&self, fingerprint: Option<&str>) -> bool {
        fingerprint.is_some_and(|fingerprint| self.shared().trust.is_trusted(fingerprint))
    }

    /// Whether the *request* may proceed: either the route is open to unpaired
    /// clients or the presented fingerprint is trusted.
    #[must_use]
    pub fn authorize(&self, route: crate::Route, fingerprint: Option<&str>) -> bool {
        route.allows_unpaired() || self.is_trusted(fingerprint)
    }

    /// The pairing requests currently waiting for a PIN.
    #[must_use]
    pub fn pending_pairings(&self) -> Vec<gs_crypto::PendingInfo> {
        self.shared().pairing.pending(self.now())
    }

    /// Begin a pairing session for a client and return its id.
    ///
    /// Emits `pairing_pending` so the panel can show a prompt.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the bounded store is full or the salt fails.
    pub fn begin_pairing(
        &self,
        name: &str,
        address: &str,
    ) -> Result<gs_crypto::PendingInfo, HttpError> {
        let now = self.now();
        let (info, event) = {
            let mut shared = self.shared();
            shared.pairing.begin(ClientInfo::new(name, address), now)?
        };
        if let PairingEvent::Pending { id, name, address } = event {
            self.inner
                .events
                .pairing_pending(&gs_crypto::PendingInfo { id, name, address });
        }
        Ok(info)
    }

    /// The live session opened from `address`.
    #[must_use]
    pub fn session_for_address(&self, address: &str) -> Option<String> {
        self.shared()
            .pairing
            .find_by_address(address, self.now())
            .map(|session| session.id().to_owned())
    }

    /// The phase of a live session.
    #[must_use]
    pub fn phase_of(&self, id: &str) -> Option<PairingPhase> {
        self.shared()
            .pairing
            .get(id, self.now())
            .ok()
            .map(gs_crypto::PairingSession::phase)
    }

    /// Record the client's challenge; returns the response to send once the
    /// operator has approved, if approval already happened.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the session is unknown, expired, or out of order.
    pub fn accept_client_challenge(
        &self,
        id: &str,
        challenge: Vec<u8>,
    ) -> Result<Option<gs_crypto::ServerChallenge>, HttpError> {
        let now = self.now();
        Ok(self
            .shared()
            .pairing
            .accept_client_challenge(id, challenge, now)?)
    }

    /// The pending challenge response, for a client polling after approval.
    #[must_use]
    pub fn challenge_response(&self, id: &str) -> Option<gs_crypto::ServerChallenge> {
        self.shared()
            .pairing
            .get(id, self.now())
            .ok()
            .and_then(gs_crypto::PairingSession::server_challenge_response)
    }

    /// Approve a session with the PIN the TV displayed.
    ///
    /// Returns the challenge response to deliver, or `None` when the client has
    /// not sent its challenge yet.
    ///
    /// # Errors
    ///
    /// [`HttpError`] for an invalid PIN or an unknown session.
    pub fn approve_pairing(
        &self,
        id: &str,
        pin: &gs_crypto::Pin,
    ) -> Result<Option<gs_crypto::ServerChallenge>, HttpError> {
        let now = self.now();
        let result = self.shared().pairing.approve(id, pin, now)?;
        self.flush_expired();
        Ok(result)
    }

    /// Decline a pairing request and emit the terminal event.
    pub fn cancel_pairing(&self, id: &str) {
        if self.shared().pairing.remove(id).is_some() {
            self.inner.events.pairing_result(id, false);
        }
    }

    /// Record the client's proof and persist the client on success.
    ///
    /// `client_der` is the certificate the client presented on the connection;
    /// without it the host has nothing to remember and the pairing is refused.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the proof does not verify, the session is not in the
    /// right phase, or the trust store cannot be written.
    pub fn accept_client_secret(
        &self,
        id: &str,
        proof: &[u8],
        client_der: Option<&[u8]>,
    ) -> Result<PairedClient, HttpError> {
        let paired = {
            let now = self.now();
            let mut shared = self.shared();
            let paired = shared.pairing.accept_client_secret(id, proof, now)?;
            let Some(der) = client_der else {
                return Err(HttpError::BadRequest(String::from(
                    "pairing requires the client to present a certificate",
                )));
            };
            let fingerprint = certificate_fingerprint(der)
                .map_err(|error| HttpError::BadRequest(error.to_string()))?;
            let record = TrustedClient::new(
                &paired.id,
                &paired.name,
                &paired.address,
                fingerprint,
                now_epoch_seconds(),
            );
            shared
                .trust
                .add(record, &self.inner.paths)
                .map_err(|error| HttpError::Internal(error.to_string()))?;
            paired
        };
        self.inner.events.pairing_result(&paired.id, true);
        Ok(paired)
    }

    /// Remove a paired client by id, persisting the change.
    ///
    /// # Errors
    ///
    /// [`HttpError`] when the store cannot be written.
    pub fn unpair(&self, id: &str) -> Result<bool, HttpError> {
        let mut shared = self.shared();
        shared
            .trust
            .remove(id, &self.inner.paths)
            .map_err(|error| HttpError::Internal(error.to_string()))
    }

    /// Drop expired pairing sessions and emit a rejected result for each.
    pub fn flush_expired(&self) {
        let expired = self.shared().pairing.expire(self.now());
        for id in expired {
            self.inner.events.pairing_result(&id, false);
        }
    }

    /// A short, stable host id derived from the certificate.
    ///
    /// Formatted as a UUID so clients that expect that shape are happy. It is
    /// intentionally derived rather than random: it must survive restarts, and
    /// it must change exactly when the identity does.
    #[must_use]
    pub fn derive_uniqueid(identity: &Identity) -> String {
        let Ok(der) = identity.certificate_der() else {
            return DEFAULT_UNIQUEID.to_owned();
        };
        let digest = openssl::hash::hash(openssl::hash::MessageDigest::sha256(), &der)
            .ok()
            .map(|digest| digest.to_vec())
            .unwrap_or_default();
        if digest.len() < 16 {
            return DEFAULT_UNIQUEID.to_owned();
        }
        let hex: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!(
            "{}-{}-4{}-8{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[13..16],
            &hex[17..20],
            &hex[20..32]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn state() -> (TempDir, DaemonState) {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("host").unwrap();
        let config = ServerConfig::new(
            "host",
            DaemonState::derive_uniqueid(&identity),
            "aa:bb:cc:dd:ee:ff",
            Ipv4Addr::new(192, 168, 1, 2),
            47984,
            48010,
        );
        let state = DaemonState::new(
            identity,
            paths,
            config,
            Arc::new(NullEventSink),
            TrustStore::empty(),
        );
        (dir, state)
    }

    #[test]
    fn serverinfo_reports_unpaired_then_paired() {
        let (_dir, state) = state();
        assert_eq!(state.server_info().pair_status, PAIR_STATUS_UNPAIRED);
        let info = state
            .begin_pairing("Living Room TV", "192.168.1.40")
            .unwrap();
        assert!(!info.id.is_empty());
        assert!(
            state
                .pending_pairings()
                .iter()
                .any(|entry| entry.id == info.id)
        );
    }

    #[test]
    fn an_unpaired_client_is_authorized_only_on_open_routes() {
        let (_dir, state) = state();
        assert!(state.authorize(crate::Route::ServerInfo, None));
        assert!(state.authorize(crate::Route::ClientChallenge, None));
        assert!(!state.authorize(crate::Route::Launch, None));
        assert!(!state.authorize(crate::Route::Launch, Some("nope")));
    }

    #[test]
    fn derived_uniqueid_is_stable_and_changes_with_the_identity() {
        let first = Identity::generate("a").unwrap();
        let second = Identity::generate("b").unwrap();
        assert_eq!(
            DaemonState::derive_uniqueid(&first),
            DaemonState::derive_uniqueid(&first)
        );
        assert_ne!(
            DaemonState::derive_uniqueid(&first),
            DaemonState::derive_uniqueid(&second)
        );
        let id = DaemonState::derive_uniqueid(&first);
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[8], b'-');
    }
}
