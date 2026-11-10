//! The RTSP control listener: `OPTIONS`/`DESCRIBE`/`SETUP`/`PLAY`/`TEARDOWN`
//! (M2-T4).
//!
//! # What this lands and what it does not
//!
//! This server turns the `gs-rtsp` message layer into a live control
//! conversation: it answers `DESCRIBE` with the SDP for the mode the session
//! manager committed to, binds a UDP media socket on `SETUP`, reports the port
//! back, and transitions to streaming on `PLAY`. On `PLAY` it also starts the
//! **test-pattern** capture loop (M2-T7) so pacing and frame timing are
//! exercised without a compositor.
//!
//! It deliberately does **not** packetise frames onto the UDP socket: that is
//! the RTP packetiser, M3-T6. So the capture loop paces and drops frames until
//! the packetiser lands, and the plan records M2's exit as "control flow reaches
//! `PLAY`", not "the TV shows pixels". This is the honest boundary: a test
//! pattern rendered but never packetised would look like a working stream that
//! is silently blank, which is worse than a recorded gap.
//!
//! # Encrypted RTSP
//!
//! `rtspenc` is offered by the client on `/launch` (`rikey`), and the
//! [`gs_rtsp::RtspCipher`] framing exists, but the byte-exact key derivation is
//! still provisional (M2-T2, blocked on M0-T5). This listener therefore speaks
//! plaintext RTSP and logs that an encrypted offer was seen but not honoured;
//! M6-T10 makes `rtspenc` mandatory once the capture pins the derivation.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gs_capture::{Capture, TestPattern};
use gs_http::SessionController as _;
use gs_proto::ipc::{IpcEvent, phase};
use gs_rtsp::message::parse_request;
use gs_rtsp::sdp::VideoSdp;
use gs_rtsp::{Method, Response, RtspError as MessageError};

use crate::ipc::IpcHub;
use crate::session::SessionManager;

/// How long a read may block before the connection re-checks shutdown.
pub const IO_TIMEOUT: Duration = Duration::from_millis(200);

/// How long the accept loop sleeps between polls while shutting down.
pub const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Why the RTSP listener could not start or stop.
#[derive(Debug, thiserror::Error)]
pub enum RtspServerError {
    /// The TCP socket could not be bound.
    #[error("rtsp listener: {0}")]
    Io(#[from] std::io::Error),
}

/// A bound RTSP listener and its accept thread.
pub struct RtspServer {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RtspServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtspServer")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl RtspServer {
    /// Bind `bind:port` and serve RTSP `DESCRIBE`/`SETUP`/`PLAY`.
    ///
    /// # Errors
    ///
    /// [`RtspServerError::Io`] when the socket cannot be bound.
    pub fn bind(
        bind: Ipv4Addr,
        port: u16,
        sessions: Arc<SessionManager>,
        hub: IpcHub,
    ) -> Result<Self, RtspServerError> {
        let listener = TcpListener::bind(SocketAddr::from((bind, port)))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name(String::from("osc-rtsp"))
                .spawn(move || accept_loop(listener, sessions, hub, shutdown))?
        };
        Ok(Self {
            address,
            shutdown,
            thread: Some(thread),
        })
    }

    /// The address the listener is bound to.
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

impl Drop for RtspServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    listener: TcpListener,
    sessions: Arc<SessionManager>,
    hub: IpcHub,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _address)) => {
                let sessions = Arc::clone(&sessions);
                let hub = hub.clone();
                let shutdown = Arc::clone(&shutdown);
                let _ = thread::Builder::new()
                    .name(String::from("osc-rtsp-conn"))
                    .spawn(move || serve_connection(stream, sessions, hub, shutdown));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Per-connection streaming state.
struct Connection {
    sessions: Arc<SessionManager>,
    hub: IpcHub,
    video_socket: Option<UdpSocket>,
    stop_capture: Arc<AtomicBool>,
    capture_thread: Option<JoinHandle<()>>,
}

impl Connection {
    fn new(sessions: Arc<SessionManager>, hub: IpcHub) -> Self {
        Self {
            sessions,
            hub,
            video_socket: None,
            stop_capture: Arc::new(AtomicBool::new(false)),
            capture_thread: None,
        }
    }

    fn stop_capture(&mut self) {
        self.stop_capture.store(true, Ordering::SeqCst);
        if let Some(thread) = self.capture_thread.take() {
            let _ = thread.join();
        }
        // A fresh flag is needed if PLAY comes again after TEARDOWN.
        self.stop_capture = Arc::new(AtomicBool::new(false));
    }
}

fn serve_connection(
    mut stream: TcpStream,
    sessions: Arc<SessionManager>,
    hub: IpcHub,
    shutdown: Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let mut connection = Connection::new(sessions, hub);
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => break,
        }

        loop {
            match parse_request(&buffer) {
                Ok((request, consumed)) => {
                    buffer.drain(..consumed);
                    let response = handle_request(&mut connection, &request);
                    if stream.write_all(&response.serialize()).is_err() {
                        return;
                    }
                    let _ = stream.flush();
                    if request.method == Method::Teardown {
                        connection.stop_capture();
                        return;
                    }
                }
                Err(MessageError::Incomplete) => break,
                Err(error) => {
                    let response = Response::new(400, "Bad Request");
                    let _ = stream.write_all(&response.serialize());
                    tracing::debug!(%error, "rejected malformed RTSP request");
                    connection.stop_capture();
                    return;
                }
            }
        }
    }
    connection.stop_capture();
}

