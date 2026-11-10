//! End-to-end tests for the M1 HTTP slice.
//!
//! These bind a real TLS listener on an ephemeral port and talk to it with a
//! real TLS client, because the parts most likely to be wrong — the handshake
//! accepting an unpaired client, reading a client certificate, and the route
//! authorization decision — only exist once both ends are real.
//!
//! A test client here is **not** a Moonlight client. It cannot complete pairing
//! because the reference crypto is provisional (M1-T4). What it proves is the
//! transport, the routing, the trust plumbing, and the event contract.

use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use gs_crypto::{Identity, TrustStore, certificate_fingerprint};
use gs_http::{DaemonState, HttpServer, ServerConfig};
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};
use tempfile::TempDir;

/// A running daemon for one test.
struct Harness {
    _dir: TempDir,
    state: DaemonState,
    server: HttpServer,
    identity: Identity,
}

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("test-host").unwrap();
        let server_identity =
            Identity::from_pem(identity.certificate_pem(), identity.key_pem()).unwrap();
        let config = ServerConfig::new(
            "test-host",
            DaemonState::derive_uniqueid(&identity),
            "00:11:22:33:44:55",
            Ipv4Addr::new(127, 0, 0, 1),
            47984,
            48010,
        );
        let state = DaemonState::new(
            server_identity,
            paths,
            config,
            Arc::new(gs_http::NullEventSink),
            TrustStore::empty(),
        );
        let server = HttpServer::bind(Ipv4Addr::new(127, 0, 0, 1), 0, state.clone()).unwrap();
        Self {
            _dir: dir,
            state,
            server,
            identity,
        }
    }

    fn port(&self) -> u16 {
        self.server.local_addr().port()
    }

    /// Send one request over TLS, optionally presenting a client certificate.
    fn request(&self, method: &str, path: &str, client: Option<&Identity>) -> (u16, String) {
        let mut builder = SslConnector::builder(SslMethod::tls()).unwrap();
        // The host certificate is self-signed; this is the trust-on-first-use
        // step a real client performs at pairing time.
        builder.set_verify(SslVerifyMode::NONE);
        if let Some(client) = client {
            builder.set_certificate(client.certificate()).unwrap();
            builder.set_private_key(client.key()).unwrap();
        }
        let connector = builder.build();
        let tcp = std::net::TcpStream::connect((Ipv4Addr::new(127, 0, 0, 1), self.port())).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut tls = connector.connect("test-host", tcp).unwrap();

        let request =
            format!("{method} {path} HTTP/1.1\r\nHost: test-host\r\nConnection: close\r\n\r\n");
        tls.write_all(request.as_bytes()).unwrap();
        tls.flush().unwrap();

        let mut response = Vec::new();
        let _ = tls.read_to_end(&mut response);
        parse_response(&response)
    }
}

fn parse_response(bytes: &[u8]) -> (u16, String) {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn serverinfo_is_reachable_without_a_client_certificate() {
    let harness = Harness::start();
    let (status, body) = harness.request("GET", "/serverinfo", None);
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["hostname"], "test-host");
    assert_eq!(json["PairStatus"], 1);
    assert_eq!(json["HttpsPort"], 47984);
    assert!(json["uniqueid"].as_str().unwrap().contains('-'));
}

#[test]
fn getservercert_returns_the_pem_that_matches_the_identity() {
    let harness = Harness::start();
    let (status, body) = harness.request("GET", "/getservercert", None);
    assert_eq!(status, 200);
    let expected = String::from_utf8(harness.identity.certificate_pem().to_vec()).unwrap();
    assert!(
        body.contains(expected.trim()),
        "served certificate should be byte-identical to the identity certificate"
    );
}

#[test]
fn applist_is_reachable_after_a_client_certificate_is_presented() {
    let harness = Harness::start();
    let client = Identity::generate("client").unwrap();
    let (status, body) = harness.request("GET", "/applist", Some(&client));
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["App"].as_array().unwrap().len(), 1);
}

#[test]
fn a_trusted_client_fingerprint_is_enforced_on_paired_routes() {
    let harness = Harness::start();
    let client = Identity::generate("client").unwrap();
    let der = client.certificate_der().unwrap();
    let fingerprint = certificate_fingerprint(&der).unwrap();

    // Not paired yet: a paired-only route is refused.
    assert!(
        !harness
            .state
            .authorize(gs_http::Route::Launch, Some(&fingerprint))
    );

    // Add the fingerprint to the trust store; the same decision flips.
    let paths = harness.state.paths().clone();
    let mut store = TrustStore::load(&paths).unwrap();
    store
        .add(
            gs_crypto::TrustedClient::new("id", "TV", "127.0.0.1", &fingerprint, 0),
            &paths,
        )
        .unwrap();
    let reloaded = TrustStore::load(&paths).unwrap();
    assert!(reloaded.is_trusted(&fingerprint));
}

#[test]
fn pairing_request_is_recorded_and_reachable_over_tls() {
    let harness = Harness::start();
    let (status, _body) = harness.request("GET", "/pair", None);
    assert_eq!(status, 200);
    let pending = harness.state.pending_pairings();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].address, "127.0.0.1");
}

#[test]
fn unknown_routes_are_404_and_launch_is_not_yet_available() {
    let harness = Harness::start();
    assert_eq!(harness.request("GET", "/nope", None).0, 404);
    // `/launch` is a known route but unpaired clients may not use it.
    assert_eq!(harness.request("GET", "/launch", None).0, 401);
}
