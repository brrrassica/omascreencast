//! Golden-trace replay harness (M1-T11).
//!
//! # What this is
//!
//! The M0 trace corpus (M0-T5/T7) is a set of captured GameStream exchanges.
//! This module turns a capture into an executable assertion: a fixture is one
//! HTTP request plus the response the protocol fixes for it, and [`replay`]
//! drives the real [`crate::handlers::handle`] dispatch and checks the result.
//!
//! # Why it is public
//!
//! Integration tests cannot reach private items, and a replay must exercise the
//! same dispatch the server uses rather than a re-implementation. The module is
//! therefore public but is **test support**: nothing in the daemon calls it.
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

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::handlers::{Peer, handle};
use crate::request::Request;
use crate::state::{DaemonState, ServerConfig};

/// The directory the trace corpus lives in, relative to this crate.
#[must_use]
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// The host a capture was taken against.
///
/// A fixture file may begin with a `{"server": { ... }}` line; every fixture in
/// the file is then replayed against a state built from this description, so
/// the asserted fields (hostname, ports) match the capture instead of a
/// hard-coded test host.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerSpec {
    /// Hostname the capture reported.
    pub hostname: String,
    /// Host MAC address, lowercase colon-separated.
    #[serde(default)]
    pub mac: Option<String>,
    /// LAN address the capture reported.
    #[serde(default)]
    pub local_ip: Option<String>,
    /// HTTPS port the capture reported.
    #[serde(default)]
    pub https_port: Option<u16>,
    /// RTSP port the capture reported.
    #[serde(default)]
    pub rtsp_port: Option<u16>,
}

impl ServerSpec {
    /// Build a [`ServerConfig`] from this spec.
    ///
    /// `uniqueid` is supplied by the caller because it is derived from the
    /// server certificate, which a capture cannot reproduce.
    #[must_use]
    pub fn to_config(&self, uniqueid: impl Into<String>) -> ServerConfig {
        ServerConfig::new(
            self.hostname.clone(),
            uniqueid,
            self.mac
                .clone()
                .unwrap_or_else(|| String::from("00:11:22:33:44:55")),
            self.local_ip
                .as_deref()
                .and_then(|ip| ip.parse().ok())
                .unwrap_or(Ipv4Addr::LOCALHOST),
            self.https_port.unwrap_or(47984),
            self.rtsp_port.unwrap_or(48010),
        )
    }
}

/// One captured HTTP exchange.
#[derive(Debug, Clone, Deserialize)]
pub struct HttpFixture {
    /// Human-readable name, used in failure messages.
    pub name: String,
    /// The captured request.
    pub request: FixtureRequest,
    /// The response the protocol fixes for that request.
    pub expect: ExpectedResponse,
}

/// A captured request.
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRequest {
    /// HTTP method, e.g. `GET`.
    pub method: String,
    /// Path with the query removed.
    pub path: String,
    /// Raw query string, without the leading `?`.
    #[serde(default)]
    pub query: String,
    /// Headers as `[name, value]` pairs.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Request body as UTF-8 text.
    #[serde(default)]
    pub body: String,
    /// Source address used for pairing correlation.
    #[serde(default)]
    pub address: Option<String>,
    /// SHA-256 fingerprint of the client certificate, if one was presented.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Client certificate in DER form, hex-encoded, if one was presented.
    #[serde(default)]
    pub certificate_der_hex: Option<String>,
}

/// The response a fixture asserts.
///
/// Every field is optional except `status`, so a capture can assert exactly the
/// parts the protocol fixes and leave volatile values (a derived `uniqueid`, a
/// random challenge) unasserted.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedResponse {
    /// Required HTTP status code.
    ///
    /// A captured GameStream host answers `200` even for a protocol failure; the
    /// real result lives in [`Self::xml`]'s `status_code` attribute.
    pub status: u16,
    /// Required `Content-Type`, when the protocol fixes it.
    #[serde(default)]
    pub content_type: Option<String>,
    /// A JSON value the response body must contain (subset match).
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    /// XML assertions for the body, the format the captured host speaks.
    #[serde(default)]
    pub xml: Option<XmlExpectation>,
    /// An exact body match, when the protocol fixes the bytes.
    #[serde(default)]
    pub body: Option<String>,
}

