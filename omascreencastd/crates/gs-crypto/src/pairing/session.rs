//! The four-phase pairing state machine and its bounded pending store.
//!
//! # Phases and ordering
//!
//! ```text
//!                 begin()                accept_client_challenge()
//!   (no session) ────────► AwaitingClientChallenge ────► AwaitingApproval
//!                                 │                            │
//!                                 │ approve(pin)               │ approve(pin)
//!                                 ▼                            ▼
//!                            AwaitingClientSecret ◄────────────┘
//!                                 │
//!                                 │ accept_client_secret() and the proof matches
//!                                 ▼
//!                              Complete
//! ```
//!
//! Every transition is checked. A request for a phase the session is not in is
//! refused with [`PairingError::OutOfOrder`] rather than tolerated, because a
//! tolerant handshake accepts a replayed or reordered message that the real
//! client never sends.
//!
//! # Two clocks, one meaning
//!
//! Every state-changing method takes an explicit `now`. Nothing here calls the
//! system clock, so a test can advance time without sleeping and an expiry test
//! is deterministic instead of flaky.
//!
//! # Bounding
//!
//! An unpaired client can open sessions as fast as it can send requests, so the
//! store is bounded ([`DEFAULT_MAX_PENDING`]) and every session has a TTL. The
//! two together are the mitigation recorded against "Pairing abandoned
//! mid-flow" in the plan's risk table.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gs_proto::PAIRING_SESSION_TTL_SECONDS;
use serde::{Deserialize, Serialize};

use super::crypto::{KEY_LEN, PairingCrypto};
use super::{PairingError, PairingEvent, Pin};

/// How many pairing sessions may be pending at once unless configured
/// otherwise.
///
/// The panel shows one prompt at a time, so more than a few pending sessions is
/// already an attack or a stuck client. Eight leaves room for retries without
/// making the store worth exhausting.
pub const DEFAULT_MAX_PENDING: usize = 8;

/// Bytes the host puts in the server challenge.
const SERVER_CHALLENGE_LEN: usize = 16;

/// Bytes identifying a pairing session in URLs and IPC events.
const PAIRING_ID_LEN: usize = 4;

/// Which phase a pairing session is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PairingPhase {
    /// The session exists and is waiting for `/clientchallenge`.
    AwaitingClientChallenge,
    /// The client's challenge arrived; the session is waiting for the operator
    /// to enter the PIN the TV displays.
    AwaitingApproval,
    /// The server challenge was issued; the session is waiting for
    /// `/clientpairingsecret`.
    AwaitingClientSecret,
    /// The client's proof matched. The session is finished and safe to drop.
    Complete,
}

impl PairingPhase {
    /// The phase name used in logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingClientChallenge => "awaiting_client_challenge",
            Self::AwaitingApproval => "awaiting_approval",
            Self::AwaitingClientSecret => "awaiting_client_secret",
            Self::Complete => "complete",
        }
    }

    /// Whether this phase still counts toward the pending bound.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        !matches!(self, Self::Complete)
    }
}

/// Who is asking to pair, as far as the host knows before any crypto runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Device name the client reported, or a placeholder when it sent none.
    pub name: String,
    /// Source address the request arrived from.
    pub address: String,
}

impl ClientInfo {
    /// Build a client description.
    #[must_use]
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address: address.into(),
        }
    }
}

/// A pending session summary, as emitted in a `pairing_pending` event.
///
/// The PIN is deliberately absent: it is supplied by the operator at approval
/// time and must never travel in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInfo {
    /// Pairing session id.
    pub id: String,
    /// Client-reported device name.
    pub name: String,
    /// Source address.
    pub address: String,
}

/// The material the host sends in `/serverchallengeresp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerChallenge {
    /// Opaque server challenge the client must prove against.
    pub challenge: Vec<u8>,
    /// The host's proof of the client's challenge.
    pub proof: Vec<u8>,
}

