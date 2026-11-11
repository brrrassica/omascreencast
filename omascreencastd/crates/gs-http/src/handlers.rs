//! Route handlers.
//!
//! # Client correlation
//!
//! GameStream's pairing phases do not echo a host-issued id, so the daemon
//! correlates a phase request with its session by **source address**. That is
//! sound because only one client may pair at a time in practice (the panel
//! shows one prompt), and a stale session from the same address is superseded
//! by a newer one in [`gs_crypto::PairingManager::find_by_address`].
//!
//! # Idempotence
//!
//! Every phase handler tolerates a retry. A network retry of
//! `/clientchallenge` after approval returns the challenge response again
//! rather than failing out of order, because the alternative is a client that
//! cannot recover from one lost packet.

use gs_crypto::{PairingPhase, Pin};
use gs_proto::dto::{AppList, LaunchParams, LaunchResponse};
use gs_proto::xml::Element;

use crate::request::Request;
use crate::route::Route;
use crate::state::{DaemonState, HttpError};
use crate::xml;

/// A fully-formed HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// `Content-Type` header value.
    pub content_type: &'static str,
    /// Response body.
    pub body: Vec<u8>,
}

impl Response {
    /// A `200` XML response.
    #[must_use]
    pub fn xml(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: "application/xml",
            body: body.into(),
        }
    }

    /// Render an [`HttpError`] the way a captured GameStream host does.
    ///
    /// Two facts from the oracle capture shape this:
    ///
    /// 1. The HTTP status is **`200`** even for a protocol failure. The client
    ///    reads `status_code` out of the XML body, not the HTTP line.
    /// 2. The body is a `<root status_code="…" status_message="…"/>` envelope.
    ///
    /// The informational message for an internal error is withheld: a captured
    /// host does not leak one either, and it keeps the reason in the log rather
    /// than on the wire.
    #[must_use]
    pub fn error(_route: Route, error: &HttpError) -> Self {
        let status = error.status();
        // A captured host emits a bare `<root status_code="404"/>` with no
        // message for an unknown path, so the message is omitted there to stay
        // byte-faithful. Other failures carry the client-facing reason.
        let envelope = match wire_message(error) {
            Some(message) => gs_proto::wire::envelope_message(status, message),
            None => gs_proto::wire::envelope(status),
        };
        let body = match envelope.to_document() {
            Ok(body) => body,
            Err(_) => format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<root status_code=\"{status}\"/>"
            ),
        };
        Self {
            status: 200,
            content_type: "application/xml",
            body: body.into_bytes(),
        }
    }
}

/// Everything about the connection a handler is allowed to see.
#[derive(Debug, Clone, Copy)]
pub struct Peer<'a> {
    /// Source address text, used for pairing correlation and logging.
    pub address: &'a str,
    /// SHA-256 fingerprint of the client certificate, if it presented one.
    pub fingerprint: Option<&'a str>,
    /// The client certificate in DER form, if it presented one.
    pub certificate_der: Option<&'a [u8]>,
}

/// Dispatch one request.
#[must_use]
pub fn handle(state: &DaemonState, request: &Request, peer: Peer<'_>) -> Response {
    let Some(route) = Route::from_path(&request.path) else {
        return Response::error(route_placeholder(), &HttpError::NotFound);
    };
    tracing::debug!(
        method = %request.method,
        path = %request.path,
        address = %peer.address,
        user_agent = request.header("user-agent").unwrap_or("-"),
        "gamestream request"
    );
    if !state.authorize(route, peer.fingerprint) {
        return Response::error(route, &HttpError::Unauthorized);
    }
    let result = match route {
        Route::ServerInfo => server_info(state),
        Route::AppList => app_list(state),
        Route::ServerCertificate => server_certificate(state),
        Route::Pair => pair(state, request, peer),
        Route::ClientChallenge => client_challenge(state, request, peer),
        Route::ServerChallengeResponse => server_challenge_response(state, peer),
        Route::ClientPairingSecret => client_pairing_secret(state, request, peer),
        Route::Unpair => unpair(state, request),
        Route::Launch => launch(state, request),
        Route::Resume => resume(state, request),
        Route::Cancel => cancel(state),
    };
    match result {
        Ok(response) => response,
        Err(error) => Response::error(route, &error),
    }
}

/// A route for error rendering when the path did not match.
const fn route_placeholder() -> Route {
    Route::ServerInfo
}