/// Assertions against an XML response body.
///
/// The body is parsed with the same [`gs_proto::xml::Element`] model the daemon
/// uses, then matched structurally. That keeps a fixture readable: it records
/// the values that matter (`status_code`, a hostname, a child element) without
/// pinning incidental whitespace or field order.
#[derive(Debug, Clone, Deserialize)]
pub struct XmlExpectation {
    /// Required `status_code` attribute on the root element.
    #[serde(default)]
    pub status_code: Option<u16>,
    /// Required `status_message` attribute, when the protocol fixes it.
    #[serde(default)]
    pub status_message: Option<String>,
    /// Child elements whose text must match, as `name: text` pairs.
    #[serde(default)]
    pub children: Vec<(String, String)>,
    /// Child elements that must be present, without asserting their text.
    #[serde(default)]
    pub present: Vec<String>,
}

/// One fixture file: an optional host description and its ordered fixtures.
#[derive(Debug, Clone)]
pub struct FixtureFile {
    /// The file the fixtures came from.
    pub path: PathBuf,
    /// The host the capture was taken against, if the file declared one.
    pub server: Option<ServerSpec>,
    /// The fixtures, in file order.
    pub fixtures: Vec<HttpFixture>,
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
    /// The response did not match the fixture.
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
pub fn load_fixtures(dir: &Path) -> Result<Vec<FixtureFile>, ReplayError> {
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

    let mut files = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| ReplayError::Io {
            path: path.clone(),
            source,
        })?;
        let mut server = None;
        let mut fixtures = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_str(line).map_err(|source| ReplayError::Parse {
                    path: path.clone(),
                    line: index + 1,
                    source,
                })?;
            if let Some(spec) = value.get("server") {
                server = Some(serde_json::from_value(spec.clone()).map_err(|source| {
                    ReplayError::Parse {
                        path: path.clone(),
                        line: index + 1,
                        source,
                    }
                })?);
                continue;
            }
            let fixture: HttpFixture =
                serde_json::from_value(value).map_err(|source| ReplayError::Parse {
                    path: path.clone(),
                    line: index + 1,
                    source,
                })?;
            fixtures.push(fixture);
        }
        files.push(FixtureFile {
            path,
            server,
            fixtures,
        });
    }
    Ok(files)
}

