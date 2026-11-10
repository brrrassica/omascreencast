//! The server certificate identity: generate once, load on every subsequent run.
//!
//! The identity is an RSA-2048 self-signed certificate plus its key. RSA is
//! used because it is what GameStream clients are built to present and verify;
//! the constraint is the client's, not a preference here.
//!
//! Regeneration is explicit, never implicit:
//!
//! - neither file present → generate and persist both;
//! - both present → load, no write;
//! - exactly one present → [`IdentityError::Incomplete`]. Silently replacing
//!   half the pair would produce an identity that no paired client trusts,
//!   while looking like a successful start.

use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Rsa;
use openssl::x509::extension::{BasicConstraints, KeyUsage, SubjectKeyIdentifier};
use openssl::x509::{X509, X509NameBuilder};

use crate::fsutil::write_atomic;
use crate::paths::IdentityPaths;

/// RSA key size. 2048 is the floor every Moonlight build accepts and the
/// cheapest option that is not outright rejected by modern TLS stacks.
pub const KEY_BITS: u32 = 2048;

/// How many days a generated certificate is valid for. The certificate is
/// pinned by the client at pairing time, so validity is mostly cosmetic; ten
/// years avoids a surprise expiry in a long-lived install.
pub const VALIDITY_DAYS: u32 = 3650;

/// File modes. The private key must not be group- or world-readable.
const KEY_MODE: u32 = 0o600;
/// The certificate is public.
const CERT_MODE: u32 = 0o644;

