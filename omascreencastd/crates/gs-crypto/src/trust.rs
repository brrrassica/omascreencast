//! The paired-client trust store: `clients.json` (M1-T8, D9).
//!
//! # What is trusted, and why it is a fingerprint
//!
//! A GameStream client proves itself during pairing and then presents **its own
//! certificate** on every later HTTPS request. The host therefore has to answer
//! one question on every connection: *is this client certificate one I paired
//! with?* The answer is stored as the SHA-256 fingerprint of the client
//! certificate, not the certificate itself, because:
//!
//! - a fingerprint cannot be confused for a credential the host can use;
//! - comparing a fixed-length hash is a constant-time-friendly operation;
//! - a re-encoded but equivalent certificate still matches.
//!
//! # Fail closed
//!
//! Every decision here fails closed. An unreadable file, a malformed record, a
//! record whose fingerprint is not 64 lowercase hex characters, or a store that
//! cannot be written is a reason to **refuse** a client, never to accept one.
//! A corrupt trust store must not become an open door.
//!
//! A malformed *entry* does not sink the whole store: it is quarantined and
//! skipped, because one bad line from an interrupted write on an older build
//! should not force the operator to re-pair every device. A malformed *file*
//! (not parseable as JSON at all) is an error, because silently starting with an
//! empty trust store would silently un-pair everyone while looking healthy.
//!
//! # Storage (D9)
//!
//! Lives beside the identity under `$XDG_DATA_HOME/omascreencast/gamestream/`,
//! never in the plugin folder, and is written `0600` through the same atomic
//! rename path as the private key.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;
use crate::paths::IdentityPaths;

/// File mode for the trust store. The list of trusted devices is not secret in
/// the way a private key is, but it is a list of what will be accepted, so it is
/// owner-only.
const STORE_MODE: u32 = 0o600;

/// Length of a SHA-256 fingerprint in lowercase hex.
const FINGERPRINT_HEX_LEN: usize = 64;

/// One paired client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedClient {
    /// Pairing id from the pairing session that created this record.
    pub id: String,
    /// Device name reported at pairing time, for the panel's unpair list.
    pub name: String,
    /// Source address at pairing time. Informational only: D16 says the host
    /// authorises by pairing, never by IP, so this is never compared on
    /// connect.
    pub address: String,
    /// SHA-256 of the client certificate's DER encoding, lowercase hex.
    pub fingerprint: String,
    /// When the client was paired, in seconds since the Unix epoch.
    pub paired_at: u64,
}

impl TrustedClient {
    /// Build a record, normalising the fingerprint.
    ///
    /// Case is folded because a fingerprint is compared as hex text and an
    /// uppercase copy from a tool must still match.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        address: impl Into<String>,
        fingerprint: impl Into<String>,
        paired_at: u64,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            address: address.into(),
            fingerprint: fingerprint.into().to_ascii_lowercase(),
            paired_at,
        }
    }

    /// Whether the stored fingerprint is well-formed enough to trust.
    fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && self.fingerprint.len() == FINGERPRINT_HEX_LEN
            && self
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

/// The on-disk format.
///
/// `version` lets a future change recognise an old file instead of
/// misinterpret it. An unknown version is refused rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    clients: Vec<TrustedClient>,
}

/// The current trust-store format version.
const STORE_VERSION: u32 = 1;

/// Why a trust-store operation failed.
///
/// Every variant means "do not trust this client", never "trust anyway".
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// Reading or writing the file failed.
    #[error("trust store io: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not valid JSON, or is not the expected shape.
    #[error("trust store is corrupt: {0}")]
    Corrupt(String),
    /// The file declares a version this build does not understand.
    #[error("trust store version {found} is not supported (this build writes {expected})")]
    UnsupportedVersion {
        /// Version found on disk.
        found: u32,
        /// Version this build writes.
        expected: u32,
    },
}

/// The paired-client trust store.
///
/// Loaded once at startup and kept in memory so the TLS verify callback does no
/// I/O. Mutations rewrite the whole file atomically: the store is small (one
/// entry per paired device) and a full rewrite is far simpler to make crash-safe
/// than an in-place edit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustStore {
    clients: Vec<TrustedClient>,
}

impl TrustStore {
    /// An empty store.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load the store, treating a missing file as empty.
    ///
    /// A missing file is the correct state for a host nobody has paired with
    /// yet, so it is not an error. A present-but-unparseable file **is** an
    /// error: starting empty would silently un-pair every device.
    ///
    /// # Errors
    ///
    /// - [`TrustError::Io`] when the file exists but cannot be read;
    /// - [`TrustError::Corrupt`] when it is not the expected JSON shape;
    /// - [`TrustError::UnsupportedVersion`] when `version` is not known.
    pub fn load(paths: &IdentityPaths) -> Result<Self, TrustError> {
        Self::load_from(&paths.clients)
    }