/// A client that finished pairing.
///
/// This is the value the HTTPS layer stores in the trust store (M1-T8); it is
/// defined here so completion has a shape and the state machine can be tested
/// without the trust store existing yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedClient {
    /// Stable pairing id, reused as the trust-store key.
    pub id: String,
    /// Device name at pairing time.
    pub name: String,
    /// Source address at pairing time.
    pub address: String,
}

/// One in-flight pairing attempt.
///
/// `Debug` is written by hand so neither the PIN-derived key nor the expected
/// proof can reach a log line.
pub struct PairingSession {
    id: String,
    client: ClientInfo,
    phase: PairingPhase,
    salt: Vec<u8>,
    created: SystemTime,
    pin_key: Option<[u8; KEY_LEN]>,
    client_challenge: Option<Vec<u8>>,
    server_challenge: Option<Vec<u8>>,
    /// The host's proof of the client's challenge, retained so it can be
    /// delivered to the client on a later request (approval may happen after
    /// the `/clientchallenge` response was already sent).
    server_proof: Option<Vec<u8>>,
    expected_client_proof: Option<Vec<u8>>,
}

impl std::fmt::Debug for PairingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingSession")
            .field("id", &self.id)
            .field("client", &self.client)
            .field("phase", &self.phase)
            .field("created", &self.created)
            .field("approved", &self.pin_key.is_some())
            .finish_non_exhaustive()
    }
}

impl PairingSession {
    /// The session id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The current phase.
    #[must_use]
    pub fn phase(&self) -> PairingPhase {
        self.phase
    }

    /// The client that opened the session.
    #[must_use]
    pub fn client(&self) -> &ClientInfo {
        &self.client
    }

    /// The salt issued with this session.
    #[must_use]
    pub fn salt(&self) -> &[u8] {
        &self.salt
    }

    /// When the session was created.
    #[must_use]
    pub fn created(&self) -> SystemTime {
        self.created
    }

    /// Whether the session has outlived
    /// [`gs_proto::PAIRING_SESSION_TTL_SECONDS`].
    #[must_use]
    pub fn is_expired(&self, now: SystemTime) -> bool {
        match now.duration_since(self.created) {
            Ok(age) => age > Duration::from_secs(PAIRING_SESSION_TTL_SECONDS),
            // Clock went backwards. Treat the session as expired rather than
            // letting it live forever; a monotonic-clock regression is not a
            // reason to keep a pairing attempt open.
            Err(_) => true,
        }
    }

    /// A summary suitable for a `pairing_pending` event.
    #[must_use]
    pub fn pending_info(&self) -> PendingInfo {
        PendingInfo {
            id: self.id.clone(),
            name: self.client.name.clone(),
            address: self.client.address.clone(),
        }
    }

    /// The challenge response to hand the client once the operator approved.
    ///
    /// `None` until both the client's challenge has arrived and the PIN has
    /// been entered. Approval can therefore happen after the client already
    /// received its `/clientchallenge` reply; the response is delivered on the
    /// client's next polling request.
    #[must_use]
    pub fn server_challenge_response(&self) -> Option<ServerChallenge> {
        match (&self.server_challenge, &self.server_proof) {
            (Some(challenge), Some(proof)) => Some(ServerChallenge {
                challenge: challenge.clone(),
                proof: proof.clone(),
            }),
            _ => None,
        }
    }

    /// Reject a request unless the session is in `expected`.
    ///
    /// The reported `received` is the session's **current** phase, which is
    /// what a confused retry needs: it says where the session actually got to,
    /// not what the caller hoped for.
    fn require(&self, expected: PairingPhase) -> Result<(), PairingError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(PairingError::OutOfOrder {
                expected,
                received: self.phase,
            })
        }
    }
}