/// Replay one fixture against `state` and assert the response.
///
/// # Errors
///
/// [`ReplayError`] when a hex field is malformed or the response differs from
/// the fixture's expectation.
pub fn replay(state: &DaemonState, fixture: &HttpFixture) -> Result<(), ReplayError> {
    let certificate_der = match &fixture.request.certificate_der_hex {
        Some(hex) => Some(crate::xml::hex_decode(hex).map_err(|_| ReplayError::Hex {
            name: fixture.name.clone(),
            field: "certificate_der_hex",
        })?),
        None => None,
    };
    let request = Request {
        method: fixture.request.method.to_ascii_uppercase(),
        path: fixture.request.path.clone(),
        query: fixture.request.query.clone(),
        headers: fixture
            .request
            .headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect(),
        body: fixture.request.body.as_bytes().to_vec(),
    };
    let peer = Peer {
        address: fixture.request.address.as_deref().unwrap_or("127.0.0.1"),
        fingerprint: fixture.request.fingerprint.as_deref(),
        certificate_der: certificate_der.as_deref(),
    };

    let response = handle(state, &request, peer);

    if response.status != fixture.expect.status {
        return Err(ReplayError::Mismatch {
            name: fixture.name.clone(),
            detail: format!(
                "status {} != expected {}",
                response.status, fixture.expect.status
            ),
        });
    }
    if let Some(expected) = &fixture.expect.content_type {
        if response.content_type != expected {
            return Err(ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail: format!(
                    "content-type {:?} != expected {:?}",
                    response.content_type, expected
                ),
            });
        }
    }
    if let Some(expected) = &fixture.expect.body {
        let actual = String::from_utf8_lossy(&response.body);
        if actual != expected.as_str() {
            return Err(ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail: format!("body {actual:?} != expected {expected:?}"),
            });
        }
    }
    if let Some(expected) = &fixture.expect.xml {
        let text = String::from_utf8_lossy(&response.body);
        let document =
            gs_proto::xml::Element::parse(&text).map_err(|error| ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail: format!("response body is not XML: {error}"),
            })?;
        if let Some(detail) = xml_mismatch(expected, &document) {
            return Err(ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail,
            });
        }
    }
    if let Some(expected) = &fixture.expect.json {
        let actual: serde_json::Value =
            serde_json::from_slice(&response.body).map_err(|error| ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail: format!("response body is not JSON: {error}"),
            })?;
        if let Some(detail) = json_subset_mismatch(expected, &actual) {
            return Err(ReplayError::Mismatch {
                name: fixture.name.clone(),
                detail,
            });
        }
    }
    Ok(())
}

/// Replay every fixture in order against one state.
///
/// # Errors
///
/// The first [`ReplayError`] encountered.
pub fn replay_all(state: &DaemonState, fixtures: &[HttpFixture]) -> Result<(), ReplayError> {
    for fixture in fixtures {
        replay(state, fixture)?;
    }
    Ok(())
}

/// Describe the first place an XML assertion does not hold, if any.
fn xml_mismatch(expected: &XmlExpectation, document: &gs_proto::xml::Element) -> Option<String> {
    if let Some(status) = expected.status_code {
        let actual = document
            .attributes
            .iter()
            .find(|(key, _)| key == "status_code")
            .map(|(_, value)| value.as_str());
        if actual != Some(status.to_string().as_str()) {
            return Some(format!("status_code {actual:?} != expected {status}"));
        }
    }
    if let Some(message) = &expected.status_message {
        let actual = document
            .attributes
            .iter()
            .find(|(key, _)| key == "status_message")
            .map(|(_, value)| value.as_str());
        if actual != Some(message.as_str()) {
            return Some(format!("status_message {actual:?} != expected {message:?}"));
        }
    }
    for name in &expected.present {
        if document.find(name).is_none() {
            return Some(format!("missing element {name:?}"));
        }
    }
    for (name, value) in &expected.children {
        match document.child_text(name) {
            Some(actual) if actual == value => {}
            Some(actual) => {
                return Some(format!(
                    "element {name:?} is {actual:?} != expected {value:?}"
                ));
            }
            None => return Some(format!("missing element {name:?}")),
        }
    }
    None
}