fn server_info(state: &DaemonState) -> Result<Response, HttpError> {
    let info = state.server_info();
    Ok(xml_response(gs_proto::wire::server_info(&info)))
}

fn app_list(_state: &DaemonState) -> Result<Response, HttpError> {
    Ok(xml_response(gs_proto::wire::app_list(
        &AppList::desktop_only(),
    )))
}

fn server_certificate(state: &DaemonState) -> Result<Response, HttpError> {
    let pem = std::str::from_utf8(state.identity().certificate_pem())
        .map_err(|error| HttpError::Internal(error.to_string()))?;
    Ok(xml_response(xml::server_certificate(pem)))
}

fn pair(state: &DaemonState, request: &Request, peer: Peer<'_>) -> Result<Response, HttpError> {
    let name = request
        .query_param("devicename")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| String::from("Unknown device"));
    // Reuse the address's live session so a retried `/pair` does not fill the
    // bounded store with duplicates, then report that pairing is in progress.
    if state.session_for_address(peer.address).is_none() {
        state.begin_pairing(&name, peer.address)?;
    }
    Ok(xml_response(xml::status(true)))
}

fn client_challenge(
    state: &DaemonState,
    request: &Request,
    peer: Peer<'_>,
) -> Result<Response, HttpError> {
    let document = parse_body(request)?;
    let Some(challenge) = xml::client_challenge(&document) else {
        return Err(HttpError::BadRequest(String::from(
            "missing client challenge",
        )));
    };
    let id = ensure_session(state, peer.address)?;
    // A retry after approval already has a response; return it again.
    if let Some(response) = state.challenge_response(&id) {
        return Ok(xml_response(xml::challenge_response(
            &response.challenge,
            &response.proof,
        )));
    }
    if let Some(response) = state.accept_client_challenge(&id, challenge)? {
        return Ok(xml_response(xml::challenge_response(
            &response.challenge,
            &response.proof,
        )));
    }
    Ok(xml_response(xml::status(true)))
}

fn server_challenge_response(state: &DaemonState, peer: Peer<'_>) -> Result<Response, HttpError> {
    let Some(id) = state.session_for_address(peer.address) else {
        return Err(HttpError::BadRequest(String::from("no pairing request")));
    };
    match state.phase_of(&id) {
        Some(PairingPhase::AwaitingClientSecret) | Some(PairingPhase::Complete) => {}
        Some(PairingPhase::AwaitingApproval) => {
            return Ok(xml_response(xml::status(false)));
        }
        // A retry before the client challenge has been recorded: nothing to
        // answer yet, but not an error the client can act on.
        Some(PairingPhase::AwaitingClientChallenge) => {
            return Ok(xml_response(xml::status(false)));
        }
        None => return Err(HttpError::BadRequest(String::from("no pairing request"))),
    }
    match state.challenge_response(&id) {
        Some(response) => Ok(xml_response(xml::challenge_response(
            &response.challenge,
            &response.proof,
        ))),
        None => Ok(xml_response(xml::status(false))),
    }
}

fn client_pairing_secret(
    state: &DaemonState,
    request: &Request,
    peer: Peer<'_>,
) -> Result<Response, HttpError> {
    let document = parse_body(request)?;
    let Some(proof) = xml::client_pairing_secret(&document) else {
        return Err(HttpError::BadRequest(String::from(
            "missing client pairing secret",
        )));
    };
    let Some(id) = state.session_for_address(peer.address) else {
        return Err(HttpError::BadRequest(String::from("no pairing request")));
    };
    // Prefer the TLS certificate; fall back to one carried in the body, which
    // some clients send instead.
    let body_certificate = xml::client_certificate(&document)
        .map(str::to_owned)
        .and_then(|pem| pem_to_der(&pem));
    let certificate = peer
        .certificate_der
        .map(<[u8]>::to_vec)
        .or(body_certificate);
    state.accept_client_secret(&id, &proof, certificate.as_deref())?;
    Ok(xml_response(xml::status(true)))
}

fn unpair(state: &DaemonState, request: &Request) -> Result<Response, HttpError> {
    let Some(id) = request.query_param("uniqueid") else {
        return Err(HttpError::BadRequest(String::from("missing uniqueid")));
    };
    let removed = state.unpair(&id)?;
    Ok(xml_response(xml::status(!removed)))
}

