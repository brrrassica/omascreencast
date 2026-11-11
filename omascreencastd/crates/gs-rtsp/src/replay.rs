//! Golden-trace replay harness (M2-T9).
//!
//! # What this is
//!
//! The M0 trace corpus (M0-T5/T7) is a set of captured GameStream exchanges.
//! The RTSP slice of that corpus is the `DESCRIBE`/`SETUP`/`PLAY`/`TEARDOWN`
//! control conversation, including the `rtspenc` framing when the client offered
//! encrypted transport. This module turns a capture into an executable
//! assertion: a fixture is one or two wire messages — a request and/or a
//! response — plus the fields the protocol fixes for them, and [`replay`] drives
//! the real [`parse_request`]/[`parse_response`] and (when a key is present) the
//! [`RtspCipher`] round trip.
//!
//! # Why it is public
//!
//! Integration tests cannot reach private items, and a replay must exercise the
//! same parser and cipher the daemon uses rather than a re-implementation. The
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

use crate::encrypt::{RtspCipher, RtspKey, TAG_LEN};
use crate::message::{RtspError, parse_request, parse_response};

/// The directory the RTSP trace corpus lives in, relative to this crate.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/rtsp")
}

/// One captured RTSP exchange.
///
/// A fixture carries a request wire message, a response wire message, or both,
/// and the fields the protocol fixes for each. When `rtspenc_key_hex` is
/// present the capture was encrypted, and both messages are additionally
/// round-tripped through the cipher.
#[derive(Debug, Clone, Deserialize)]
pub struct RtspFixture {
    /// Human-readable name, used in failure messages.
    pub name: String,
    /// The captured request, as the exact wire bytes (JSON escapes `\r\n`).
    #[serde(default)]
    pub request_wire: Option<String>,
    /// Fields the captured request must parse to.
    #[serde(default)]
    pub expect_request: Option<ExpectedRequest>,
    /// The captured response, as the exact wire bytes (JSON escapes `\r\n`).
    #[serde(default)]
    pub response_wire: Option<String>,
    /// Fields the captured response must parse to.
    #[serde(default)]
    pub expect_response: Option<ExpectedResponse>,
    /// The `rikey` the host sent on `/launch`, hex-encoded, when the capture
    /// used encrypted RTSP.
    #[serde(default)]
    pub rtspenc_key_hex: Option<String>,
}

/// The fields a captured request must parse to.
///
/// Every field is optional so a capture can assert exactly the parts the
/// protocol fixes and leave the rest unasserted.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExpectedRequest {
    /// The method token, e.g. `DESCRIBE`.
    #[serde(default)]
    pub method: Option<String>,
    /// The request URI, verbatim.
    #[serde(default)]
    pub uri: Option<String>,
    /// The numeric `CSeq`.
    #[serde(default)]
    pub cseq: Option<u32>,
    /// Header name/value pairs, each of which must be present.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body as UTF-8 text, when the capture carries one.
    #[serde(default)]
    pub body: Option<String>,
}

