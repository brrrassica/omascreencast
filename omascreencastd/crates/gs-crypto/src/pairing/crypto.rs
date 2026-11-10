//! The byte-level pairing contract, isolated behind one trait.
//!
//! # Why a trait
//!
//! The state machine in [`super::session`] must not encode a guess about a
//! salt length, an iteration count, or an IV derivation. Those are wire facts
//! and are pinned by the M0 trace corpus (M0-T5). Until that capture exists,
//! every such decision lives behind [`PairingCrypto`], so replacing the guess
//! with the captured truth touches this file only.
//!
//! # What [`ReferenceCrypto`] is and is not
//!
//! It is a **provisional, self-consistent implementation**: PIN-derived keys
//! via PBKDF2-HMAC-SHA256, SHA-256 challenge hashes, constant-time comparison.
//! It exists so the handshake can be exercised end to end in tests today.
//!
//! It is **not** claimed to interoperate with Moonlight. Treating it as the
//! real algorithm would be exactly the kind of quiet scope change the plan
//! forbids: a real client will not complete pairing against it, and that
//! failure is expected rather than a bug to paper over.
//!
//! # Clean-room note
//!
//! Built on the public OpenSSL API. No GPL or `moonlight-common-c` source is
//! consulted or transcribed.

use openssl::hash::{MessageDigest, hash};
use openssl::pkcs5::pbkdf2_hmac;
use openssl::rand::rand_bytes;

use super::{PairingError, Pin};

/// Length of every symmetric key and challenge hash this module produces.
pub const KEY_LEN: usize = 32;

/// Length of the random salt sent in the first pairing response.
///
/// Provisional: the real salt length is a trace fact.
pub const SALT_LEN: usize = 16;

/// PBKDF2 work factor used by [`ReferenceCrypto`].
///
/// Provisional, and deliberately high enough that a stolen salt is not free to
/// attack. The real exchange's factor is a trace fact; nothing above this trait
/// depends on the number.
pub const PBKDF2_ROUNDS: usize = 10_000;

/// The cryptographic operations a pairing handshake needs.
///
/// Implementors must be cheap to clone or share; the state machine holds one
/// for the lifetime of a session.
pub trait PairingCrypto: std::fmt::Debug + Send + Sync {
    /// Generate a fresh random salt for a new pairing session.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::Crypto`] when the system entropy source fails.
    fn new_salt(&self) -> Result<Vec<u8>, PairingError>;

    /// Derive the pairing key from the operator's PIN and the session salt.
    ///
    /// The key is the seed for every later operation in the session.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::Crypto`] when key derivation fails.
    fn derive_pin_key(&self, pin: &Pin, salt: &[u8]) -> Result<[u8; KEY_LEN], PairingError>;

    /// Compute the server's challenge response from the pairing key and the
    /// client's challenge bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::Crypto`] when hashing or the keyed step fails.
    fn server_proof(
        &self,
        key: &[u8; KEY_LEN],
        client_challenge: &[u8],
    ) -> Result<Vec<u8>, PairingError>;

    /// Compute the value the client is expected to return for the server's
    /// challenge.
    ///
    /// This is the host-side expectation only; it is never sent.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::Crypto`] when hashing or the keyed step fails.
    fn expected_client_proof(
        &self,
        key: &[u8; KEY_LEN],
        server_challenge: &[u8],
    ) -> Result<Vec<u8>, PairingError>;

    /// Constant-time comparison of two byte strings.
    ///
    /// The default implementation is already constant-time with respect to
    /// content and is not meant to be overridden.
    fn proofs_match(&self, expected: &[u8], received: &[u8]) -> bool {
        if expected.len() != received.len() {
            return false;
        }
        let mut difference = 0u8;
        for (left, right) in expected.iter().zip(received) {
            difference |= left ^ right;
        }
        difference == 0
    }
}

/// A provisional [`PairingCrypto`] used until the trace corpus pins the real
/// byte-level exchange. See the module note.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceCrypto;