    /// Load from an explicit path. Used by [`Self::load`] and by tests.
    ///
    /// # Errors
    ///
    /// As [`Self::load`].
    pub fn load_from(path: &PathBuf) -> Result<Self, TrustError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(error) => return Err(TrustError::Io(error)),
        };
        let file: StoreFile =
            serde_json::from_str(&text).map_err(|error| TrustError::Corrupt(error.to_string()))?;
        if file.version != STORE_VERSION {
            return Err(TrustError::UnsupportedVersion {
                found: file.version,
                expected: STORE_VERSION,
            });
        }
        // Quarantine invalid entries rather than failing the load, but never
        // keep them: an entry that cannot be validated is an entry that must
        // not authorise a connection.
        let clients = file
            .clients
            .into_iter()
            .filter(|client| client.is_valid())
            .collect();
        Ok(Self { clients })
    }

    /// Persist the store atomically at `0600`.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures, including creating the data directory.
    pub fn save(&self, paths: &IdentityPaths) -> Result<(), TrustError> {
        fs::create_dir_all(&paths.dir)?;
        let file = StoreFile {
            version: STORE_VERSION,
            clients: self.clients.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|error| TrustError::Corrupt(error.to_string()))?;
        write_atomic(&paths.clients, &json, STORE_MODE)?;
        Ok(())
    }

    /// Every trusted client, in insertion order.
    #[must_use]
    pub fn clients(&self) -> &[TrustedClient] {
        &self.clients
    }

    /// Whether any client is paired.
    ///
    /// This is what `/serverinfo` reports as `PairStatus`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// How many clients are paired.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Add a client and persist.
    ///
    /// Re-pairing a device replaces the record with the same id, so a client
    /// that pairs twice does not appear twice. A record with an invalid
    /// fingerprint is refused.
    ///
    /// # Errors
    ///
    /// - [`TrustError::Corrupt`] when the record fails validation;
    /// - [`TrustError::Io`] when the store cannot be written.
    pub fn add(&mut self, client: TrustedClient, paths: &IdentityPaths) -> Result<(), TrustError> {
        if !client.is_valid() {
            return Err(TrustError::Corrupt(format!(
                "client {} has an invalid fingerprint or id",
                client.id
            )));
        }
        self.clients.retain(|existing| existing.id != client.id);
        self.clients.push(client);
        self.save(paths)
    }

    /// Remove a client by id and persist.
    ///
    /// Returns whether anything was removed. Removing a client that is not
    /// present is not an error: unpair is expected to be idempotent.
    ///
    /// # Errors
    ///
    /// [`TrustError::Io`] when the store cannot be written.
    pub fn remove(&mut self, id: &str, paths: &IdentityPaths) -> Result<bool, TrustError> {
        let before = self.clients.len();
        self.clients.retain(|client| client.id != id);
        let removed = self.clients.len() != before;
        if removed {
            self.save(paths)?;
        }
        Ok(removed)
    }

    /// Whether a client certificate fingerprint is trusted.
    ///
    /// This is the question the TLS verify callback asks. The comparison folds
    /// case so a fingerprint supplied in uppercase still matches, and it fails
    /// closed on anything malformed: a fingerprint that is not 64 hex
    /// characters cannot match a valid record.
    #[must_use]
    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        let normalised = fingerprint.to_ascii_lowercase();
        if normalised.len() != FINGERPRINT_HEX_LEN {
            return false;
        }
        self.clients
            .iter()
            .any(|client| client.fingerprint == normalised)
    }

    /// Look up a client by fingerprint.
    #[must_use]
    pub fn find_by_fingerprint(&self, fingerprint: &str) -> Option<&TrustedClient> {
        let normalised = fingerprint.to_ascii_lowercase();
        self.clients
            .iter()
            .find(|client| client.fingerprint == normalised)
    }
}

