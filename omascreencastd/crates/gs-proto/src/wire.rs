//! GameStream XML wire documents, as captured from the Sunshine oracle.
//!
//! # Why this module exists
//!
//! The first milestone assumed `/serverinfo` and `/applist` were JSON, because
//! the M0 fake responder spoke JSON. A capture against a real Sunshine host
//! (see `omascreencastd/tests/fixtures/m0/README.md`) proved otherwise: the
//! control endpoints answer **XML**, wrapped in a `<root>` element that carries
//! a `status_code` attribute. HTTP status is `200` even for a protocol-level
//! failure; the client reads `status_code` from the body.
//!
//! ```text
//! <?xml version="1.0" encoding="utf-8"?>
//! <root status_code="200"><hostname>…</hostname>…</root>
//! ```
//!
//! This module renders that envelope from the typed DTOs. Where a shape has not
//! been captured yet (for example `/applist`), the element names mirror the
//! documented JSON field names and are marked provisional; a later capture
//! corrects them and the golden tests start asserting the real bytes.
//!
//! # Clean-room note
//!
//! Field names are the public GameStream wire contract, observed on the wire.
//! No GPL source is transcribed.

use crate::dto::{AppList, LaunchResponse, ServerInfo};
use crate::xml::Element;

/// The root element name used by every GameStream XML document.
pub const ROOT: &str = "root";

/// The success status code carried in the `status_code` attribute.
pub const STATUS_OK: u16 = 200;

/// The status code a captured host returns for an unknown path.
pub const STATUS_NOT_FOUND: u16 = 404;

/// Build a bare `<root status_code="…"/>` envelope.
#[must_use]
pub fn envelope(status: u16) -> Element {
    Element::new(ROOT).attr("status_code", status.to_string())
}

/// Build a `<root status_code="…" status_message="…"/>` envelope.
///
/// `query` is deliberately omitted: the captured host always emits it empty,
/// and echoing a client-supplied query back is a needless reflection surface.
#[must_use]
pub fn envelope_message(status: u16, message: &str) -> Element {
    envelope(status).attr("status_message", message.to_owned())
}

/// Render a `/serverinfo` response.
///
/// Field order follows the captured oracle so a human diffing a trace sees the
/// same layout. `SessionUrl0` is included even though the captured host omits
/// it: Moonlight reads it to find the RTSP endpoint, and a client tolerates the
/// extra element.
#[must_use]
pub fn server_info(info: &ServerInfo) -> Element {
    let root = envelope(STATUS_OK);
    root.child(text("hostname", &info.hostname))
        .child(text("appversion", &info.appversion))
        .child(text("GfeVersion", &info.gfe_version))
        .child(text("uniqueid", &info.uniqueid))
        .child(text("HttpsPort", info.https_port.to_string()))
        .child(text("ExternalPort", info.external_port.to_string()))
        .child(text("mac", &info.mac))
        .child(text("LocalIP", &info.local_ip))
        .child(text(
            "ServerCodecModeSupport",
            info.server_codec_mode_support.to_string(),
        ))
        .child(text("PairStatus", info.pair_status.to_string()))
        .child(text("currentgame", info.currentgame.to_string()))
        .child(text("state", &info.state))
        .child(text("SessionUrl0", &info.session_url0))
}

/// Render an `/applist` response.
///
/// **Provisional.** No capture of this endpoint exists yet (it needs a trusted
/// client certificate). The element names mirror the documented JSON fields so
/// the daemon is self-consistent until a capture pins the real shape.
#[must_use]
pub fn app_list(list: &AppList) -> Element {
    let mut root = envelope(STATUS_OK).child(text("AppState", &list.app_state));
    for app in &list.apps {
        root = root.child(
            Element::new("App")
                .child(text("AppTitle", &app.title))
                .child(text("ID", &app.id))
                .child(text("IsHdrSupported", &app.is_hdr_supported))
                .child(text("HdrSupported", app.hdr_supported.to_string())),
        );
    }
    root
}

/// Render a `/launch` or `/resume` response.
///
/// **Provisional.** The field names mirror the documented JSON response; a
/// capture from a trusted client will confirm or correct them.
#[must_use]
pub fn launch_response(response: &LaunchResponse) -> Element {
    envelope(STATUS_OK)
        .child(text("Gamesession", &response.gamesession))
        .child(text("SessionUrl0", &response.session_url0))
}

/// Render a pairing acknowledgement carrying the `paired` flag.
#[must_use]
pub fn paired(ok: bool) -> Element {
    envelope(STATUS_OK).child(text("paired", if ok { "1" } else { "0" }))
}

fn text(name: &str, value: impl Into<String>) -> Element {
    Element::with_text(name.to_owned(), value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ServerInfo {
        ServerInfo::idle(
            "E14G4-Omarchy Monitor 1",
            "953B453D-A1A2-F705-B004-86F2E320CCDA",
            "00:00:00:00:00:00",
            "127.0.0.1",
            47_984,
            48_010,
            0,
        )
    }

    #[test]
    fn server_info_renders_the_captured_envelope() {
        let xml = server_info(&info()).to_document().unwrap();
        assert!(
            xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"),
            "the document carries the captured declaration"
        );
        assert!(xml.contains(r#"<root status_code="200">"#));
        assert!(xml.contains("<hostname>E14G4-Omarchy Monitor 1</hostname>"));
        assert!(xml.contains("<HttpsPort>47984</HttpsPort>"));
        assert!(xml.contains("<PairStatus>0</PairStatus>"));
    }

    #[test]
    fn an_error_envelope_carries_a_status_code_not_an_http_status() {
        let xml = envelope_message(401, "The client is not authorized.")
            .to_document()
            .unwrap();
        assert!(xml.contains(r#"status_code="401""#));
        assert!(xml.contains(r#"status_message="The client is not authorized.""#));
    }

    #[test]
    fn app_list_renders_one_desktop_entry() {
        let xml = app_list(&AppList::desktop_only()).to_xml().unwrap();
        assert!(xml.contains("<AppTitle>Desktop</AppTitle>"));
        assert!(xml.contains("<ID>1</ID>"));
    }

    #[test]
    fn launch_response_carries_the_rtsp_url() {
        let xml = launch_response(&LaunchResponse::new("192.168.1.9", 48_010))
            .to_xml()
            .unwrap();
        assert!(xml.contains("<Gamesession>1</Gamesession>"));
        assert!(xml.contains("<SessionUrl0>rtsp://192.168.1.9:48010</SessionUrl0>"));
    }
}