/// Describe the first place `expected` is not a subset of `actual`.
///
/// Objects match when every expected key is present and matches; arrays match
/// element-wise and must be the same length; scalars must be equal.
fn json_subset_mismatch(
    expected: &serde_json::Value,
    actual: &serde_json::Value,
) -> Option<String> {
    use serde_json::Value;
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, value) in expected {
                match actual.get(key) {
                    Some(actual_value) => {
                        if let Some(detail) = json_subset_mismatch(value, actual_value) {
                            return Some(format!("{key}: {detail}"));
                        }
                    }
                    None => return Some(format!("missing key {key:?}")),
                }
            }
            None
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                return Some(format!(
                    "array length {} != expected {}",
                    actual.len(),
                    expected.len()
                ));
            }
            for (index, (expected_value, actual_value)) in expected.iter().zip(actual).enumerate() {
                if let Some(detail) = json_subset_mismatch(expected_value, actual_value) {
                    return Some(format!("[{index}]: {detail}"));
                }
            }
            None
        }
        (expected, actual) => {
            if expected == actual {
                None
            } else {
                Some(format!("{actual} != expected {expected}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use gs_crypto::{Identity, TrustStore};

    fn state() -> (tempfile::TempDir, DaemonState) {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("golden-host").unwrap();
        let config = ServerConfig::new(
            "golden-host",
            DaemonState::derive_uniqueid(&identity),
            "00:11:22:33:44:55",
            Ipv4Addr::LOCALHOST,
            47984,
            48010,
        );
        let state = DaemonState::new(
            identity,
            paths,
            config,
            Arc::new(crate::NullEventSink),
            TrustStore::empty(),
        );
        (dir, state)
    }

    fn fixture(name: &str, path: &str, expect: ExpectedResponse) -> HttpFixture {
        HttpFixture {
            name: name.to_owned(),
            request: FixtureRequest {
                method: String::from("GET"),
                path: path.to_owned(),
                query: String::new(),
                headers: Vec::new(),
                body: String::new(),
                address: None,
                fingerprint: None,
                certificate_der_hex: None,
            },
            expect,
        }
    }

    fn xml_expectation(
        status: u16,
        status_code: u16,
        children: &[(&str, &str)],
    ) -> ExpectedResponse {
        ExpectedResponse {
            status,
            content_type: Some(String::from("application/xml")),
            json: None,
            xml: Some(XmlExpectation {
                status_code: Some(status_code),
                status_message: None,
                children: children
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                present: Vec::new(),
            }),
            body: None,
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
    fn a_server_directive_and_fixtures_parse_from_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"server\": {\"hostname\": \"captured-host\", \"https_port\": 47984}}\n",
                "{\"name\": \"serverinfo\", \"request\": {\"method\": \"GET\", \"path\": \"/serverinfo\"}, ",
                "\"expect\": {\"status\": 200, \"json\": {\"hostname\": \"captured-host\"}}}\n",
            ),
        )
        .unwrap();
        let files = load_fixtures(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].fixtures.len(), 1);
        let spec = files[0].server.as_ref().unwrap();
        assert_eq!(spec.hostname, "captured-host");
        assert_eq!(spec.to_config("id").hostname, "captured-host");
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
    fn a_matching_xml_fixture_replays_and_a_mismatch_is_reported() {
        let (_dir, state) = state();
        let ok = fixture(
            "serverinfo",
            "/serverinfo",
            xml_expectation(
                200,
                200,
                &[("hostname", "golden-host"), ("PairStatus", "1")],
            ),
        );
        replay(&state, &ok).unwrap();

        // The HTTP status no longer decides success: a protocol failure is a
        // 200 with a non-200 `status_code` in the body.
        let not_found = fixture("not-found", "/nope", xml_expectation(200, 404, &[]));
        replay(&state, &not_found).unwrap();

        let wrong_child = fixture(
            "wrong",
            "/serverinfo",
            xml_expectation(200, 200, &[("hostname", "someone-else")]),
        );
        assert!(matches!(
            replay(&state, &wrong_child),
            Err(ReplayError::Mismatch { .. })
        ));
    }

    #[test]
    fn json_subset_matching_is_recursive_and_length_aware() {
        let actual = serde_json::json!({"a": 1, "b": {"c": [1, 2]}});
        assert!(json_subset_mismatch(&serde_json::json!({"a": 1}), &actual).is_none());
        assert!(json_subset_mismatch(&serde_json::json!({"b": {"c": [1, 2]}}), &actual).is_none());
        assert!(json_subset_mismatch(&serde_json::json!({"a": 2}), &actual).is_some());
        assert!(json_subset_mismatch(&serde_json::json!({"z": 1}), &actual).is_some());
        assert!(json_subset_mismatch(&serde_json::json!({"b": {"c": [1]}}), &actual).is_some());
    }
}