/// The fields a captured response must parse to.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExpectedResponse {
    /// The numeric status code.
    #[serde(default)]
    pub status: Option<u16>,
    /// The reason phrase, verbatim.
    #[serde(default)]
    pub reason: Option<String>,
    /// The numeric `CSeq`.
    #[serde(default)]
    pub cseq: Option<u32>,
    /// Header name/value pairs, each of which must be present.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body as UTF-8 text, when the capture carries one.
    #[serde(default)]
    pub body: Option<String>,
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
    /// The `rtspenc` key decoded to the wrong number of bytes.
    #[error("{name}: rtspenc key must be 16 bytes, got {length}")]
    KeyLength {
        /// Fixture name.
        name: String,
        /// The decoded length, in bytes.
        length: usize,
    },
    /// A wire message did not parse, or did not consume exactly its bytes.
    #[error("{name}: {detail}")]
    Malformed {
        /// Fixture name.
        name: String,
        /// What went wrong.
        detail: String,
    },
    /// The fixture carries neither a request nor a response to replay.
    #[error("{name}: fixture carries no wire message")]
    Empty {
        /// Fixture name.
        name: String,
    },
    /// A parsed or decrypted value did not match the fixture.
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
pub fn load_fixtures(dir: &Path) -> Result<Vec<RtspFixture>, ReplayError> {
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
            let fixture: RtspFixture =
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

/// Replay one fixture and assert its framing, fields, and crypto round-trip.
///
/// # Errors
///
/// [`ReplayError`] when the fixture carries no wire message, a wire message does
/// not parse or does not consume exactly its bytes, a hex field is malformed, a
/// parsed value differs from the fixture, or a cipher round trip does not
/// reproduce the captured bytes.
pub fn replay(fixture: &RtspFixture) -> Result<(), ReplayError> {
    if fixture.request_wire.is_none() && fixture.response_wire.is_none() {
        return Err(ReplayError::Empty {
            name: fixture.name.clone(),
        });
    }

    let key = match &fixture.rtspenc_key_hex {
        Some(hex) => Some(decode_hex(hex).ok_or_else(|| ReplayError::Hex {
            name: fixture.name.clone(),
            field: "rtspenc_key_hex",
        })?),
        None => None,
    };

    if let Some(wire) = &fixture.request_wire {
        let wire = wire.as_bytes();
        let (request, consumed) =
            parse_request(wire).map_err(|error| malformed(fixture, &error))?;
        if consumed != wire.len() {
            return Err(framing(fixture, consumed, wire.len(), "request"));
        }
        if let Some(expected) = &fixture.expect_request {
            check_request(fixture, &request, expected)?;
        }
        if let Some(key) = &key {
            round_trip(fixture, key, wire)?;
        }
    }

    if let Some(wire) = &fixture.response_wire {
        let wire = wire.as_bytes();
        let (response, consumed) =
            parse_response(wire).map_err(|error| malformed(fixture, &error))?;
        if consumed != wire.len() {
            return Err(framing(fixture, consumed, wire.len(), "response"));
        }
        if let Some(expected) = &fixture.expect_response {
            check_response(fixture, &response, expected)?;
        }
        if let Some(key) = &key {
            round_trip(fixture, key, wire)?;
        }
    }

    Ok(())
}

/// Replay every fixture in order.
///
/// # Errors
///
/// The first [`ReplayError`] encountered.
pub fn replay_all(fixtures: &[RtspFixture]) -> Result<(), ReplayError> {
    for fixture in fixtures {
        replay(fixture)?;
    }
    Ok(())
}

/// Encrypt `wire` with `key`, assert the framing, then decrypt it back.
///
/// Both directions start at sequence zero, which is what a single captured
/// message exercises. A multi-message capture should be split into one fixture
/// per message so each starts at zero, matching the recorded cipher state at
/// that point in the conversation.
fn round_trip(fixture: &RtspFixture, key: &[u8], wire: &[u8]) -> Result<(), ReplayError> {
    let key = RtspKey::from_bytes(key).map_err(|_| ReplayError::KeyLength {
        name: fixture.name.clone(),
        length: key.len(),
    })?;

    let mut sender = RtspCipher::new(key.clone());
    let mut receiver = RtspCipher::new(key);

    let framed = sender
        .encrypt(wire)
        .map_err(|error| ReplayError::Mismatch {
            name: fixture.name.clone(),
            detail: format!("rtspenc encrypt failed: {error}"),
        })?;
    if framed.len() != wire.len() + TAG_LEN {
        return Err(ReplayError::Mismatch {
            name: fixture.name.clone(),
            detail: format!(
                "rtspenc framing is {} bytes, expected plaintext + {TAG_LEN}",
                framed.len()
            ),
        });
    }
    let recovered = receiver
        .decrypt(&framed)
        .map_err(|error| ReplayError::Mismatch {
            name: fixture.name.clone(),
            detail: format!("rtspenc decrypt failed: {error}"),
        })?;
    if recovered != wire {
        return Err(ReplayError::Mismatch {
            name: fixture.name.clone(),
            detail: String::from("rtspenc round trip did not reproduce the capture"),
        });
    }
    Ok(())
}

/// Assert a parsed request against its expectation.
fn check_request(
    fixture: &RtspFixture,
    request: &crate::message::Request,
    expected: &ExpectedRequest,
) -> Result<(), ReplayError> {
    if let Some(method) = &expected.method {
        if !request.method.as_str().eq_ignore_ascii_case(method) {
            return Err(mismatch(
                fixture,
                format!("method {} != expected {method}", request.method.as_str()),
            ));
        }
    }
    if let Some(uri) = &expected.uri {
        if &request.uri != uri {
            return Err(mismatch(
                fixture,
                format!("uri {:?} != expected {:?}", request.uri, uri),
            ));
        }
    }
    if let Some(cseq) = expected.cseq {
        if request.cseq() != Some(cseq) {
            return Err(mismatch(
                fixture,
                format!("cseq {:?} != expected {cseq}", request.cseq()),
            ));
        }
    }
    check_headers(fixture, &request.headers, &expected.headers)?;
    if let Some(body) = &expected.body {
        let actual = String::from_utf8_lossy(&request.body);
        if actual != body.as_str() {
            return Err(mismatch(
                fixture,
                format!("request body {actual:?} != expected {body:?}"),
            ));
        }
    }
    Ok(())
}

/// Assert a parsed response against its expectation.
fn check_response(
    fixture: &RtspFixture,
    response: &crate::message::Response,
    expected: &ExpectedResponse,
) -> Result<(), ReplayError> {
    if let Some(status) = expected.status {
        if response.status != status {
            return Err(mismatch(
                fixture,
                format!("status {} != expected {status}", response.status),
            ));
        }
    }
    if let Some(reason) = &expected.reason {
        if &response.reason != reason {
            return Err(mismatch(
                fixture,
                format!("reason {:?} != expected {:?}", response.reason, reason),
            ));
        }
    }
    if let Some(cseq) = expected.cseq {
        if response.cseq() != Some(cseq) {
            return Err(mismatch(
                fixture,
                format!("cseq {:?} != expected {cseq}", response.cseq()),
            ));
        }
    }
    check_headers(fixture, &response.headers, &expected.headers)?;
    if let Some(body) = &expected.body {
        let actual = String::from_utf8_lossy(&response.body);
        if actual != body.as_str() {
            return Err(mismatch(
                fixture,
                format!("response body {actual:?} != expected {body:?}"),
            ));
        }
    }
    Ok(())
}

/// Assert every expected header is present with the expected value.
fn check_headers(
    fixture: &RtspFixture,
    headers: &crate::message::Headers,
    expected: &[(String, String)],
) -> Result<(), ReplayError> {
    for (name, value) in expected {
        match headers.get(name) {
            Some(actual) if actual == value => {}
            Some(actual) => {
                return Err(mismatch(
                    fixture,
                    format!("header {name:?} {actual:?} != expected {value:?}"),
                ));
            }
            None => {
                return Err(mismatch(fixture, format!("missing header {name:?}")));
            }
        }
    }
    Ok(())
}

/// Build a malformed-message error attributed to this fixture.
fn malformed(fixture: &RtspFixture, error: &RtspError) -> ReplayError {
    ReplayError::Malformed {
        name: fixture.name.clone(),
        detail: error.to_string(),
    }
}

/// Build a framing error when a message does not consume exactly its bytes.
fn framing(fixture: &RtspFixture, consumed: usize, total: usize, kind: &str) -> ReplayError {
    ReplayError::Malformed {
        name: fixture.name.clone(),
        detail: format!("consumed {consumed} of {total} {kind} bytes"),
    }
}

/// Build a mismatch error attributed to this fixture.
fn mismatch(fixture: &RtspFixture, detail: impl Into<String>) -> ReplayError {
    ReplayError::Mismatch {
        name: fixture.name.clone(),
        detail: detail.into(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn describe_request() -> RtspFixture {
        RtspFixture {
            name: String::from("describe"),
            request_wire: Some(String::from(
                "DESCRIBE rtsp://host:48010 RTSP/1.0\r\nCSeq: 2\r\nAccept: application/sdp\r\n\r\n",
            )),
            expect_request: Some(ExpectedRequest {
                method: Some(String::from("DESCRIBE")),
                uri: Some(String::from("rtsp://host:48010")),
                cseq: Some(2),
                headers: vec![(String::from("accept"), String::from("application/sdp"))],
                body: None,
            }),
            response_wire: None,
            expect_response: None,
            rtspenc_key_hex: None,
        }
    }

    fn describe_response() -> RtspFixture {
        RtspFixture {
            name: String::from("describe-response"),
            request_wire: None,
            expect_request: None,
            response_wire: Some(String::from(
                "RTSP/1.0 200 OK\r\nCSeq: 2\r\nContent-Type: application/sdp\r\nContent-Length: 9\r\n\r\nv=0\r\nm=vi",
            )),
            expect_response: Some(ExpectedResponse {
                status: Some(200),
                reason: Some(String::from("OK")),
                cseq: Some(2),
                headers: vec![(
                    String::from("Content-Type"),
                    String::from("application/sdp"),
                )],
                body: Some(String::from("v=0\r\nm=vi")),
            }),
            rtspenc_key_hex: None,
        }
    }

    #[test]
    fn an_empty_directory_loads_no_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_fixtures(dir.path()).unwrap().is_empty());
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
                "{\"name\": \"options\", \"request_wire\": \"OPTIONS * RTSP/1.0\\r\\nCSeq: 1\\r\\n\\r\\n\", ",
                "\"expect_request\": {\"method\": \"OPTIONS\", \"cseq\": 1}}\n",
            ),
        )
        .unwrap();
        let fixtures = load_fixtures(dir.path()).unwrap();
        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].name, "options");
        replay(&fixtures[0]).unwrap();
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
    fn a_matching_request_and_response_replay() {
        replay(&describe_request()).unwrap();
        replay(&describe_response()).unwrap();
    }

    #[test]
    fn a_field_mismatch_is_reported() {
        let mut fixture = describe_request();
        fixture.expect_request.as_mut().unwrap().cseq = Some(99);
        assert!(matches!(
            replay(&fixture),
            Err(ReplayError::Mismatch { .. })
        ));
    }

    #[test]
    fn a_missing_header_is_reported() {
        let mut fixture = describe_request();
        fixture
            .expect_request
            .as_mut()
            .unwrap()
            .headers
            .push((String::from("x-absent"), String::from("1")));
        assert!(matches!(
            replay(&fixture),
            Err(ReplayError::Mismatch { .. })
        ));
    }

    #[test]
    fn a_fixture_without_a_wire_message_is_refused() {
        let empty = RtspFixture {
            name: String::from("empty"),
            request_wire: None,
            expect_request: None,
            response_wire: None,
            expect_response: None,
            rtspenc_key_hex: None,
        };
        assert!(matches!(replay(&empty), Err(ReplayError::Empty { .. })));
    }

    #[test]
    fn a_trailing_byte_after_the_message_fails_framing() {
        let mut fixture = describe_request();
        fixture.request_wire = Some(format!("{}X", fixture.request_wire.unwrap()));
        assert!(matches!(
            replay(&fixture),
            Err(ReplayError::Malformed { .. })
        ));
    }

    #[test]
    fn an_encrypted_capture_round_trips() {
        let mut fixture = describe_request();
        fixture.rtspenc_key_hex = Some(String::from("00112233445566778899aabbccddeeff"));
        replay(&fixture).unwrap();
    }

    #[test]
    fn a_bad_key_is_reported() {
        let mut fixture = describe_request();
        fixture.rtspenc_key_hex = Some(String::from("zz"));
        assert!(matches!(replay(&fixture), Err(ReplayError::Hex { .. })));

        let mut short = describe_request();
        short.rtspenc_key_hex = Some(String::from("0011"));
        assert!(matches!(
            replay(&short),
            Err(ReplayError::KeyLength { length: 2, .. })
        ));
    }
}
