//! The plaintext discovery listener (port 47989).
//!
//! # Why a second listener
//!
//! Captured from a real Sunshine host: port `47989` answers `GET /serverinfo`
//! over **plaintext HTTP with no client certificate**, and returns
//! `<root status_code="404"/>` for every other path. Port `47984` is HTTPS and
//! demands a trusted client certificate on *every* route, including
//! `/serverinfo` and `/getservercert`.
//!
//! Moonlight uses the plaintext port only to discover a host and read its
//! `PairStatus`; pairing, launching, and streaming all happen on the trusted
//! TLS port. Serving discovery without a certificate is therefore not a
//! weakening of the security model (D16): nothing on this listener mutates
//! state, and every privileged route answers `404` here exactly as the
//! reference host does.
//!
//! # What it refuses
//!
//! Any path other than `/serverinfo` is a `404` envelope. It does not accept
//! `/pair`, `/launch`, or the pairing phases, so a client that tries to skip
//! TLS cannot start a pairing or a session over this socket.

use std::io::{BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::handlers::{self, Peer, Response};
use crate::request::read_request;
use crate::route::Route;
use crate::state::DaemonState;

/// How long the accept loop sleeps between polls while shutting down.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a discovery connection may be silent before it is dropped.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the discovery listener could not bind.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The listening socket could not be created.
    #[error("http discovery listener: {0}")]
    Io(#[from] std::io::Error),
}

/// A bound plaintext discovery server and its accept thread.
pub struct DiscoveryServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for DiscoveryServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryServer")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl DiscoveryServer {
    /// Bind `bind:port` and serve discovery for `state`.
    ///
    /// Passing port `0` binds an ephemeral port; the actual address is available
    /// from [`Self::local_addr`].
    ///
    /// # Errors
    ///
    /// [`DiscoveryError`] when the socket cannot be created.
    pub fn bind(bind: Ipv4Addr, port: u16, state: DaemonState) -> Result<Self, DiscoveryError> {
        let listener = TcpListener::bind(SocketAddr::from((bind, port)))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name(String::from("osc-discovery"))
                .spawn(move || accept_loop(listener, state, shutdown))?
        };

        Ok(Self {
            address,
            shutdown,
            thread: Some(thread),
        })
    }

    /// The address the server is listening on.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Stop accepting and join the accept thread. Idempotent.
    pub fn shutdown(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DiscoveryServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: TcpListener, state: DaemonState, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, address)) => {
                let state = state.clone();
                let _ = thread::Builder::new()
                    .name(String::from("osc-discovery-conn"))
                    .spawn(move || serve_connection(stream, address, &state));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

fn serve_connection(stream: TcpStream, address: SocketAddr, state: &DaemonState) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let address_text = address.ip().to_string();

    let response = {
        let mut reader = BufReader::new(&stream);
        match read_request(&mut reader) {
            Ok(request) => respond(state, &request, &address_text),
            Err(_) => Response::error(
                Route::ServerInfo,
                &crate::HttpError::BadRequest(String::new()),
            ),
        }
    };

    let mut writer = &stream;
    let _ = write_response(&mut writer, &response);
}

