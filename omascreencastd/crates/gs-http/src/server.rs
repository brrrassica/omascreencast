//! The TLS listener.
//!
//! # Client certificates are recorded, not required
//!
//! The TLS layer runs with `SslVerifyMode::PEER` and a callback that accepts
//! every certificate. That combination is what makes the pairing flow possible:
//! the handshake completes whether or not the client has a certificate yet, and
//! the presented certificate (if any) is available to the request handler,
//! which is the layer that decides whether it is trusted.
//!
//! A client that presents *no* certificate can pair but cannot use a
//! paired-only route, because the trust decision is a fingerprint lookup and
//! `None` never matches.
//!
//! # Shutdown
//!
//! The accept loop polls a flag between accepts, exactly like the IPC server.
//! Accepted sockets get read and write timeouts so a silent connection cannot
//! pin a worker thread and delay shutdown.

use std::io::{BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gs_crypto::certificate_fingerprint;
use openssl::ssl::{SslAcceptor, SslMethod, SslVerifyMode};

use crate::handlers::{self, Peer, Response};
use crate::request::read_request;
use crate::state::DaemonState;

/// How long the accept loop sleeps between polls while shutting down.
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a connection may be silent before it is dropped.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the server could not bind or accept.
#[derive(Debug, thiserror::Error)]
pub enum HttpServerError {
    /// The listening socket could not be created.
    #[error("https listener: {0}")]
    Io(#[from] std::io::Error),
    /// The TLS context could not be built from the identity.
    #[error("tls setup: {0}")]
    Tls(#[from] openssl::error::ErrorStack),
}

/// A bound GameStream HTTPS server and its accept thread.
pub struct HttpServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    /// Bind `bind:port` and start serving `state`.
    ///
    /// Passing port `0` binds an ephemeral port; the actual address is
    /// available from [`Self::local_addr`], which is what tests use.
    ///
    /// # Errors
    ///
    /// [`HttpServerError`] when the socket or TLS context cannot be created.
    pub fn bind(bind: Ipv4Addr, port: u16, state: DaemonState) -> Result<Self, HttpServerError> {
        let acceptor = build_acceptor(&state)?;
        let listener = TcpListener::bind(SocketAddr::from((bind, port)))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name(String::from("osc-https"))
                .spawn(move || accept_loop(listener, Arc::new(acceptor), state, shutdown))?
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

    /// Stop accepting and join the accept thread.
    ///
    /// Idempotent. Connections already being served finish or hit their I/O
    /// timeout; no new ones are accepted.
    pub fn shutdown(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn build_acceptor(state: &DaemonState) -> Result<SslAcceptor, HttpServerError> {
    let mut builder = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls_server())?;
    builder.set_private_key(state.identity().key())?;
    builder.set_certificate(state.identity().certificate())?;
    builder.check_private_key()?;
    // Record the certificate; authorisation happens per route.
    builder.set_verify(SslVerifyMode::PEER);
    builder.set_verify_callback(SslVerifyMode::PEER, |_preverify_ok, _context| true);
    Ok(builder.build())
}

fn accept_loop(
    listener: TcpListener,
    acceptor: Arc<SslAcceptor>,
    state: DaemonState,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, address)) => {
                let state = state.clone();
                let acceptor = Arc::clone(&acceptor);
                // A failure to spawn must not take the listener down; the next
                // connection will try again.
                let _ = thread::Builder::new()
                    .name(String::from("osc-https-conn"))
                    .spawn(move || serve_connection(stream, address, &acceptor, &state));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

fn serve_connection(
    stream: TcpStream,
    address: SocketAddr,
    acceptor: &SslAcceptor,
    state: &DaemonState,
) {
    // Timeouts make a silent or stalled client cheap to abandon, and keep
    // shutdown bounded.
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let mut tls = match acceptor.accept(stream) {
        Ok(tls) => tls,
        Err(error) => {
            tracing::debug!(%address, %error, "tls handshake failed");
            return;
        }
    };

    let certificate_der = tls
        .ssl()
        .peer_certificate()
        .and_then(|certificate| certificate.to_der().ok());
    let fingerprint = certificate_der
        .as_deref()
        .and_then(|der| certificate_fingerprint(der).ok());
    let address_text = address.ip().to_string();

    // The request must be read from the TLS stream, not the socket beneath it:
    // the socket's bytes are still encrypted records.
    let response = {
        let mut reader = BufReader::new(&mut tls);
        match read_request(&mut reader) {
            Ok(request) => {
                let peer = Peer {
                    address: &address_text,
                    fingerprint: fingerprint.as_deref(),
                    certificate_der: certificate_der.as_deref(),
                };
                handlers::handle(state, &request, peer)
            }
            Err(error) => Response::error(
                crate::Route::ServerInfo,
                &crate::HttpError::BadRequest(error.to_string()),
            ),
        }
    };

    if write_response(&mut tls, &response).is_err() {
        tracing::debug!(%address, "failed to write response");
    }
}

fn write_response(
    tls: &mut openssl::ssl::SslStream<TcpStream>,
    response: &Response,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nServer: omascreencastd\r\n\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    )
    .into_bytes();
    head.extend_from_slice(&response.body);
    tls.write_all(&head)?;
    tls.flush()
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

/// The LAN address of the interface carrying the default route.
///
/// A UDP `connect` to a TEST-NET address picks the default-route source address
/// without sending a packet. D16 uses the same property for binding; here it is
/// only used to fill `LocalIP` and `SessionUrl0`.
#[must_use]
pub fn local_ipv4() -> Ipv4Addr {
    let Ok(socket) = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) else {
        return Ipv4Addr::LOCALHOST;
    };
    if socket
        .connect(SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9)))
        .is_err()
    {
        return Ipv4Addr::LOCALHOST;
    }
    match socket.local_addr() {
        Ok(SocketAddr::V4(address)) => *address.ip(),
        _ => Ipv4Addr::LOCALHOST,
    }
}
