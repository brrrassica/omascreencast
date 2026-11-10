//! The daemon's IPC event vocabulary (M1-T10, plan §5.1).
//!
//! These live in `gs-proto` rather than in the daemon because they are a
//! contract between two independent consumers: the daemon writes them and the
//! panel reads them. Putting them in a protocol crate means the wire shape is
//! reviewable in one place and cannot drift into whatever the daemon found
//! convenient at the time.
//!
//! # Phase vocabulary
//!
//! The `phase` strings deliberately mirror what `Service.qml` already speaks
//! (`starting`, `casting`, `disconnected`, `error`), so the panel's existing
//! notifications and `dropPolicy` logic keep working. New phases are additive.
//!
//! # Forward compatibility
//!
//! A reader must ignore events it does not recognise. That is why the enum is
//! tagged on `event` and why unknown fields are tolerated on input: a newer
//! daemon talking to an older panel must degrade, not crash.

use serde::{Deserialize, Serialize};

/// Phase strings shared with the panel.
///
/// Kept as constants rather than an enum on the wire so a daemon can emit a
/// phase this build does not know yet without the panel rejecting the line.
pub mod phase {
    /// The daemon is coming up.
    pub const STARTING: &str = "starting";
    /// A stream is active.
    pub const CASTING: &str = "casting";
    /// No stream is active.
    pub const DISCONNECTED: &str = "disconnected";
    /// Something failed; `detail` carries the reason.
    pub const ERROR: &str = "error";
}

/// One newline-delimited JSON event on the daemon socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum IpcEvent {
    /// The session phase changed.
    ///
    /// `session` is the `HEADLESS-*` output name, absent when no session is
    /// active.
    #[serde(rename = "phase")]
    Phase {
        /// Output name, or `None` when no session is active.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// One of the [`phase`] constants, or a newer phase string.
        phase: String,
        /// Human-readable detail, for example the RTSP method reached.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A client is waiting for the operator to approve pairing.
    #[serde(rename = "pairing_pending")]
    PairingPending {
        /// Pairing session id.
        id: String,
        /// Client-reported device name.
        name: String,
        /// Source address the request arrived from.
        address: String,
    },
    /// A pairing attempt finished.
    #[serde(rename = "pairing_result")]
    PairingResult {
        /// Pairing session id that finished.
        id: String,
        /// `true` on success.
        ok: bool,
    },
    /// A client asked to start a session.
    #[serde(rename = "session_request")]
    SessionRequest {
        /// Client-requested mode, `WIDTHxHEIGHTxFPS`.
        mode: String,
        /// Requested app id.
        appid: u32,
    },
}

impl IpcEvent {
    /// Build a `phase` event with no detail.
    #[must_use]
    pub fn phase(phase: impl Into<String>) -> Self {
        Self::Phase {
            session: None,
            phase: phase.into(),
            detail: None,
        }
    }

    /// Build a `phase` event that names the active session.
    #[must_use]
    pub fn session_phase(
        session: impl Into<String>,
        phase: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self::Phase {
            session: Some(session.into()),
            phase: phase.into(),
            detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_event_matches_the_plan_contract() {
        let event = IpcEvent::session_phase("HEADLESS-1", "casting", Some("PLAY".to_owned()));
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "event": "phase",
                "session": "HEADLESS-1",
                "phase": "casting",
                "detail": "PLAY",
            })
        );
    }

    #[test]
    fn absent_optional_fields_are_omitted_not_null() {
        let json = serde_json::to_value(IpcEvent::phase(phase::DISCONNECTED)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"event": "phase", "phase": "disconnected"})
        );
        assert!(json.get("session").is_none());
        assert!(json.get("detail").is_none());
    }

    #[test]
    fn pairing_events_match_the_plan_contract() {
        assert_eq!(
            serde_json::to_value(IpcEvent::PairingPending {
                id: "9f2c".to_owned(),
                name: "Living Room TV".to_owned(),
                address: "192.168.1.40".to_owned(),
            })
            .unwrap(),
            serde_json::json!({
                "event": "pairing_pending",
                "id": "9f2c",
                "name": "Living Room TV",
                "address": "192.168.1.40",
            })
        );
        assert_eq!(
            serde_json::to_value(IpcEvent::PairingResult {
                id: "9f2c".to_owned(),
                ok: true,
            })
            .unwrap(),
            serde_json::json!({"event": "pairing_result", "id": "9f2c", "ok": true})
        );
    }

    #[test]
    fn session_request_matches_the_plan_contract() {
        assert_eq!(
            serde_json::to_value(IpcEvent::SessionRequest {
                mode: "1920x1080x60".to_owned(),
                appid: 1,
            })
            .unwrap(),
            serde_json::json!({
                "event": "session_request",
                "mode": "1920x1080x60",
                "appid": 1,
            })
        );
    }

    #[test]
    fn every_event_round_trips_through_json() {
        for event in [
            IpcEvent::phase(phase::STARTING),
            IpcEvent::session_phase("HEADLESS-1", phase::CASTING, None),
            IpcEvent::PairingPending {
                id: "1".to_owned(),
                name: "TV".to_owned(),
                address: "10.0.0.2".to_owned(),
            },
            IpcEvent::PairingResult {
                id: "1".to_owned(),
                ok: false,
            },
            IpcEvent::SessionRequest {
                mode: "1280x720x60".to_owned(),
                appid: 1,
            },
        ] {
            let text = serde_json::to_string(&event).unwrap();
            assert!(!text.contains('\n'), "an event must be one JSONL line");
            assert_eq!(serde_json::from_str::<IpcEvent>(&text).unwrap(), event);
        }
    }

    #[test]
    fn a_newer_phase_string_is_not_rejected() {
        // An older panel must survive a phase it does not know.
        let event: IpcEvent =
            serde_json::from_str(r#"{"event":"phase","phase":"reconnecting"}"#).unwrap();
        assert_eq!(event, IpcEvent::phase("reconnecting"));
    }
}