/// Owns every pending session and enforces the bound and the TTL.
///
/// The manager is the only type the HTTP layer talks to; it never touches a
/// [`PairingSession`] directly. Every mutating call takes `now` so expiry is
/// checked at the same instant the transition happens, not on a timer that
/// might not have run yet.
pub struct PairingManager {
    crypto: Arc<dyn PairingCrypto>,
    max_pending: usize,
    sessions: HashMap<String, PairingSession>,
}

impl std::fmt::Debug for PairingManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingManager")
            .field("max_pending", &self.max_pending)
            .field("pending", &self.pending_count())
            .field("crypto", &self.crypto)
            .finish_non_exhaustive()
    }
}

impl PairingManager {
    /// Create a manager with an explicit pending bound.
    #[must_use]
    pub fn new(crypto: Arc<dyn PairingCrypto>, max_pending: usize) -> Self {
        Self {
            crypto,
            max_pending,
            sessions: HashMap::new(),
        }
    }

    /// Create a manager with [`DEFAULT_MAX_PENDING`].
    #[must_use]
    pub fn with_defaults(crypto: Arc<dyn PairingCrypto>) -> Self {
        Self::new(crypto, DEFAULT_MAX_PENDING)
    }

    /// How many sessions are still pending.
    ///
    /// Completed sessions are retained briefly so a repeated
    /// `/clientpairingsecret` sees `Complete` rather than `UnknownSession`;
    /// they do not count against the bound.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|session| session.phase.is_pending())
            .count()
    }

    /// Open a pairing session, returning it and the `pairing_pending` event to
    /// emit.
    ///
    /// # Errors
    ///
    /// - [`PairingError::TooManyPending`] when the bound is already reached;
    /// - [`PairingError::Crypto`] when the salt cannot be generated.
    pub fn begin(
        &mut self,
        client: ClientInfo,
        now: SystemTime,
    ) -> Result<(PendingInfo, PairingEvent), PairingError> {
        self.begin_with_salt(client, now, None)
    }

    /// Open a pairing session with a salt supplied by the caller.
    ///
    /// # Why the salt is a parameter
    ///
    /// A captured Moonlight client **chooses the salt itself** and sends it on the
    /// first pairing request (`/pair?…&phrase=getservercert&salt=<hex>`). The
    /// earlier design had the host mint the salt, which is the opposite
    /// direction: it meant the client and host could never agree on the key
    /// material, so the exchange could not succeed against a real client no
    /// matter how correct the KDF was.
    ///
    /// `None` asks the host to generate one, which is kept for tests and for a
    /// client that omits the parameter.
    ///
    /// # Errors
    ///
    /// - [`PairingError::TooManyPending`] when the bound is already reached;
    /// - [`PairingError::Crypto`] when a host-generated salt fails.
    pub fn begin_with_salt(
        &mut self,
        client: ClientInfo,
        now: SystemTime,
        salt: Option<Vec<u8>>,
    ) -> Result<(PendingInfo, PairingEvent), PairingError> {
        self.expire(now);
        if self.pending_count() >= self.max_pending {
            return Err(PairingError::TooManyPending {
                max: self.max_pending,
            });
        }
        let salt = match salt {
            Some(salt) if !salt.is_empty() => salt,
            _ => self.crypto.new_salt()?,
        };
        let mut id = random_hex(PAIRING_ID_LEN)?;
        // A collision is astronomically unlikely, but the store is keyed by id
        // and silently replacing a session would be worse than a retry.
        let mut attempts = 0;
        while self.sessions.contains_key(&id) {
            attempts += 1;
            if attempts >= 8 {
                return Err(PairingError::Crypto(String::from(
                    "could not allocate a unique pairing id",
                )));
            }
            id = random_hex(PAIRING_ID_LEN)?;
        }
        let session = PairingSession {
            id: id.clone(),
            client,
            phase: PairingPhase::AwaitingClientChallenge,
            salt,
            created: now,
            pin_key: None,
            client_challenge: None,
            server_challenge: None,
            server_proof: None,
            expected_client_proof: None,
        };
        let info = session.pending_info();
        self.sessions.insert(id, session);
        Ok((info.clone(), PairingEvent::pending(&info)))
    }

    /// Look up a live session, refusing an expired one.
    ///
    /// # Errors
    ///
    /// [`PairingError::UnknownSession`] when the id is not present, or
    /// [`PairingError::Expired`] when it has outlived its TTL.
    pub fn get(&self, id: &str, now: SystemTime) -> Result<&PairingSession, PairingError> {
        let session = self.sessions.get(id).ok_or(PairingError::UnknownSession)?;
        if session.is_expired(now) {
            return Err(PairingError::Expired);
        }
        Ok(session)
    }

    /// Record the client's challenge (`/clientchallenge`).
    ///
    /// Moves `AwaitingClientChallenge` to `AwaitingApproval`, or straight to
    /// `AwaitingClientSecret` when the PIN was already approved — the operator
    /// may enter the PIN before the client finishes its half of the exchange.
    ///
    /// # Errors
    ///
    /// [`PairingError::OutOfOrder`] when the session is not waiting for a
    /// client challenge.
    pub fn accept_client_challenge(
        &mut self,
        id: &str,
        challenge: Vec<u8>,
        now: SystemTime,
    ) -> Result<Option<ServerChallenge>, PairingError> {
        // Clone the crypto handle before taking the mutable session borrow:
        // both live on `self`, and the state transition needs both.
        let crypto = Arc::clone(&self.crypto);
        let session = self.mutable(id, now)?;
        session.require(PairingPhase::AwaitingClientChallenge)?;
        session.client_challenge = Some(challenge.clone());
        session.phase = PairingPhase::AwaitingApproval;
        // Approval already happened? Then issue the server challenge now, and
        // hand the caller the proof it must return in this response.
        if let Some(key) = session.pin_key {
            let state = issue_server_challenge(&*crypto, &key)?;
            let proof = crypto.server_proof(&key, &challenge)?;
            session.server_challenge = Some(state.challenge.clone());
            session.server_proof = Some(proof.clone());
            session.expected_client_proof = Some(state.expected_proof);
            session.phase = PairingPhase::AwaitingClientSecret;
            return Ok(Some(ServerChallenge {
                challenge: state.challenge,
                proof,
            }));
        }
        Ok(None)
    }

    /// Approve a pending session with the operator's PIN.
    ///
    /// When the client's challenge is present, this returns the
    /// [`ServerChallenge`] to send in `/serverchallengeresp` and moves the
    /// session to `AwaitingClientSecret`. When it is not, the session stays in
    /// `AwaitingClientChallenge` and `accept_client_challenge` completes the
    /// transition later.
    ///
    /// Approval is allowed in `AwaitingClientChallenge`, `AwaitingApproval`,
    /// and `AwaitingClientSecret` (a retry after a lost response). It is
    /// refused once the session is `Complete`.
    ///
    /// # Errors
    ///
    /// - [`PairingError::OutOfOrder`] when the session already completed;
    /// - [`PairingError::Crypto`] when key derivation or the proof fails.
    pub fn approve(
        &mut self,
        id: &str,
        pin: &Pin,
        now: SystemTime,
    ) -> Result<Option<ServerChallenge>, PairingError> {
        let crypto = Arc::clone(&self.crypto);
        let session = self.mutable(id, now)?;
        if session.phase == PairingPhase::Complete {
            return Err(PairingError::OutOfOrder {
                expected: PairingPhase::AwaitingApproval,
                received: PairingPhase::Complete,
            });
        }
        let key = crypto.derive_pin_key(pin, &session.salt)?;
        session.pin_key = Some(key);

        // Without the client's challenge there is nothing to prove yet.
        let Some(client_challenge) = session.client_challenge.clone() else {
            return Ok(None);
        };
        let state = issue_server_challenge(&*crypto, &key)?;
        let proof = crypto.server_proof(&key, &client_challenge)?;
        session.server_challenge = Some(state.challenge.clone());
        session.server_proof = Some(proof.clone());
        session.expected_client_proof = Some(state.expected_proof);
        session.phase = PairingPhase::AwaitingClientSecret;
        Ok(Some(ServerChallenge {
            challenge: state.challenge,
            proof,
        }))
    }

    /// Every live pending session, oldest first.
    ///
    /// Used by the operator-facing `pair` command and the panel: the operator
    /// needs the list of requests waiting for a PIN, not just the newest.
    #[must_use]
    pub fn pending(&self, now: SystemTime) -> Vec<PendingInfo> {
        let mut pending: Vec<&PairingSession> = self
            .sessions
            .values()
            .filter(|session| {
                session.phase.is_pending()
                    && session.phase != PairingPhase::AwaitingClientSecret
                    && !session.is_expired(now)
            })
            .collect();
        pending.sort_by_key(|session| session.created);
        pending
            .iter()
            .map(|session| session.pending_info())
            .collect()
    }

    /// The live session opened from a given source address, if any.
    ///
    /// GameStream does not echo the host's pairing id on later phase requests,
    /// so the source address is the only stable correlation the host has. When
    /// more than one session exists for an address the newest wins: an
    /// abandoned attempt from the same client must not shadow a fresh retry.
    #[must_use]
    pub fn find_by_address(&self, address: &str, now: SystemTime) -> Option<&PairingSession> {
        self.sessions
            .values()
            .filter(|session| {
                session.client.address == address
                    && session.phase.is_pending()
                    && !session.is_expired(now)
            })
            .max_by_key(|session| session.created)
    }

    /// Accept the client's proof (`/clientpairingsecret`).
    ///
    /// On a match the session becomes `Complete` and the [`PairedClient`] is
    /// returned for the trust store. On a mismatch the session is left in
    /// `AwaitingClientSecret` so a network-retried request can still succeed,
    /// but the failure is reported.
    ///
    /// # Errors
    ///
    /// - [`PairingError::OutOfOrder`] when the session is not awaiting the
    ///   client's secret;
    /// - [`PairingError::ChallengeRejected`] when the proof does not match.
    pub fn accept_client_secret(
        &mut self,
        id: &str,
        proof: &[u8],
        now: SystemTime,
    ) -> Result<PairedClient, PairingError> {
        let crypto = Arc::clone(&self.crypto);
        let session = self.mutable(id, now)?;
        session.require(PairingPhase::AwaitingClientSecret)?;
        let expected = session
            .expected_client_proof
            .as_deref()
            .ok_or(PairingError::ChallengeRejected)?;
        if !crypto.proofs_match(expected, proof) {
            return Err(PairingError::ChallengeRejected);
        }
        session.phase = PairingPhase::Complete;
        Ok(PairedClient {
            id: session.id.clone(),
            name: session.client.name.clone(),
            address: session.client.address.clone(),
        })
    }

    /// Drop expired sessions and return the ids that were removed.
    ///
    /// The caller turns each id into a rejected `pairing_result` event, so an
    /// abandoned pairing does not leave the panel showing a prompt forever.
    pub fn expire(&mut self, now: SystemTime) -> Vec<String> {
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.sessions.remove(id);
        }
        expired
    }

    /// Forget a session outright, whether pending or complete.
    ///
    /// Used when the operator cancels a prompt.
    pub fn remove(&mut self, id: &str) -> Option<PairingSession> {
        self.sessions.remove(id)
    }

    fn mutable(&mut self, id: &str, now: SystemTime) -> Result<&mut PairingSession, PairingError> {
        let session = self
            .sessions
            .get_mut(id)
            .ok_or(PairingError::UnknownSession)?;
        if session.is_expired(now) {
            return Err(PairingError::Expired);
        }
        Ok(session)
    }
}