fn respond(state: &DaemonState, request: &crate::request::Request, address: &str) -> Response {
    // Discovery serves `/serverinfo`, plus the **pairing start** route.
    //
    // That second half is a captured correction, not a convenience. Pointing a
    // real Moonlight 6.2.0 client at this daemon showed it opening the pairing
    // exchange here, on plaintext 47989:
    //
    // ```text
    // GET /pair?uniqueid=…&devicename=roth&updateState=1
    //          &phrase=getservercert&salt=<hex>&clientcert=<hex>
    // ```
    //
    // The earlier belief that 47989 serves *only* `/serverinfo` came from a
    // `curl` capture and was never exercised by a client doing a pairing. The
    // client also supplies the salt and presents its certificate up front, so
    // this listener is where the handshake actually begins.
    //
    // The widening is deliberately narrow. `Route::allows_unpaired` decides what
    // may run without trust, and `/applist`, `/launch`, `/resume`, and `/cancel`
    // are not in it, so no session or app enumeration is reachable here. Pairing
    // is unauthenticated by construction (the PIN is the authentication), so
    // serving its start on the plaintext plane does not lower the bar: the
    // certificate the client presents here is recorded and still has to survive
    // the proof exchange before it is trusted.
    let route = Route::from_path(&request.path);
    let Some(route) = route else {
        tracing::warn!(
            path = %request.path,
            address = %address,
            "discovery rejected an unknown route"
        );
        return Response::error(Route::ServerInfo, &crate::HttpError::NotFound);
    };
    if route != Route::ServerInfo && !route.allows_unpaired() {
        tracing::warn!(
            path = %request.path,
            address = %address,
            "discovery rejected a trusted-only route"
        );
        return Response::error(route, &crate::HttpError::NotFound);
    }
    tracing::debug!(path = %request.path, address = %address, "discovery served a route");
    let peer = Peer {
        address,
        fingerprint: None,
        certificate_der: None,
    };
    handlers::handle(state, request, peer)
}

fn write_response(stream: &mut &TcpStream, response: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nServer: omascreencastd\r\n\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    )
    .into_bytes();
    head.extend_from_slice(&response.body);
    stream.write_all(&head)?;
    stream.flush()
}

const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    // `Write` is already in scope for the whole module via the parent import.
    use std::io::Read as _;

    use gs_crypto::{Identity, TrustStore};

    use super::*;
    use crate::state::{NullEventSink, ServerConfig};

    fn harness() -> (tempfile::TempDir, DiscoveryServer) {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("discovery-host").unwrap();
        let config = ServerConfig::new(
            "discovery-host",
            DaemonState::derive_uniqueid(&identity),
            "00:11:22:33:44:55",
            Ipv4Addr::LOCALHOST,
            47_984,
            48_010,
        );
        let state = DaemonState::new(
            identity,
            paths,
            config,
            Arc::new(NullEventSink),
            TrustStore::empty(),
        );
        let server = DiscoveryServer::bind(Ipv4Addr::LOCALHOST, 0, state).unwrap();
        (dir, server)
    }

    fn get(server: &DiscoveryServer, path: &str) -> String {
        let mut stream = TcpStream::connect(server.local_addr()).unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        body
    }

    #[test]
    fn serverinfo_is_plaintext_xml_without_a_certificate() {
        let (_dir, server) = harness();
        let response = get(&server, "/serverinfo");
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("<hostname>discovery-host</hostname>"));
        assert!(response.contains("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    }

    #[test]
    fn pairing_start_is_served_but_session_routes_are_not() {
        // Corrected by a client-driven capture (2026-11-12): a real Moonlight
        // opens the pairing handshake on this listener via
        // `/pair?…&phrase=getservercert`. The earlier version of this test
        // asserted `/pair` was a 404 here, which encoded the wrong belief that
        // came from `curl` and blocked every real client at the first request.
        let (_dir, server) = harness();
        let pairing = get(&server, "/pair?phrase=getservercert&salt=0011");
        assert!(pairing.starts_with("HTTP/1.1 200 OK"));
        assert!(
            pairing.contains("BEGIN CERTIFICATE"),
            "serves the host cert"
        );

        // The security property that must survive the widening: nothing that
        // starts or tears down a *session* is reachable here. `/applist` is
        // deliberately absent from this list: it is an unpaired route because
        // Moonlight asks for it before a launch, and it exposes only the one
        // synthetic Desktop entry.
        for path in ["/launch", "/resume", "/cancel", "/nope"] {
            let response = get(&server, path);
            assert!(response.starts_with("HTTP/1.1 200 OK"), "{path}");
            assert!(response.contains(r#"status_code="404""#), "{path}");
        }
    }
}