/// A loaded server identity.
///
/// `Clone` is cheap (OpenSSL reference counts the underlying key) and lets the
/// HTTPS listener hand a copy to each accept loop without re-reading disk.
#[derive(Clone)]
pub struct Identity {
    certificate: X509,
    key: PKey<Private>,
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material, even at trace level.
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint_hex())
            .field("key_bits", &self.key.bits())
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Generate a fresh self-signed identity.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::OpenSsl`] if key or certificate generation
    /// fails, which in practice means the system entropy source is broken.
    pub fn generate(common_name: &str) -> Result<Self, IdentityError> {
        let rsa = Rsa::generate(KEY_BITS)?;
        let key = PKey::from_rsa(rsa)?;

        let mut name = X509NameBuilder::new()?;
        name.append_entry_by_text("CN", common_name)?;
        let name = name.build();

        let mut builder = X509::builder()?;
        builder.set_version(2)?; // X.509 v3
        builder.set_subject_name(&name)?;
        builder.set_issuer_name(&name)?;
        builder.set_pubkey(&key)?;

        let serial = {
            let mut serial = BigNum::new()?;
            serial.rand(159, MsbOption::MAYBE_ZERO, false)?;
            serial.to_asn1_integer()?
        };
        builder.set_serial_number(&serial)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let not_before = Asn1Time::from_unix(now.saturating_sub(60) as i64)?;
        let not_after = Asn1Time::from_unix((now + u64::from(VALIDITY_DAYS) * 86_400) as i64)?;
        builder.set_not_before(&not_before)?;
        builder.set_not_after(&not_after)?;

        builder.append_extension(BasicConstraints::new().critical().build()?)?;
        builder.append_extension(
            KeyUsage::new()
                .critical()
                .digital_signature()
                .key_encipherment()
                .build()?,
        )?;
        let subject_key_id =
            SubjectKeyIdentifier::new().build(&builder.x509v3_context(None, None))?;
        builder.append_extension(subject_key_id)?;

        builder.sign(&key, MessageDigest::sha256())?;
        let certificate = builder.build();

        let cert_pem = certificate.to_pem()?;
        let key_pem = key.private_key_to_pem_pkcs8()?;

        Ok(Self {
            certificate,
            key,
            cert_pem,
            key_pem,
        })
    }

    /// Load an identity from a certificate and key in PEM form.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::OpenSsl`] when either PEM block is invalid or
    /// the key does not match the certificate.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, IdentityError> {
        let certificate = X509::from_pem(cert_pem)?;
        let key = PKey::private_key_from_pem(key_pem)?;
        if !certificate.public_key()?.public_eq(&key) {
            return Err(IdentityError::KeyMismatch);
        }
        Ok(Self {
            certificate,
            key,
            cert_pem: cert_pem.to_vec(),
            key_pem: key_pem.to_vec(),
        })
    }

    /// Load the identity if both files exist, otherwise create and persist it.
    ///
    /// # Errors
    ///
    /// - [`IdentityError::Incomplete`] when exactly one file exists;
    /// - [`IdentityError::Io`] when the directory cannot be created or the
    ///   files cannot be read or written;
    /// - [`IdentityError::KeyMismatch`] when a stored pair disagrees;
    /// - [`IdentityError::OpenSsl`] for malformed stored PEM.
    pub fn load_or_generate(
        paths: &IdentityPaths,
        common_name: &str,
    ) -> Result<Self, IdentityError> {
        let cert_exists = paths.cert.is_file();
        let key_exists = paths.key.is_file();
        match (cert_exists, key_exists) {
            (true, true) => {
                let cert_pem = fs::read(&paths.cert).map_err(IdentityError::Io)?;
                let key_pem = fs::read(&paths.key).map_err(IdentityError::Io)?;
                Self::from_pem(&cert_pem, &key_pem)
            }
            (false, false) => {
                let identity = Self::generate(common_name)?;
                identity.persist(paths)?;
                Ok(identity)
            }
            _ => Err(IdentityError::Incomplete {
                cert: paths.cert.clone(),
                key: paths.key.clone(),
            }),
        }
    }

    /// Write the identity to `paths`, creating the directory if needed.
    ///
    /// Writes are atomic: each file is written to a sibling temporary file and
    /// renamed, so a crash cannot leave a half-written key. The key file is
    /// created with mode `0600` before any bytes are written.
    ///
    /// # Errors
    ///
    /// Propagates I/O failures.
    pub fn persist(&self, paths: &IdentityPaths) -> Result<(), IdentityError> {
        fs::create_dir_all(&paths.dir).map_err(IdentityError::Io)?;
        write_atomic(&paths.key, &self.key_pem, KEY_MODE).map_err(IdentityError::Io)?;
        write_atomic(&paths.cert, &self.cert_pem, CERT_MODE).map_err(IdentityError::Io)?;
        Ok(())
    }

    /// The certificate in PEM form, as served by `/getservercert`.
    #[must_use]
    pub fn certificate_pem(&self) -> &[u8] {
        &self.cert_pem
    }

    /// The private key in PEM form. Never log or serve this.
    #[must_use]
    pub fn key_pem(&self) -> &[u8] {
        &self.key_pem
    }

    /// The parsed certificate.
    #[must_use]
    pub fn certificate(&self) -> &X509 {
        &self.certificate
    }

    /// The parsed private key.
    #[must_use]
    pub fn key(&self) -> &PKey<Private> {
        &self.key
    }

    /// SHA-256 fingerprint of the DER-encoded certificate, lowercase hex.
    ///
    /// This is the value shown to the operator and compared with the client's
    /// view; it is derived, never stored.
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        match self.certificate.digest(MessageDigest::sha256()) {
            Ok(digest) => digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            Err(_) => String::from("unavailable"),
        }
    }

    /// The certificate's DER encoding, used by the identity command and tests.
    ///
    /// # Errors
    ///
    /// Propagates OpenSSL encoding failures.
    pub fn certificate_der(&self) -> Result<Vec<u8>, IdentityError> {
        Ok(self.certificate.to_der()?)
    }
}

