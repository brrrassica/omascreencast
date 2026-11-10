//! Golden-trace replay for the pairing crypto (M1-T11).
//!
//! # What this is
//!
//! The M0 trace corpus (M0-T5/T7) pins the byte-level GameStream pairing
//! exchange. This module turns a captured exchange into an executable
//! assertion: a fixture is a **known-answer vector** — a PIN, a salt, and the
//! challenge bytes — plus the values the protocol fixes for them, and
//! [`replay`] drives the active [`PairingCrypto`] and checks the result.
//!
//! # Why it is public
//!
//! Integration tests cannot reach private items, and a replay must exercise the
//! same [`PairingCrypto`] the daemon uses rather than a re-implementation. The
//! module is therefore public but is **test support**: nothing in the daemon
//! calls it.
//!
//! # The empty corpus
//!
//! The corpus is empty until M0-T5 runs. [`load_fixtures`] returns an empty
//! vector for an empty directory, and the integration test skips rather than
//! failing, so CI is green before the capture lands and starts asserting the
//! moment it does.
//!
//! # Clean-room note
//!
//! Fixtures are captures, never hand-written protocol bytes. The format below
//! is this project's own; see `tests/fixtures/README.md`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::pairing::{PairingCrypto, Pin};

/// The directory the pairing trace corpus lives in, relative to this crate.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pairing")
}

/// One captured pairing known-answer vector.
///
/// Every field is a wire fact: the PIN the operator entered, the salt the host
/// issued, and the challenge bytes the client and host exchanged. The `expect`
/// block holds the values the protocol fixes for those inputs.
#[derive(Debug, Clone, Deserialize)]
pub struct PairingFixture {
    /// Human-readable name, used in failure messages.
    pub name: String,
    /// The four-digit PIN the operator entered.
    pub pin: String,
    /// The salt the host issued, hex-encoded.
    pub salt_hex: String,
    /// The client's challenge, hex-encoded, when the vector covers the host's
    /// proof of it.
    #[serde(default)]
    pub client_challenge_hex: Option<String>,
    /// The host's challenge, hex-encoded, when the vector covers the value the
    /// client must return for it.
    #[serde(default)]
    pub server_challenge_hex: Option<String>,
    /// The values the protocol fixes for this vector.
    pub expect: ExpectedValues,
}

/// The values a fixture asserts.
///
/// Every field is optional so a capture can assert exactly the parts the
/// protocol fixes and leave the rest unasserted. At least one must be present;
/// a fixture that asserts nothing is a fixture-authoring error, not a pass.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedValues {
    /// The PIN-derived key, hex-encoded.
    #[serde(default)]
    pub pin_key_hex: Option<String>,
    /// The host's proof of the client's challenge, hex-encoded.
    #[serde(default)]
    pub server_proof_hex: Option<String>,
    /// The value the client must return for the host's challenge, hex-encoded.
    #[serde(default)]
    pub client_proof_hex: Option<String>,
}

impl PairingFixture {
    /// Decode a hex field, attributing a failure to this fixture.
    fn hex(&self, field: &'static str, value: &str) -> Result<Vec<u8>, ReplayError> {
        decode_hex(value).ok_or_else(|| ReplayError::Hex {
            name: self.name.clone(),
            field,
        })
    }

    /// Build a mismatch error attributed to this fixture.
    fn mismatch(&self, detail: impl Into<String>) -> ReplayError {
        ReplayError::Mismatch {
            name: self.name.clone(),
            detail: detail.into(),
        }
    }
}

/// Why a fixture could not be loaded or replayed.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// A fixture file could not be read.
    #[error("could not read {path}: {source}")]
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A fixture line was not valid JSON, or did not match the schema.
    #[error("{path}:{line}: {source}")]
    Parse {
        /// The file that failed.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// A hex field was malformed.
    #[error("{name}: {field} is not valid hex")]
    Hex {
        /// Fixture name.
        name: String,
        /// Field name.
        field: &'static str,
    },
    /// The fixture's PIN was not four decimal digits.
    #[error("{name}: pin is not four decimal digits")]
    Pin {
        /// Fixture name.
        name: String,
    },
    /// The fixture asserts nothing, so replaying it proves nothing.
    #[error("{name}: fixture asserts no values")]
    NoAssertions {
        /// Fixture name.
        name: String,
    },
    /// A derived value did not match the fixture.
    #[error("{name}: {detail}")]
    Mismatch {
        /// Fixture name.
        name: String,
        /// What differed.
        detail: String,
    },
}

