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

/// A local operator command sent to the daemon over the **control** socket.
///
/// The event stream is daemon-to-panel only; approvals cannot come from a
/// remote client because a client must never be able to approve itself. They
/// therefore travel over a separate local socket, which the daemon binds
/// `0600` exactly like the event socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum IpcCommand {
    /// List pairing requests waiting for a PIN.
    PairingList,
    /// Approve a pending request with the PIN shown on the TV.
    PairingApprove {
        /// Pairing session id.
        id: String,
        /// The four digits the client displayed.
        pin: String,
    },
    /// Decline a pending request.
    PairingCancel {
        /// Pairing session id.
        id: String,
    },
    /// Report host and trust state.
    Status,
}

/// A pending pairing request, as shown to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    /// Pairing session id.
    pub id: String,
    /// Client-reported device name.
    pub name: String,
    /// Source address the request arrived from.
    pub address: String,
}

/// The daemon's reply to one [`IpcCommand`], one JSONL line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum CommandReply {
    /// Response to [`IpcCommand::PairingList`].
    PairingList {
        /// Every request currently waiting for a PIN.
        pending: Vec<PairingRequest>,
    },
    /// Response to [`IpcCommand::PairingApprove`] or
    /// [`IpcCommand::PairingCancel`].
    Ack {
        /// `true` when the command took effect.
        ok: bool,
        /// Human-readable reason when `ok` is `false`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Response to [`IpcCommand::Status`].
    Status {
        /// Host name advertised to clients.
        hostname: String,
        /// Whether any client is paired.
        paired: bool,
        /// Number of paired clients.
        paired_clients: usize,
        /// Number of pairing requests waiting for a PIN.
        pending: usize,
        /// Whether a stream is active.
        streaming: bool,
    },
    /// The command was not understood or could not be processed.
    Error {
        /// Human-readable reason.
        detail: String,
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

    #[cfg(test)]
    mod command_tests {
        use super::*;

        #[test]
        fn commands_match_their_wire_names() {
            assert_eq!(
                serde_json::to_value(IpcCommand::PairingList).unwrap(),
                serde_json::json!({"command": "pairing_list"})
            );
            assert_eq!(
                serde_json::to_value(IpcCommand::PairingApprove {
                    id: "9f2c".to_owned(),
                    pin: "1234".to_owned(),
                })
                .unwrap(),
                serde_json::json!({"command": "pairing_approve", "id": "9f2c", "pin": "1234"})
            );
            assert_eq!(
                serde_json::to_value(IpcCommand::PairingCancel {
                    id: "9f2c".to_owned(),
                })
                .unwrap(),
                serde_json::json!({"command": "pairing_cancel", "id": "9f2c"})
            );
            assert_eq!(
                serde_json::to_value(IpcCommand::Status).unwrap(),
                serde_json::json!({"command": "status"})
            );
        }

        #[test]
        fn replies_match_their_wire_names() {
            assert_eq!(
                serde_json::to_value(CommandReply::PairingList {
                    pending: vec![PairingRequest {
                        id: "1".to_owned(),
                        name: "TV".to_owned(),
                        address: "10.0.0.2".to_owned(),
                    }],
                })
                .unwrap(),
                serde_json::json!({
                    "reply": "pairing_list",
                    "pending": [{"id": "1", "name": "TV", "address": "10.0.0.2"}],
                })
            );
            assert_eq!(
                serde_json::to_value(CommandReply::Ack {
                    ok: true,
                    detail: None,
                })
                .unwrap(),
                serde_json::json!({"reply": "ack", "ok": true})
            );
            assert_eq!(
                serde_json::to_value(CommandReply::Status {
                    hostname: "host".to_owned(),
                    paired: false,
                    paired_clients: 0,
                    pending: 1,
                    streaming: false,
                })
                .unwrap(),
                serde_json::json!({
                    "reply": "status",
                    "hostname": "host",
                    "paired": false,
                    "paired_clients": 0,
                    "pending": 1,
                    "streaming": false,
                })
            );
        }

        #[test]
        fn commands_and_replies_round_trip() {
            let command = IpcCommand::PairingApprove {
                id: "a".to_owned(),
                pin: "0000".to_owned(),
            };
            let text = serde_json::to_string(&command).unwrap();
            assert_eq!(serde_json::from_str::<IpcCommand>(&text).unwrap(), command);

            let reply = CommandReply::Error {
                detail: "nope".to_owned(),
            };
            let text = serde_json::to_string(&reply).unwrap();
            assert_eq!(serde_json::from_str::<CommandReply>(&text).unwrap(), reply);
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