/// The current time in seconds since the Unix epoch, for [`TrustedClient::new`].
///
/// Kept here rather than in the caller so every record uses one clock source.
#[must_use]
pub fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// SHA-256 fingerprint of a DER-encoded certificate, lowercase hex.
///
/// This is the value stored in [`TrustedClient::fingerprint`]. It is the same
/// computation as
/// [`Identity::fingerprint_hex`](crate::identity::Identity::fingerprint_hex) but
/// for an arbitrary certificate, which is what the client's trust decision
/// needs.
///
/// # Errors
///
/// Returns [`TrustError::Corrupt`] when the bytes are not a certificate.
pub fn certificate_fingerprint(der: &[u8]) -> Result<String, TrustError> {
    let certificate = openssl::x509::X509::from_der(der)
        .map_err(|error| TrustError::Corrupt(format!("not a certificate: {error}")))?;
    let digest = certificate
        .digest(openssl::hash::MessageDigest::sha256())
        .map_err(|error| TrustError::Corrupt(error.to_string()))?;
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::paths_under;

    fn temp_paths() -> (tempfile::TempDir, IdentityPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        (dir, paths)
    }

    fn fingerprint(seed: u8) -> String {
        (0..FINGERPRINT_HEX_LEN)
            .map(|index| format!("{:x}", (u32::from(seed) + index as u32) % 16))
            .collect()
    }

    fn sample(id: &str) -> TrustedClient {
        TrustedClient::new(
            id,
            "Living Room TV",
            "192.168.1.40",
            fingerprint(0),
            1_700_000_000,
        )
    }

    #[test]
    fn a_missing_file_is_an_empty_store_not_an_error() {
        let (_dir, paths) = temp_paths();
        let store = TrustStore::load(&paths).unwrap();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(!store.is_trusted(&fingerprint(0)));
    }

    #[test]
    fn add_persists_and_reloads() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        assert!(paths.clients.is_file());

        let reloaded = TrustStore::load(&paths).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.clients()[0].name, "Living Room TV");
    }

    #[test]
    fn the_store_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        let mode = fs::metadata(&paths.clients).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, STORE_MODE);
    }

    #[test]
    fn trust_decision_matches_only_the_stored_fingerprint() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        assert!(store.is_trusted(&fingerprint(0)));
        // Case is folded.
        assert!(store.is_trusted(&fingerprint(0).to_uppercase()));
        assert!(!store.is_trusted(&fingerprint(1)));
    }

    #[test]
    fn trust_fails_closed_on_malformed_fingerprints() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        for bad in ["", "abc", &"z".repeat(FINGERPRINT_HEX_LEN)] {
            assert!(!store.is_trusted(bad), "{bad:?} must not be trusted");
        }
    }

    #[test]
    fn re_adding_the_same_id_replaces_rather_than_duplicates() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        let mut renamed = sample("a");
        renamed.name = "Den".to_owned();
        store.add(renamed, &paths).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.clients()[0].name, "Den");
    }

    #[test]
    fn an_invalid_record_is_refused_on_add() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        let bad = TrustedClient::new("a", "TV", "1.2.3.4", "not-a-fingerprint", 0);
        assert!(matches!(
            store.add(bad, &paths),
            Err(TrustError::Corrupt(_))
        ));
        // The store was not modified and nothing was written.
        assert!(store.is_empty());
        assert!(!paths.clients.is_file());
    }

    #[test]
    fn remove_is_idempotent_and_persists() {
        let (_dir, paths) = temp_paths();
        let mut store = TrustStore::empty();
        store.add(sample("a"), &paths).unwrap();
        assert!(store.remove("a", &paths).unwrap());
        assert!(store.is_empty());
        assert!(!store.is_trusted(&fingerprint(0)));
        // Removing again is a no-op, not an error.
        assert!(!store.remove("a", &paths).unwrap());
        assert!(TrustStore::load(&paths).unwrap().is_empty());
    }

    #[test]
    fn invalid_entries_are_quarantined_on_load_not_trusted() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(&paths.dir).unwrap();
        let json = serde_json::json!({
            "version": 1,
            "clients": [
                {"id": "good", "name": "TV", "address": "1.2.3.4",
                 "fingerprint": fingerprint(0), "paired_at": 0},
                {"id": "bad", "name": "TV", "address": "1.2.3.4",
                 "fingerprint": "nope", "paired_at": 0}
            ]
        });
        fs::write(&paths.clients, serde_json::to_vec(&json).unwrap()).unwrap();
        let store = TrustStore::load(&paths).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.clients()[0].id, "good");
        assert!(!store.is_trusted("nope"));
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_an_empty_store() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(&paths.dir).unwrap();
        fs::write(&paths.clients, b"{ not json").unwrap();
        assert!(matches!(
            TrustStore::load(&paths),
            Err(TrustError::Corrupt(_))
        ));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(&paths.dir).unwrap();
        let json = serde_json::json!({"version": 99, "clients": []});
        fs::write(&paths.clients, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(matches!(
            TrustStore::load(&paths),
            Err(TrustError::UnsupportedVersion { found: 99, .. })
        ));
    }

    #[test]
    fn fingerprint_of_a_real_certificate_is_64_lowercase_hex() {
        let identity = crate::identity::Identity::generate("client").unwrap();
        let der = identity.certificate_der().unwrap();
        let fingerprint = certificate_fingerprint(&der).unwrap();
        assert_eq!(fingerprint.len(), FINGERPRINT_HEX_LEN);
        assert!(
            fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        // And it matches the identity's own helper.
        assert_eq!(fingerprint, identity.fingerprint_hex());
    }

    #[test]
    fn a_certificate_fingerprint_can_be_trusted_end_to_end() {
        let (_dir, paths) = temp_paths();
        let identity = crate::identity::Identity::generate("client").unwrap();
        let fingerprint = certificate_fingerprint(&identity.certificate_der().unwrap()).unwrap();

        let mut store = TrustStore::empty();
        store
            .add(
                TrustedClient::new("id", "TV", "1.2.3.4", &fingerprint, now_epoch_seconds()),
                &paths,
            )
            .unwrap();

        let reloaded = TrustStore::load(&paths).unwrap();
        assert!(reloaded.is_trusted(&fingerprint));
        assert_eq!(reloaded.find_by_fingerprint(&fingerprint).unwrap().id, "id");
    }

    #[test]
    fn garbage_bytes_are_not_a_certificate() {
        assert!(matches!(
            certificate_fingerprint(b"not der"),
            Err(TrustError::Corrupt(_))
        ));
    }
}
