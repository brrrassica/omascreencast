//! Daemon library surface: the IPC server, the control socket, and the pieces
//! that need unit tests.
//!
//! The binary is a thin shell over this crate. Anything with non-obvious
//! behaviour — socket lifecycle, event fan-out, stale-socket handling, pairing
//! command execution — lives here so it can be tested without spawning a
//! process.

#![deny(missing_docs)]

pub mod control;
pub mod ipc;
pub mod output;
pub mod rtsp;
pub mod session;

pub use control::{ControlError, ControlServer, control_path, control_path_for, send_command};
pub use gs_http::{DaemonState, DiscoveryServer, HttpServer, ServerConfig};
pub use ipc::{IpcError, IpcHub, IpcServer, socket_path, socket_path_for};
pub use output::{HyprlandOutput, OutputError, OutputManager};
pub use rtsp::{RtspServer, RtspServerError};
pub use session::{ActiveSession, SessionManager};

use std::sync::Arc;

use gs_crypto::PendingInfo;
use gs_http::{EventSink, HttpError};
use gs_proto::ipc::{CommandReply, IpcCommand, IpcEvent, PairingRequest};

/// Bridges pairing lifecycle events from `gs-http` onto the IPC event stream.
///
/// The event socket is the panel's contract (plan §5.1); this adapter is the
/// only place that translates the `gs-crypto` event vocabulary into it.
#[derive(Debug, Clone)]
pub struct HubEventSink {
    hub: IpcHub,
}

impl HubEventSink {
    /// Build a sink that publishes through `hub`.
    #[must_use]
    pub fn new(hub: IpcHub) -> Self {
        Self { hub }
    }
}

impl EventSink for HubEventSink {
    fn pairing_pending(&self, info: &PendingInfo) {
        self.hub.emit(&IpcEvent::PairingPending {
            id: info.id.clone(),
            name: info.name.clone(),
            address: info.address.clone(),
        });
    }

    fn pairing_result(&self, id: &str, ok: bool) {
        self.hub.emit(&IpcEvent::PairingResult {
            id: id.to_owned(),
            ok,
        });
    }

    fn session_request(&self, mode: &str, appid: u32) {
        self.hub.emit(&IpcEvent::SessionRequest {
            mode: mode.to_owned(),
            appid,
        });
    }
}

/// Build the control handler over one daemon state.
///
/// Kept as a free function so the command semantics — including the refusal to
/// approve an unknown id — are testable without a socket.
#[must_use]
pub fn command_handler(state: DaemonState) -> control::CommandHandler {
    Arc::new(move |command| execute(&state, command))
}

/// Execute one operator command against the daemon state.
#[must_use]
pub fn execute(state: &DaemonState, command: IpcCommand) -> CommandReply {
    match command {
        IpcCommand::PairingList => CommandReply::PairingList {
            pending: state
                .pending_pairings()
                .into_iter()
                .map(|info| PairingRequest {
                    id: info.id,
                    name: info.name,
                    address: info.address,
                })
                .collect(),
        },
        IpcCommand::PairingApprove { id, pin } => approve(state, &id, &pin),
        IpcCommand::PairingCancel { id } => {
            state.cancel_pairing(&id);
            CommandReply::Ack {
                ok: true,
                detail: Some(format!("declined pairing request {id}")),
            }
        }
        IpcCommand::Status => CommandReply::Status {
            hostname: state.config().hostname.clone(),
            paired: state.is_paired(),
            paired_clients: state.paired_count(),
            pending: state.pending_pairings().len(),
            streaming: state.is_streaming(),
        },
    }
}

fn approve(state: &DaemonState, id: &str, pin: &str) -> CommandReply {
    let pin = match gs_http::parse_pin(pin) {
        Ok(pin) => pin,
        Err(_) => {
            return CommandReply::Error {
                detail: String::from("the PIN must be the four digits shown on the TV"),
            };
        }
    };
    match state.approve_pairing(id, &pin) {
        Ok(_) => CommandReply::Ack {
            ok: true,
            detail: Some(format!(
                "approved {id}; finish pairing on the TV to complete it"
            )),
        },
        Err(error) => CommandReply::Error {
            detail: describe(&error),
        },
    }
}

fn describe(error: &HttpError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gs_crypto::{Identity, TrustStore};
    use std::net::Ipv4Addr;
    use tempfile::TempDir;

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
            Arc::new(gs_http::NullEventSink),
            TrustStore::empty(),
        );
        (dir, state)
    }

    #[test]
    fn status_reports_an_unpaired_idle_host() {
        let (_dir, state) = state();
        let reply = execute(&state, IpcCommand::Status);
        assert_eq!(
            reply,
            CommandReply::Status {
                hostname: String::from("host"),
                paired: false,
                paired_clients: 0,
                pending: 0,
                streaming: false,
            }
        );
    }

    #[test]
    fn pairing_list_reflects_a_pending_request() {
        let (_dir, state) = state();
        let info = state
            .begin_pairing("Living Room TV", "192.168.1.40")
            .unwrap();
        let reply = execute(&state, IpcCommand::PairingList);
        match reply {
            CommandReply::PairingList { pending } => {
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].id, info.id);
                assert_eq!(pending[0].name, "Living Room TV");
            }
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn approving_with_a_bad_pin_or_unknown_id_is_a_clear_error() {
        let (_dir, state) = state();
        let info = state.begin_pairing("TV", "10.0.0.2").unwrap();

        let bad_pin = execute(
            &state,
            IpcCommand::PairingApprove {
                id: info.id.clone(),
                pin: String::from("12"),
            },
        );
        assert!(matches!(bad_pin, CommandReply::Error { .. }));

        let unknown = execute(
            &state,
            IpcCommand::PairingApprove {
                id: String::from("nope"),
                pin: String::from("1234"),
            },
        );
        assert!(matches!(unknown, CommandReply::Error { .. }));
    }

    #[test]
    fn approving_a_live_request_succeeds() {
        let (_dir, state) = state();
        let info = state.begin_pairing("TV", "10.0.0.2").unwrap();
        let reply = execute(
            &state,
            IpcCommand::PairingApprove {
                id: info.id,
                pin: String::from("1234"),
            },
        );
        assert!(matches!(reply, CommandReply::Ack { ok: true, .. }));
    }

    #[test]
    fn cancel_removes_the_request() {
        let (_dir, state) = state();
        let info = state.begin_pairing("TV", "10.0.0.2").unwrap();
        let reply = execute(
            &state,
            IpcCommand::PairingCancel {
                id: info.id.clone(),
            },
        );
        assert!(matches!(reply, CommandReply::Ack { ok: true, .. }));
        assert!(state.pending_pairings().is_empty());
    }

    #[test]
    fn the_hub_sink_publishes_the_plan_contract_events() {
        let hub = IpcHub::new();
        let receiver = hub.subscribe();
        let sink = HubEventSink::new(hub.clone());
        sink.pairing_pending(&PendingInfo {
            id: String::from("9f2c"),
            name: String::from("Living Room TV"),
            address: String::from("192.168.1.40"),
        });
        sink.pairing_result("9f2c", true);
        assert_eq!(
            receiver.recv().unwrap(),
            IpcEvent::PairingPending {
                id: String::from("9f2c"),
                name: String::from("Living Room TV"),
                address: String::from("192.168.1.40"),
            }
        );
        assert_eq!(
            receiver.recv().unwrap(),
            IpcEvent::PairingResult {
                id: String::from("9f2c"),
                ok: true,
            }
        );
    }
}