/// `GET /launch` — create the output at the requested mode and return the RTSP
/// URL (M2-T4/T6).
///
/// The session manager clamps the mode to the server caps (D14) and creates the
/// `HEADLESS-*` output *before* this returns. A second concurrent launch is
/// refused with `409` (D17), and an output-creation failure with `503`.
fn launch(state: &DaemonState, request: &Request) -> Result<Response, HttpError> {
    let params = LaunchParams::from_pairs(request.query_pairs());
    let app_id = params.appid.unwrap_or(u32::from(gs_proto::DESKTOP_APP_ID));
    state
        .events()
        .session_request(params.mode.as_deref().unwrap_or("default"), app_id);
    state.sessions().launch(&params)?;
    Ok(xml_response(gs_proto::wire::launch_response(
        &LaunchResponse::new(
            &state.config().local_ip.to_string(),
            state.config().rtsp_port,
        ),
    )))
}

/// `GET /resume` — resume a paused session (M2-T8).
fn resume(state: &DaemonState, request: &Request) -> Result<Response, HttpError> {
    let params = LaunchParams::from_pairs(request.query_pairs());
    state.sessions().resume(&params)?;
    Ok(xml_response(gs_proto::wire::launch_response(
        &LaunchResponse::new(
            &state.config().local_ip.to_string(),
            state.config().rtsp_port,
        ),
    )))
}

/// `GET /cancel` — tear the active session down (M2-T8).
fn cancel(state: &DaemonState) -> Result<Response, HttpError> {
    state.sessions().cancel()?;
    Ok(xml_response(xml::status(true)))
}

fn ensure_session(state: &DaemonState, address: &str) -> Result<String, HttpError> {
    if let Some(id) = state.session_for_address(address) {
        return Ok(id);
    }
    // A phase request with no prior `/pair` is tolerated: some clients skip
    // that call, and refusing here would strand them with no way forward.
    Ok(state.begin_pairing("Unknown device", address)?.id)
}

fn parse_body(request: &Request) -> Result<Element, HttpError> {
    let text = std::str::from_utf8(&request.body)
        .map_err(|_| HttpError::BadRequest(String::from("body is not UTF-8")))?;
    Element::parse(text).map_err(|error| HttpError::BadRequest(error.to_string()))
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    openssl::x509::X509::from_pem(pem.as_bytes())
        .ok()
        .and_then(|certificate| certificate.to_der().ok())
}

fn xml_response(document: Element) -> Response {
    match document.to_document() {
        Ok(body) => Response::xml(body),
        Err(error) => {
            tracing::error!(%error, "could not serialise response");
            Response::error(Route::Pair, &HttpError::Internal(error.to_string()))
        }
    }
}

/// The client-facing message for an error, in the vocabulary a captured host
/// uses. Internal reasons are logged, never echoed.
///
/// `None` means the captured host sends no `status_message` at all, which is
/// what it does for an unknown path.
fn wire_message(error: &HttpError) -> Option<&str> {
    match error {
        HttpError::Unauthorized => {
            Some("The client is not authorized. Certificate verification failed.")
        }
        HttpError::BadRequest(_) => Some("Invalid request."),
        HttpError::Busy(_) => Some("The host is already streaming."),
        HttpError::Unavailable(_) => Some("The host could not start a session."),
        HttpError::NotImplemented(_) => Some("Not implemented."),
        HttpError::NotFound => None,
        HttpError::Internal(message) => {
            tracing::error!(%message, "internal error");
            Some("Internal error.")
        }
    }
}