/// Load every `*.jsonl` fixture under `dir`, sorted by file name.
///
/// A missing or empty directory yields an empty vector: the corpus is empty
/// until M0-T5 runs, and that is not an error.
///
/// # Errors
///
/// [`ReplayError`] when a file cannot be read or a line is not a valid fixture.
pub fn load_fixtures(dir: &Path) -> Result<Vec<PairingFixture>, ReplayError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if dir.is_dir() {
        let entries = std::fs::read_dir(dir).map_err(|source| ReplayError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ReplayError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                paths.push(path);
            }
        }
    }
    paths.sort();

    let mut fixtures = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| ReplayError::Io {
            path: path.clone(),
            source,
        })?;
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fixture: PairingFixture =
                serde_json::from_str(line).map_err(|source| ReplayError::Parse {
                    path: path.clone(),
                    line: index + 1,
                    source,
                })?;
            fixtures.push(fixture);
        }
    }
    Ok(fixtures)
}

/// Replay one fixture against `crypto` and assert the derived values.
///
/// # Errors
///
/// [`ReplayError`] when the PIN or a hex field is malformed, the fixture
/// asserts nothing, or a derived value differs from the fixture.
pub fn replay(crypto: &dyn PairingCrypto, fixture: &PairingFixture) -> Result<(), ReplayError> {
    let expected = &fixture.expect;
    if expected.pin_key_hex.is_none()
        && expected.server_proof_hex.is_none()
        && expected.client_proof_hex.is_none()
    {
        return Err(ReplayError::NoAssertions {
            name: fixture.name.clone(),
        });
    }

    let pin = Pin::parse(&fixture.pin).map_err(|_| ReplayError::Pin {
        name: fixture.name.clone(),
    })?;
    let salt = fixture.hex("salt_hex", &fixture.salt_hex)?;
    let key = crypto
        .derive_pin_key(&pin, &salt)
        .map_err(|error| fixture.mismatch(error.to_string()))?;

    if let Some(expected_key) = &expected.pin_key_hex {
        let expected_key = fixture.hex("pin_key_hex", expected_key)?;
        if key.as_slice() != expected_key.as_slice() {
            return Err(fixture.mismatch(format!(
                "pin_key {} != expected {}",
                encode_hex(&key),
                encode_hex(&expected_key)
            )));
        }
    }

    if let Some(expected_proof) = &expected.server_proof_hex {
        let challenge = fixture.client_challenge_hex.as_deref().ok_or_else(|| {
            fixture.mismatch("server_proof_hex asserted without client_challenge_hex")
        })?;
        let challenge = fixture.hex("client_challenge_hex", challenge)?;
        let proof = crypto
            .server_proof(&key, &challenge)
            .map_err(|error| fixture.mismatch(error.to_string()))?;
        let expected_proof = fixture.hex("server_proof_hex", expected_proof)?;
        if proof != expected_proof {
            return Err(fixture.mismatch(format!(
                "server_proof {} != expected {}",
                encode_hex(&proof),
                encode_hex(&expected_proof)
            )));
        }
    }

    if let Some(expected_proof) = &expected.client_proof_hex {
        let challenge = fixture.server_challenge_hex.as_deref().ok_or_else(|| {
            fixture.mismatch("client_proof_hex asserted without server_challenge_hex")
        })?;
        let challenge = fixture.hex("server_challenge_hex", challenge)?;
        let proof = crypto
            .expected_client_proof(&key, &challenge)
            .map_err(|error| fixture.mismatch(error.to_string()))?;
        let expected_proof = fixture.hex("client_proof_hex", expected_proof)?;
        if proof != expected_proof {
            return Err(fixture.mismatch(format!(
                "client_proof {} != expected {}",
                encode_hex(&proof),
                encode_hex(&expected_proof)
            )));
        }
    }

    Ok(())
}

/// Replay every fixture in order against one crypto implementation.
///
/// # Errors
///
/// The first [`ReplayError`] encountered.
pub fn replay_all(
    crypto: &dyn PairingCrypto,
    fixtures: &[PairingFixture],
) -> Result<(), ReplayError> {
    for fixture in fixtures {
        replay(crypto, fixture)?;
    }
    Ok(())
}

/// Decode a hex string, tolerating surrounding whitespace.
///
/// Returns `None` for an odd length or a non-hex character.
fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if value.len() % 2 != 0 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