/// The values derived when a server challenge is issued.
struct IssuedChallenge {
    challenge: Vec<u8>,
    expected_proof: Vec<u8>,
}

fn issue_server_challenge(
    crypto: &dyn PairingCrypto,
    key: &[u8; KEY_LEN],
) -> Result<IssuedChallenge, PairingError> {
    let challenge = random_bytes(SERVER_CHALLENGE_LEN)?;
    let expected_proof = crypto.expected_client_proof(key, &challenge)?;
    Ok(IssuedChallenge {
        challenge,
        expected_proof,
    })
}

/// Random bytes from the platform entropy source, hex-encoded.
fn random_hex(len: usize) -> Result<String, PairingError> {
    Ok(random_bytes(len)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Random bytes from the platform entropy source.
fn random_bytes(len: usize) -> Result<Vec<u8>, PairingError> {
    let mut bytes = vec![0u8; len];
    openssl::rand::rand_bytes(&mut bytes)
        .map_err(|error| PairingError::Crypto(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::ReferenceCrypto;
    use std::time::UNIX_EPOCH;

    fn manager() -> PairingManager {
        PairingManager::with_defaults(Arc::new(ReferenceCrypto))
    }

    fn t0() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    /// The error from looking a session up, for assertions that do not want to
    /// compare the whole `Result<&PairingSession, _>`.
    fn lookup_error(manager: &PairingManager, id: &str, now: SystemTime) -> PairingError {
        match manager.get(id, now) {
            Ok(_) => panic!("expected an error for {id}"),
            Err(error) => error,
        }
    }

    fn later(seconds: u64) -> SystemTime {
        t0() + Duration::from_secs(seconds)
    }

    /// Drive a session to `Complete` and return the paired client.
    fn complete_pairing(manager: &mut PairingManager, id: &str, pin: &str) -> PairedClient {
        manager
            .accept_client_challenge(id, b"client-challenge".to_vec(), t0())
            .unwrap();
        let challenge = manager
            .approve(id, &Pin::parse(pin).unwrap(), later(1))
            .unwrap()
            .expect("challenge should be issued once the client challenge exists");
        let key = ReferenceCrypto
            .derive_pin_key(
                &Pin::parse(pin).unwrap(),
                manager.get(id, t0()).unwrap().salt(),
            )
            .unwrap();
        let proof = ReferenceCrypto
            .expected_client_proof(&key, &challenge.challenge)
            .unwrap();
        manager.accept_client_secret(id, &proof, later(2)).unwrap()
    }

    #[test]
    fn begin_opens_a_session_and_emits_a_pending_event() {
        let mut manager = manager();
        let (info, event) = manager
            .begin(ClientInfo::new("Living Room TV", "192.168.1.40"), t0())
            .unwrap();
        assert_eq!(info.name, "Living Room TV");
        assert_eq!(info.address, "192.168.1.40");
        assert_eq!(info.id.len(), PAIRING_ID_LEN * 2);
        assert_eq!(event, PairingEvent::pending(&info));
        assert_eq!(
            manager.get(&info.id, t0()).unwrap().phase(),
            PairingPhase::AwaitingClientChallenge
        );
        assert_eq!(manager.pending_count(), 1);
    }

    #[test]
    fn the_happy_path_reaches_complete() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        let paired = complete_pairing(&mut manager, &info.id, "1234");
        assert_eq!(paired.id, info.id);
        assert_eq!(paired.name, "TV");
        assert_eq!(
            manager.get(&info.id, t0()).unwrap().phase(),
            PairingPhase::Complete
        );
        // A completed session no longer counts against the pending bound.
        assert_eq!(manager.pending_count(), 0);
    }

    #[test]
    fn a_client_challenge_is_refused_out_of_order() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        manager
            .accept_client_challenge(&info.id, b"c".to_vec(), t0())
            .unwrap();
        // The second challenge arrives in AwaitingApproval, not
        // AwaitingClientChallenge.
        assert!(matches!(
            manager.accept_client_challenge(&info.id, b"c".to_vec(), t0()),
            Err(PairingError::OutOfOrder { .. })
        ));
    }

    #[test]
    fn a_client_secret_before_approval_is_refused_out_of_order() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        manager
            .accept_client_challenge(&info.id, b"c".to_vec(), t0())
            .unwrap();
        assert!(matches!(
            manager.accept_client_secret(&info.id, b"proof", t0()),
            Err(PairingError::OutOfOrder { .. })
        ));
    }

    #[test]
    fn approval_before_the_challenge_defers_the_server_challenge() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        // Approve first: there is nothing to prove against yet.
        assert_eq!(
            manager
                .approve(&info.id, &Pin::parse("1234").unwrap(), t0())
                .unwrap(),
            None
        );
        assert_eq!(
            manager.get(&info.id, t0()).unwrap().phase(),
            PairingPhase::AwaitingClientChallenge
        );
        // The client challenge then drives the transition in one step.
        manager
            .accept_client_challenge(&info.id, b"client-challenge".to_vec(), t0())
            .unwrap();
        assert_eq!(
            manager.get(&info.id, t0()).unwrap().phase(),
            PairingPhase::AwaitingClientSecret
        );
    }

    #[test]
    fn a_wrong_client_proof_is_rejected_and_the_session_stays_open() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        manager
            .accept_client_challenge(&info.id, b"client-challenge".to_vec(), t0())
            .unwrap();
        manager
            .approve(&info.id, &Pin::parse("1234").unwrap(), t0())
            .unwrap()
            .unwrap();
        assert_eq!(
            manager.accept_client_secret(&info.id, b"wrong", later(1)),
            Err(PairingError::ChallengeRejected)
        );
        // A retry is still possible.
        assert_eq!(
            manager.get(&info.id, later(1)).unwrap().phase(),
            PairingPhase::AwaitingClientSecret
        );
    }

    #[test]
    fn a_pin_derived_from_the_wrong_pin_cannot_complete() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        manager
            .accept_client_challenge(&info.id, b"client-challenge".to_vec(), t0())
            .unwrap();
        let challenge = manager
            .approve(&info.id, &Pin::parse("1234").unwrap(), t0())
            .unwrap()
            .unwrap();
        let salt = manager.get(&info.id, t0()).unwrap().salt().to_vec();
        let wrong_key = ReferenceCrypto
            .derive_pin_key(&Pin::parse("9999").unwrap(), &salt)
            .unwrap();
        let wrong_proof = ReferenceCrypto
            .expected_client_proof(&wrong_key, &challenge.challenge)
            .unwrap();
        assert_eq!(
            manager.accept_client_secret(&info.id, &wrong_proof, later(1)),
            Err(PairingError::ChallengeRejected)
        );
    }

    #[test]
    fn approving_a_completed_session_is_refused() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        complete_pairing(&mut manager, &info.id, "1234");
        assert!(matches!(
            manager.approve(&info.id, &Pin::parse("1234").unwrap(), later(3)),
            Err(PairingError::OutOfOrder { .. })
        ));
    }

    #[test]
    fn unknown_ids_are_reported_as_unknown() {
        let mut manager = manager();
        assert_eq!(
            manager.accept_client_challenge("nope", b"c".to_vec(), t0()),
            Err(PairingError::UnknownSession)
        );
        assert_eq!(
            lookup_error(&manager, "nope", t0()),
            PairingError::UnknownSession
        );
        assert_eq!(
            manager.accept_client_secret("nope", b"p", t0()),
            Err(PairingError::UnknownSession)
        );
    }

    #[test]
    fn a_session_expires_after_the_ttl_and_is_swept() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        let just_before = later(PAIRING_SESSION_TTL_SECONDS);
        assert!(manager.get(&info.id, just_before).is_ok());
        let after = later(PAIRING_SESSION_TTL_SECONDS + 1);
        assert_eq!(
            lookup_error(&manager, &info.id, after),
            PairingError::Expired
        );
        assert_eq!(manager.expire(after), vec![info.id.clone()]);
        assert_eq!(
            lookup_error(&manager, &info.id, after),
            PairingError::UnknownSession
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_expires_rather_than_revives() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        let before_creation = t0() - Duration::from_secs(5);
        assert_eq!(
            lookup_error(&manager, &info.id, before_creation),
            PairingError::Expired
        );
    }

    #[test]
    fn expiry_reports_every_dropped_id() {
        let mut manager = manager();
        let (a, _) = manager
            .begin(ClientInfo::new("A", "10.0.0.2"), t0())
            .unwrap();
        let (b, _) = manager
            .begin(ClientInfo::new("B", "10.0.0.3"), t0())
            .unwrap();
        let mut removed = manager.expire(later(PAIRING_SESSION_TTL_SECONDS + 1));
        removed.sort();
        let mut expected = vec![a.id, b.id];
        expected.sort();
        assert_eq!(removed, expected);
        assert_eq!(manager.pending_count(), 0);
    }

    #[test]
    fn the_pending_store_is_bounded() {
        let mut manager = PairingManager::new(Arc::new(ReferenceCrypto), 2);
        manager
            .begin(ClientInfo::new("A", "10.0.0.2"), t0())
            .unwrap();
        manager
            .begin(ClientInfo::new("B", "10.0.0.3"), t0())
            .unwrap();
        assert_eq!(
            manager.begin(ClientInfo::new("C", "10.0.0.4"), t0()),
            Err(PairingError::TooManyPending { max: 2 })
        );
        // Expiring the earlier sessions frees room again.
        manager.expire(later(PAIRING_SESSION_TTL_SECONDS + 1));
        assert!(
            manager
                .begin(ClientInfo::new("D", "10.0.0.5"), t0())
                .is_ok()
        );
    }

    #[test]
    fn remove_forgets_a_pending_session() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        assert!(manager.remove(&info.id).is_some());
        assert_eq!(
            lookup_error(&manager, &info.id, t0()),
            PairingError::UnknownSession
        );
        assert!(manager.remove(&info.id).is_none());
    }

    #[test]
    fn two_sessions_get_distinct_ids_and_salts() {
        let mut manager = manager();
        let (a, _) = manager
            .begin(ClientInfo::new("A", "10.0.0.2"), t0())
            .unwrap();
        let (b, _) = manager
            .begin(ClientInfo::new("B", "10.0.0.3"), t0())
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_ne!(
            manager.get(&a.id, t0()).unwrap().salt(),
            manager.get(&b.id, t0()).unwrap().salt()
        );
    }

    #[test]
    fn session_debug_never_prints_key_material() {
        let mut manager = manager();
        let (info, _) = manager
            .begin(ClientInfo::new("TV", "10.0.0.2"), t0())
            .unwrap();
        manager
            .accept_client_challenge(&info.id, b"client-challenge".to_vec(), t0())
            .unwrap();
        manager
            .approve(&info.id, &Pin::parse("1234").unwrap(), t0())
            .unwrap();
        let debug = format!("{:?}", manager.get(&info.id, t0()).unwrap());
        assert!(debug.contains("approved: true"));
        assert!(!debug.contains("expected_client_proof"));
        assert!(!format!("{manager:?}").contains("1234"));
    }

    #[test]
    fn phases_report_their_pending_state() {
        assert!(PairingPhase::AwaitingClientChallenge.is_pending());
        assert!(PairingPhase::AwaitingApproval.is_pending());
        assert!(PairingPhase::AwaitingClientSecret.is_pending());
        assert!(!PairingPhase::Complete.is_pending());
        assert_eq!(PairingPhase::AwaitingApproval.as_str(), "awaiting_approval");
    }
}