/// Parse the four-digit PIN from an approve claim.
///
/// Exposed for the daemon's `pair` subcommand so the PIN validation rule lives
/// in one place.
///
/// # Errors
///
/// [`HttpError::BadRequest`] when the value is not four decimal digits.
pub fn parse_pin(value: &str) -> Result<Pin, HttpError> {
    Pin::parse(value).map_err(|_| {
        HttpError::BadRequest(String::from(
            "the PIN must be the four digits shown on the TV",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionError;
    use crate::state::{NullEventSink, ServerConfig};
    use gs_crypto::{Identity, TrustStore};
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// The fingerprint of the client seeded into a trusted trust store.
    const TRUSTED: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn state_with_trust() -> (TempDir, DaemonState, TrustStore) {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("host").unwrap();
        let config = ServerConfig::new(
            "host",
            DaemonState::derive_uniqueid(&identity),
            "aa:bb:cc:dd:ee:ff",
            Ipv4Addr::new(192, 168, 1, 2),
            47984,
            48010,
        );
        let mut trust = TrustStore::empty();
        let client = gs_crypto::TrustedClient::new("tv", "TV", "10.0.0.2", TRUSTED, 0);
        trust.add(client, &paths).unwrap();
        let state = DaemonState::new(
            identity,
            paths,
            config,
            Arc::new(NullEventSink),
            trust.clone(),
        );
        (dir, state, trust)
    }

    fn state() -> (TempDir, DaemonState) {
        let dir = tempfile::tempdir().unwrap();
        let paths = gs_crypto::paths_under(dir.path());
        let identity = Identity::generate("host").unwrap();
        let config = ServerConfig::new(
            "host",
            DaemonState::derive_uniqueid(&identity),
            "aa:bb:cc:dd:ee:ff",
            Ipv4Addr::new(192, 168, 1, 2),
            47984,
            48010,
        );
        let state = DaemonState::new(
            identity,
            paths,
            config,
            Arc::new(NullEventSink),
            TrustStore::empty(),
        );
        (dir, state)
    }

    /// A paired peer, so the launch route passes authorization.
    fn trusted_peer() -> Peer<'static> {
        Peer {
            address: "10.0.0.2",
            fingerprint: Some(TRUSTED),
            certificate_der: None,
        }
    }

    /// A state with one trusted client and a stub session controller.
    fn trusted_state(
        outcome: Result<crate::LaunchOutcome, SessionError>,
    ) -> (TempDir, DaemonState) {
        let (dir, state, _trust) = state_with_trust();
        let state = state.with_sessions(Arc::new(StubSessions { outcome }));
        (dir, state)
    }

    fn request(method: &str, path: &str, body: &str) -> Request {
        Request {
            method: method.to_owned(),
            path: path.to_owned(),
            query: String::new(),
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn peer(address: &'static str) -> Peer<'static> {
        Peer {
            address,
            fingerprint: None,
            certificate_der: None,
        }
    }

    #[test]
    fn serverinfo_is_xml_with_the_wire_fields() {
        let (_dir, state) = state();
        let response = handle(&state, &request("GET", "/serverinfo", ""), peer("10.0.0.2"));
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/xml");
        let text = String::from_utf8(response.body).unwrap();
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
        let doc = Element::parse(&text).unwrap();
        assert_eq!(doc.child_text("hostname"), Some("host"));
        assert_eq!(doc.child_text("PairStatus"), Some("0"));
        assert_eq!(doc.child_text("HttpsPort"), Some("47984"));
    }

    #[test]
    fn applist_has_the_single_desktop_app() {
        let (_dir, state) = state();
        let response = handle(&state, &request("GET", "/applist", ""), peer("10.0.0.2"));
        assert_eq!(response.status, 200);
        let text = String::from_utf8(response.body).unwrap();
        let doc = Element::parse(&text).unwrap();
        let app = doc.find("App").expect("one desktop app");
        assert_eq!(app.child_text("ID"), Some("1"));
    }

    #[test]
    fn getservercert_returns_the_host_certificate() {
        let (_dir, state) = state();
        let response = handle(
            &state,
            &request("GET", "/getservercert", ""),
            peer("10.0.0.2"),
        );
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn pair_opens_a_session_and_emits_a_pending_event() {
        let (_dir, state) = state();
        let mut request = request("GET", "/pair", "");
        request.query = String::from("devicename=Living%20Room%20TV");
        let response = handle(&state, &request, peer("10.0.0.2"));
        assert_eq!(response.status, 200);
        let pending = state.pending_pairings();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].name, "Living Room TV");
    }

    #[test]
    fn a_retried_pair_does_not_duplicate_the_session() {
        let (_dir, state) = state();
        let request = request("GET", "/pair", "");
        let first = handle(&state, &request, peer("10.0.0.2"));
        let second = handle(&state, &request, peer("10.0.0.2"));
        assert_eq!(first.status, 200);
        assert_eq!(second.status, 200);
        assert_eq!(state.pending_pairings().len(), 1);
    }

    #[test]
    fn client_challenge_advances_to_awaiting_approval() {
        let (_dir, state) = state();
        let body = "<root><client-challenge><challenge>0a0b</challenge></client-challenge></root>";
        let response = handle(
            &state,
            &request("POST", "/clientchallenge", body),
            peer("10.0.0.2"),
        );
        assert_eq!(response.status, 200);
        let id = state.session_for_address("10.0.0.2").unwrap();
        assert_eq!(state.phase_of(&id), Some(PairingPhase::AwaitingApproval));
    }

    #[test]
    fn launch_without_pairing_returns_a_200_xml_401_envelope() {
        // A captured host answers HTTP 200 and carries the protocol status in
        // the XML body; the client reads `status_code`, not the HTTP line.
        let (_dir, state) = state();
        let response = handle(&state, &request("GET", "/launch", ""), peer("10.0.0.2"));
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/xml");
        let text = String::from_utf8(response.body).unwrap();
        let doc = Element::parse(&text).unwrap();
        assert_eq!(
            doc.attributes,
            vec![
                (String::from("status_code"), String::from("401")),
                (
                    String::from("status_message"),
                    String::from("The client is not authorized. Certificate verification failed."),
                ),
            ]
        );
    }

    #[test]
    fn not_found_is_a_self_closing_envelope_without_a_message() {
        // Byte-faithful to the capture: `<root status_code="404"/>`, no message.
        let (_dir, state) = state();
        let response = handle(&state, &request("GET", "/nope", ""), peer("10.0.0.2"));
        let text = String::from_utf8(response.body).unwrap();
        assert_eq!(
            text,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<root status_code=\"404\"/>"
        );
    }

    #[test]
    fn launch_success_returns_the_session_url() {
        let (_dir, state) = trusted_state(Ok(crate::LaunchOutcome {
            mode: gs_proto::StreamMode {
                width: 1280,
                height: 720,
                fps: 60,
            },
            session_url: String::new(),
        }));
        let mut request = request("GET", "/launch", "");
        request.query = String::from("appid=1&mode=1280x720x60");
        let response = handle(&state, &request, trusted_peer());
        assert_eq!(response.status, 200);
        let text = String::from_utf8(response.body).unwrap();
        let doc = Element::parse(&text).unwrap();
        assert_eq!(doc.child_text("Gamesession"), Some("1"));
        assert_eq!(
            doc.child_text("SessionUrl0"),
            Some("rtsp://192.168.1.2:48010")
        );
    }

    fn protocol_status(response: &Response) -> String {
        let text = String::from_utf8(response.body.clone()).unwrap();
        let doc = Element::parse(&text).unwrap();
        doc.attributes
            .iter()
            .find(|(key, _)| key == "status_code")
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_busy_session_is_a_409_in_the_protocol_status() {
        let (_dir, state) =
            trusted_state(Err(SessionError::Busy(String::from("already streaming"))));
        let mut request = request("GET", "/launch", "");
        request.query = String::from("mode=1920x1080x60");
        let response = handle(&state, &request, trusted_peer());
        assert_eq!(response.status, 200);
        assert_eq!(protocol_status(&response), "409");
    }

    #[test]
    fn an_unavailable_session_is_a_503_in_the_protocol_status() {
        let (_dir, state) = trusted_state(Err(SessionError::Unavailable(String::from(
            "no compositor",
        ))));
        let mut request = request("GET", "/launch", "");
        request.query = String::from("mode=1920x1080x60");
        let response = handle(&state, &request, trusted_peer());
        assert_eq!(response.status, 200);
        assert_eq!(protocol_status(&response), "503");
    }

    /// A [`crate::SessionController`] with a fixed result.
    #[derive(Debug)]
    struct StubSessions {
        outcome: Result<crate::LaunchOutcome, SessionError>,
    }

    impl crate::SessionController for StubSessions {
        fn launch(
            &self,
            _params: &gs_proto::dto::LaunchParams,
        ) -> Result<crate::LaunchOutcome, SessionError> {
            self.outcome.clone()
        }

        fn cancel(&self) -> Result<(), SessionError> {
            Ok(())
        }

        fn is_streaming(&self) -> bool {
            self.outcome.is_ok()
        }
    }

    #[test]
    fn unknown_routes_are_a_404_envelope() {
        let (_dir, state) = state();
        let response = handle(&state, &request("GET", "/nope", ""), peer("10.0.0.2"));
        assert_eq!(response.status, 200);
        assert_eq!(protocol_status(&response), "404");
    }
}