fn handle_request(connection: &mut Connection, request: &gs_rtsp::Request) -> Response {
    let cseq = request.cseq();
    let with_cseq = |response: Response| match cseq {
        Some(cseq) => response.with_cseq(cseq),
        None => response,
    };

    tracing::debug!(method = %request.method.as_str(), uri = %request.uri, "rtsp request");

    match request.method {
        Method::Options => {
            let mut response = Response::ok();
            response
                .headers
                .insert("Public", "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN");
            with_cseq(response)
        }
        Method::Describe => describe(connection, with_cseq),
        Method::Setup => setup(connection, request, with_cseq),
        Method::Play => play(connection, with_cseq),
        Method::Teardown => {
            connection.stop_capture();
            let _ = connection.sessions.cancel();
            with_cseq(Response::ok())
        }
        Method::Announce => with_cseq(Response::ok()),
    }
}

fn describe(connection: &Connection, wrap: impl FnOnce(Response) -> Response) -> Response {
    let Some(active) = connection.sessions.active() else {
        return Response::new(404, "Not Found");
    };
    // The `o=` origin host is informational; the client connects to the address
    // it already used, so a loopback placeholder is honest here and the mode is
    // what actually matters.
    let sdp = VideoSdp::new("omascreencast", active.mode).to_sdp();
    let mut response = Response::ok();
    response.headers.insert("Content-Type", "application/sdp");
    response.headers.insert("Content-Base", "rtsp://localhost/");
    response.body = sdp.into_bytes();
    wrap(response)
}

fn setup(
    connection: &mut Connection,
    request: &gs_rtsp::Request,
    wrap: impl FnOnce(Response) -> Response,
) -> Response {
    if connection.video_socket.is_none() {
        match UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))) {
            Ok(socket) => {
                let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
                connection.video_socket = Some(socket);
            }
            Err(error) => {
                tracing::warn!(%error, "could not bind the video UDP socket");
                return Response::new(500, "Internal Server Error");
            }
        }
    }
    let port = connection
        .video_socket
        .as_ref()
        .and_then(|socket| socket.local_addr().ok())
        .map_or(0, |address| address.port());
    let client_port = parse_client_port(request.headers.get("transport"));
    let mut transport = format!("RTP/AVP/UDP;unicast;server_port={port}");
    if let Some(client_port) = &client_port {
        transport.push_str(";client_port=");
        transport.push_str(client_port);
    }
    let mut response = Response::ok();
    response.headers.insert("Transport", transport);
    response.headers.insert("Session", "1");
    wrap(response)
}

fn play(connection: &mut Connection, wrap: impl FnOnce(Response) -> Response) -> Response {
    let Some(active) = connection.sessions.active() else {
        return Response::new(404, "Not Found");
    };
    if connection.capture_thread.is_none() {
        let stop = Arc::clone(&connection.stop_capture);
        let hub = connection.hub.clone();
        let output = active.output.clone();
        let (width, height, fps) = (
            u32::from(active.mode.width),
            u32::from(active.mode.height),
            u32::from(active.mode.fps),
        );
        connection.capture_thread = thread::Builder::new()
            .name(String::from("osc-capture"))
            .spawn(move || capture_loop(&hub, &output, width, height, fps, &stop))
            .ok();
        connection.hub.emit(&IpcEvent::session_phase(
            &active.output,
            phase::CASTING,
            Some(String::from("PLAY")),
        ));
    }
    let mut response = Response::ok();
    response.headers.insert("Session", "1");
    wrap(response)
}

/// Pace the test pattern at the requested rate and drop frames until the RTP
/// packetiser (M3-T6) exists.
///
/// Dropping rather than buffering is the pipeline rule (§2.4): a queue would
/// convert latency into memory, and there is no consumer to send to yet.
fn capture_loop(hub: &IpcHub, output: &str, width: u32, height: u32, fps: u32, stop: &AtomicBool) {
    let mut pattern = match TestPattern::new(width, height, fps) {
        Ok(pattern) => pattern,
        Err(error) => {
            tracing::warn!(%error, "test-pattern capture could not start");
            return;
        }
    };
    if let Err(error) = pattern.start() {
        tracing::warn!(%error, "test-pattern capture could not start");
        return;
    }
    let mut dropped: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        match pattern.next_frame() {
            Ok(Some(_frame)) => {
                // No packetiser yet: the frame is paced and then dropped.
                dropped = dropped.wrapping_add(1);
                if dropped % u64::from(fps.max(1)) == 0 {
                    tracing::debug!(module = "capture", dropped, "paced frames (RTP is M3)");
                }
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, "test-pattern capture ended");
                break;
            }
        }
    }
    let _ = pattern.stop();
    hub.emit(&IpcEvent::session_phase(
        output,
        phase::DISCONNECTED,
        Some(String::from("capture stopped")),
    ));
}