/// Why an identity could not be generated, loaded, or persisted.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The underlying OpenSSL operation failed.
    #[error("openssl: {0}")]
    OpenSsl(#[from] openssl::error::ErrorStack),
    /// A file operation failed.
    #[error("identity file: {0}")]
    Io(#[source] io::Error),
    /// Exactly one of the certificate and key files exists.
    #[error(
        "incomplete identity: {} and {} must both exist or both be absent; \
         delete both to regenerate (this un-pairs every client)",
        .cert.display(),
        .key.display()
    )]
    Incomplete {
        /// The certificate path that was checked.
        cert: std::path::PathBuf,
        /// The key path that was checked.
        key: std::path::PathBuf,
    },
    /// The stored key does not belong to the stored certificate.
    #[error("stored key does not match the stored certificate")]
    KeyMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::paths_under;
    use std::os::unix::fs::PermissionsExt as _;

    fn temp_paths() -> (tempfile::TempDir, IdentityPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        (dir, paths)
    }

    #[test]
    fn generated_identity_has_a_usable_certificate() {
        let identity = Identity::generate("test-host").unwrap();
        assert_eq!(identity.key().bits(), KEY_BITS);
        assert!(
            identity
                .certificate_pem()
                .starts_with(b"-----BEGIN CERTIFICATE-----")
        );
        assert!(
            identity
                .key_pem()
                .starts_with(b"-----BEGIN PRIVATE KEY-----")
        );
        assert_eq!(identity.fingerprint_hex().len(), 64);
        assert!(identity.certificate_der().unwrap().len() > 100);
    }

    #[test]
    fn pem_round_trips_and_rejects_a_mismatched_key() {
        let first = Identity::generate("a").unwrap();
        let second = Identity::generate("b").unwrap();
        let reloaded = Identity::from_pem(first.certificate_pem(), first.key_pem()).unwrap();
        assert_eq!(reloaded.fingerprint_hex(), first.fingerprint_hex());

        assert!(matches!(
            Identity::from_pem(first.certificate_pem(), second.key_pem()),
            Err(IdentityError::KeyMismatch)
        ));
    }

    #[test]
    fn first_run_generates_and_persists_both_files() {
        let (_dir, paths) = temp_paths();
        let identity = Identity::load_or_generate(&paths, "host").unwrap();
        assert!(paths.cert.is_file());
        assert!(paths.key.is_file());
        assert_eq!(fs::read(&paths.cert).unwrap(), identity.certificate_pem());
        assert_eq!(fs::read(&paths.key).unwrap(), identity.key_pem());
    }

    #[test]
    fn key_file_is_owner_only() {
        let (_dir, paths) = temp_paths();
        Identity::load_or_generate(&paths, "host").unwrap();
        let mode = fs::metadata(&paths.key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, KEY_MODE, "key must not be readable by group or world");
    }

    #[test]
    fn second_run_reuses_the_same_identity() {
        let (_dir, paths) = temp_paths();
        let first = Identity::load_or_generate(&paths, "host").unwrap();
        let second = Identity::load_or_generate(&paths, "host").unwrap();
        assert_eq!(first.fingerprint_hex(), second.fingerprint_hex());
    }

    #[test]
    fn a_single_file_is_an_error_not_a_regeneration() {
        let (_dir, paths) = temp_paths();
        let identity = Identity::load_or_generate(&paths, "host").unwrap();
        let _ = identity;
        fs::remove_file(&paths.key).unwrap();
        assert!(matches!(
            Identity::load_or_generate(&paths, "host"),
            Err(IdentityError::Incomplete { .. })
        ));
        // The remaining certificate was not replaced.
        assert!(paths.cert.is_file());
    }

    #[test]
    fn malformed_pem_is_reported() {
        assert!(matches!(
            Identity::from_pem(b"not a certificate", b"not a key"),
            Err(IdentityError::OpenSsl(_))
        ));
    }

    #[test]
    fn debug_output_does_not_leak_key_material() {
        let identity = Identity::generate("host").unwrap();
        let debug = format!("{identity:?}");
        assert!(debug.contains("fingerprint"));
        assert!(!debug.contains("PRIVATE KEY"));
    }
}