impl ReferenceCrypto {
    /// SHA-256 of `data`, used as the unkeyed half of both proofs.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::Crypto`] if OpenSSL refuses to hash, which means
    /// the platform's crypto library is broken.
    fn sha256(data: &[u8]) -> Result<[u8; KEY_LEN], PairingError> {
        let digest = hash(MessageDigest::sha256(), data)
            .map_err(|error| PairingError::Crypto(error.to_string()))?;
        let bytes = digest.as_ref();
        if bytes.len() != KEY_LEN {
            return Err(PairingError::Crypto(String::from(
                "unexpected digest length",
            )));
        }
        let mut out = [0u8; KEY_LEN];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// A keyed hash: `SHA-256(key || 0x00 || domain || 0x00 || data)`.
    ///
    /// This mirrors the shape of an HMAC without claiming to be one; the real
    /// construction is a trace fact and lives behind the trait.
    ///
    /// The `domain` label is not cosmetic. Without it both proofs would be the
    /// same function of the same key, so a client could return the server's own
    /// proof and be accepted — a reflection attack. Distinct labels make the
    /// two values non-interchangeable even though the reference construction is
    /// provisional.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::sha256`].
    fn keyed_hash(key: &[u8], domain: &[u8], data: &[u8]) -> Result<[u8; KEY_LEN], PairingError> {
        let mut material = Vec::with_capacity(key.len() + domain.len() + data.len() + 2);
        material.extend_from_slice(key);
        material.push(0);
        material.extend_from_slice(domain);
        material.push(0);
        material.extend_from_slice(data);
        Self::sha256(&material)
    }
}

impl PairingCrypto for ReferenceCrypto {
    fn new_salt(&self) -> Result<Vec<u8>, PairingError> {
        let mut salt = vec![0u8; SALT_LEN];
        rand_bytes(&mut salt).map_err(|error| PairingError::Crypto(error.to_string()))?;
        Ok(salt)
    }

    fn derive_pin_key(&self, pin: &Pin, salt: &[u8]) -> Result<[u8; KEY_LEN], PairingError> {
        // `Pin::as_bytes` is the only way to reach the digits, so no other
        // module can accidentally derive a key from a redacted `Display`.
        let pin_bytes = pin.as_bytes();
        let mut key = [0u8; KEY_LEN];
        // Argument order is (password, salt, iterations, digest, out).
        pbkdf2_hmac(
            pin_bytes,
            salt,
            PBKDF2_ROUNDS,
            MessageDigest::sha256(),
            &mut key,
        )
        .map_err(|error| PairingError::Crypto(error.to_string()))?;
        Ok(key)
    }

    fn server_proof(
        &self,
        key: &[u8; KEY_LEN],
        client_challenge: &[u8],
    ) -> Result<Vec<u8>, PairingError> {
        // The server's proof is a keyed hash over the client's challenge. The
        // exact construction is provisional; see the module note.
        Ok(Self::keyed_hash(key, b"server-proof", client_challenge)?.to_vec())
    }

    fn expected_client_proof(
        &self,
        key: &[u8; KEY_LEN],
        server_challenge: &[u8],
    ) -> Result<Vec<u8>, PairingError> {
        Ok(Self::keyed_hash(key, b"client-proof", server_challenge)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(pin: &str) -> [u8; KEY_LEN] {
        ReferenceCrypto
            .derive_pin_key(&Pin::parse(pin).unwrap(), b"fixed-salt")
            .unwrap()
    }

    #[test]
    fn salt_is_random_and_the_expected_length() {
        let crypto = ReferenceCrypto;
        let first = crypto.new_salt().unwrap();
        let second = crypto.new_salt().unwrap();
        assert_eq!(first.len(), SALT_LEN);
        assert_ne!(first, second, "two salts must not collide");
    }

    #[test]
    fn key_derivation_is_deterministic_and_pin_sensitive() {
        assert_eq!(key_for("1234"), key_for("1234"));
        assert_ne!(key_for("1234"), key_for("1235"));
    }

    #[test]
    fn key_derivation_is_salt_sensitive() {
        let pin = Pin::parse("1234").unwrap();
        let crypto = ReferenceCrypto;
        let first = crypto.derive_pin_key(&pin, b"salt-a").unwrap();
        let second = crypto.derive_pin_key(&pin, b"salt-b").unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn proofs_are_keyed_and_challenge_sensitive() {
        let crypto = ReferenceCrypto;
        let key = key_for("1234");
        let proof = crypto.server_proof(&key, b"client-challenge").unwrap();
        assert_eq!(proof.len(), KEY_LEN);
        assert_ne!(
            proof,
            crypto.server_proof(&key, b"other-challenge").unwrap()
        );

        let other_key = key_for("9999");
        assert_ne!(
            proof,
            crypto
                .server_proof(&other_key, b"client-challenge")
                .unwrap()
        );
    }

    #[test]
    fn proof_comparison_is_length_aware_and_exact() {
        let crypto = ReferenceCrypto;
        assert!(crypto.proofs_match(b"abcd", b"abcd"));
        assert!(!crypto.proofs_match(b"abcd", b"abce"));
        assert!(!crypto.proofs_match(b"abcd", b"abc"));
        assert!(!crypto.proofs_match(b"", b"a"));
        assert!(crypto.proofs_match(b"", b""));
    }

    #[test]
    fn server_and_client_proofs_use_different_inputs() {
        // The two proofs must not be interchangeable: reusing the client's
        // challenge where the server's belongs would accept a reflected value.
        let crypto = ReferenceCrypto;
        let key = key_for("1234");
        let challenge = b"challenge-bytes";
        assert_ne!(
            crypto.server_proof(&key, challenge).unwrap(),
            crypto.expected_client_proof(&key, challenge).unwrap()
        );
    }
}