/// Extract the client RTP port from a `Transport` header.
///
/// Returns the raw `client_port=` value unchanged when present so the response
/// can echo it; a client that sends none gets a transport without one, which is
/// still a valid `SETUP` reply.
fn parse_client_port(transport: Option<&str>) -> Option<String> {
    let transport = transport?;
    transport
        .split(';')
        .find_map(|part| part.trim().strip_prefix("client_port="))
        .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputError, OutputManager};
    use gs_http::SessionController;
    use gs_proto::StreamMode;
    use gs_proto::dto::LaunchParams;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct CountingOutputs {
        created: Mutex<u32>,
    }

    impl OutputManager for CountingOutputs {
        fn create_headless(&self, _mode: StreamMode) -> Result<String, OutputError> {
            let mut created = self.created.lock().unwrap();
            *created += 1;
            Ok(format!("HEADLESS-{created}"))
        }

        fn remove_output(&self, _name: &str) -> Result<(), OutputError> {
            Ok(())
        }
    }

    fn connection() -> (Arc<SessionManager>, Connection) {
        let sessions = Arc::new(SessionManager::with_outputs(
            Arc::new(CountingOutputs::default()),
            IpcHub::new(),
        ));
        let connection = Connection::new(sessions.clone(), IpcHub::new());
        (sessions, connection)
    }

    fn params() -> LaunchParams {
        LaunchParams::from_pairs([("mode", "1280x720x30")])
    }

    #[test]
    fn options_lists_the_supported_methods() {
        let (_sessions, mut connection) = connection();
        let request = gs_rtsp::Request::new(Method::Options, "*");
        let response = handle_request(&mut connection, &request);
        assert_eq!(response.status, 200);
        let public = response.headers.get("public").unwrap();
        assert!(public.contains("DESCRIBE"));
        assert!(public.contains("PLAY"));
    }

    #[test]
    fn describe_without_a_session_is_404() {
        let (_sessions, mut connection) = connection();
        let request = gs_rtsp::Request::new(Method::Describe, "rtsp://host");
        let response = handle_request(&mut connection, &request);
        assert_eq!(response.status, 404);
    }

    #[test]
    fn describe_returns_the_committed_sdp() {
        let (sessions, mut connection) = connection();
        sessions.launch(&params()).unwrap();
        let request = gs_rtsp::Request::new(Method::Describe, "rtsp://host");
        let response = handle_request(&mut connection, &request);
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("m=video 0 RTP/AVP 96"));
        assert!(body.contains("a=x-omascreencast-mode:1280x720x30"));
    }

    #[test]
    fn setup_binds_a_udp_socket_and_reports_its_port() {
        let (sessions, mut connection) = connection();
        sessions.launch(&params()).unwrap();
        let mut request = gs_rtsp::Request::new(Method::Setup, "rtsp://host/streamid=0");
        request
            .headers
            .insert("Transport", "RTP/AVP/UDP;unicast;client_port=5000-5001");
        let response = handle_request(&mut connection, &request);
        assert_eq!(response.status, 200);
        let transport = response.headers.get("transport").unwrap();
        assert!(transport.contains("client_port=5000-5001"));
        let port: u16 = transport
            .split("server_port=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(port, 0);
        assert!(connection.video_socket.is_some());
    }

    #[test]
    fn play_starts_capture_and_teardown_stops_it() {
        let (sessions, mut connection) = connection();
        sessions.launch(&params()).unwrap();
        let play = gs_rtsp::Request::new(Method::Play, "rtsp://host");
        let response = handle_request(&mut connection, &play);
        assert_eq!(response.status, 200);
        assert!(connection.capture_thread.is_some());

        let teardown = gs_rtsp::Request::new(Method::Teardown, "rtsp://host");
        let response = handle_request(&mut connection, &teardown);
        assert_eq!(response.status, 200);
        assert!(connection.capture_thread.is_none());
        // Teardown also cancels the session, freeing the slot.
        assert!(!sessions.is_streaming());
    }

    #[test]
    fn client_port_is_parsed_from_the_transport_header() {
        assert_eq!(
            parse_client_port(Some("RTP/AVP/UDP;unicast;client_port=47998-47999")),
            Some(String::from("47998-47999"))
        );
        assert_eq!(parse_client_port(Some("RTP/AVP")), None);
        assert_eq!(parse_client_port(None), None);
    }
}