/// The value of one ASCII hex digit, or `None`.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Lowercase hex encoding, the inverse of [`decode_hex`].
fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::ReferenceCrypto;

    fn fixture(name: &str, expect: ExpectedValues) -> PairingFixture {
        PairingFixture {
            name: name.to_owned(),
            pin: String::from("1234"),
            salt_hex: encode_hex(b"fixed-salt-bytes"),
            client_challenge_hex: Some(encode_hex(b"client-challenge")),
            server_challenge_hex: Some(encode_hex(b"server-challenge")),
            expect,
        }
    }

    #[test]
    fn an_empty_directory_loads_no_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_fixtures(dir.path()).unwrap().is_empty());
        // A directory that does not exist is also not an error.
        assert!(
            load_fixtures(&dir.path().join("missing"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fixtures_parse_from_one_file_and_skip_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sample.jsonl"),
            concat!(
                "# a comment line\n",
                "\n",
                "{\"name\": \"v1\", \"pin\": \"1234\", \"salt_hex\": \"00ff\", ",
                "\"expect\": {\"pin_key_hex\": \"aabb\"}}\n",
            ),
        )
        .unwrap();
        let fixtures = load_fixtures(dir.path()).unwrap();
        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].name, "v1");
        assert_eq!(fixtures[0].expect.pin_key_hex.as_deref(), Some("aabb"));
    }

    #[test]
    fn a_malformed_line_reports_its_position() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.jsonl"), "{not json}\n").unwrap();
        match load_fixtures(dir.path()) {
            Err(ReplayError::Parse { line, .. }) => assert_eq!(line, 1),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn a_matching_fixture_replays_and_a_mismatch_is_reported() {
        let crypto = ReferenceCrypto;
        let pin = Pin::parse("1234").unwrap();
        let salt = b"fixed-salt-bytes";
        let key = crypto.derive_pin_key(&pin, salt).unwrap();
        let server_proof = crypto.server_proof(&key, b"client-challenge").unwrap();
        let client_proof = crypto
            .expected_client_proof(&key, b"server-challenge")
            .unwrap();

        let ok = fixture(
            "all-values",
            ExpectedValues {
                pin_key_hex: Some(encode_hex(&key)),
                server_proof_hex: Some(encode_hex(&server_proof)),
                client_proof_hex: Some(encode_hex(&client_proof)),
            },
        );
        replay(&crypto, &ok).unwrap();

        let mut wrong = ok.clone();
        wrong.expect.server_proof_hex = Some(encode_hex(b"nope"));
        assert!(matches!(
            replay(&crypto, &wrong),
            Err(ReplayError::Mismatch { .. })
        ));
    }

    #[test]
    fn a_fixture_that_asserts_nothing_is_refused() {
        let crypto = ReferenceCrypto;
        let empty = fixture(
            "empty",
            ExpectedValues {
                pin_key_hex: None,
                server_proof_hex: None,
                client_proof_hex: None,
            },
        );
        assert!(matches!(
            replay(&crypto, &empty),
            Err(ReplayError::NoAssertions { .. })
        ));
    }

    #[test]
    fn a_proof_asserted_without_its_challenge_is_refused() {
        let crypto = ReferenceCrypto;
        let mut missing = fixture(
            "missing-challenge",
            ExpectedValues {
                pin_key_hex: None,
                server_proof_hex: Some(String::from("aabb")),
                client_proof_hex: None,
            },
        );
        missing.client_challenge_hex = None;
        assert!(matches!(
            replay(&crypto, &missing),
            Err(ReplayError::Mismatch { .. })
        ));
    }

    #[test]
    fn malformed_hex_and_pins_are_reported() {
        let crypto = ReferenceCrypto;
        let mut bad_hex = fixture(
            "bad-hex",
            ExpectedValues {
                pin_key_hex: Some(String::from("zz")),
                server_proof_hex: None,
                client_proof_hex: None,
            },
        );
        bad_hex.salt_hex = String::from("not-hex");
        assert!(matches!(
            replay(&crypto, &bad_hex),
            Err(ReplayError::Hex { .. })
        ));

        let mut bad_pin = fixture(
            "bad-pin",
            ExpectedValues {
                pin_key_hex: Some(String::from("aabb")),
                server_proof_hex: None,
                client_proof_hex: None,
            },
        );
        bad_pin.pin = String::from("12");
        assert!(matches!(
            replay(&crypto, &bad_pin),
            Err(ReplayError::Pin { .. })
        ));
    }

    #[test]
    fn hex_round_trips_and_rejects_odd_lengths() {
        assert_eq!(decode_hex("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert_eq!(decode_hex("AABB").unwrap(), vec![0xaa, 0xbb]);
        assert!(decode_hex("abc").is_none());
        assert!(decode_hex("zz").is_none());
        assert_eq!(encode_hex(&[0x00, 0xff, 0x10]), "00ff10");
    }
}
