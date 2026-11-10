//! The session manager (M2-T4/T6/T8/T10).
//!
//! One client at a time (D17): [`SessionManager`] holds at most one
//! [`ActiveSession`], and a second `/launch` while one is live is refused with a
//! readable reason rather than interleaving two streams.
//!
//! # Why the manager is the only writer
//!
//! `gs-http` decides *when* to launch and `gs-rtsp` decides *how* to stream, but
//! both must agree on which output and which mode the session committed to.
//! That fact lives here, behind a mutex, so `/launch` creates it and the RTSP
//! loop reads it without either owning the other.
//!
//! # Teardown removes only what we created
//!
//! [`SessionManager::cancel`] removes the exact output name recorded at launch
//! and clears the slot whether or not removal succeeds. The latter is
//! deliberate: a compositor that refuses removal must not wedge the host in a
//! permanently busy state (the output becomes an orphan for M6-T1 to reconcile,
//! not a lock on the session slot).

use std::sync::{Arc, Mutex, MutexGuard};

use gs_http::{LaunchOutcome, SessionController, SessionError};
use gs_proto::ipc::{IpcEvent, phase};
use gs_proto::{StreamMode, dto::LaunchParams};

use crate::ipc::IpcHub;
use crate::output::{HyprlandOutput, OutputManager};

/// The mode used when the client omits `mode=` or sends something unparseable.
///
/// This is the D14 default, not a guess: 1080p60 is the cap and the value a
/// client is told it got when it asks for nothing in particular.
pub const DEFAULT_MODE: StreamMode = StreamMode {
    width: 1920,
    height: 1080,
    fps: 60,
};

/// The one live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    /// The `HEADLESS-*` output name this session created. Teardown removes
    /// exactly this name (M2-T8).
    pub output: String,
    /// The committed mode, already clamped (D14).
    pub mode: StreamMode,
}

/// Owns the single session slot and the output lifecycle.
#[derive(Debug)]
pub struct SessionManager {
    outputs: Arc<dyn OutputManager>,
    hub: IpcHub,
    active: Mutex<Option<ActiveSession>>,
}

impl SessionManager {
    /// Build a manager over a real `hyprctl` boundary and an event hub.
    #[must_use]
    pub fn new(hub: IpcHub) -> Self {
        Self::with_outputs(Arc::new(HyprlandOutput::new()), hub)
    }

    /// Build a manager with an explicit output boundary, for tests.
    #[must_use]
    pub fn with_outputs(outputs: Arc<dyn OutputManager>, hub: IpcHub) -> Self {
        Self {
            outputs,
            hub,
            active: Mutex::new(None),
        }
    }

    /// The active session, if any.
    #[must_use]
    pub fn active(&self) -> Option<ActiveSession> {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Option<ActiveSession>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SessionController for SessionManager {
    fn launch(&self, params: &LaunchParams) -> Result<LaunchOutcome, SessionError> {
        // Hold the slot for the whole create so two concurrent launches cannot
        // both observe an empty slot and race to create two outputs (D17).
        let mut slot = self.lock();
        if let Some(active) = slot.as_ref() {
            return Err(SessionError::Busy(format!(
                "{} is already streaming at {}; only one client may stream at a time",
                active.output,
                active.mode.to_wire()
            )));
        }
        // Reserve the slot before the (possibly slow) compositor call so a
        // second launch sees it occupied.
        let mode = params.stream_mode().unwrap_or(DEFAULT_MODE).clamp();
        let output = self.outputs.create_headless(mode).map_err(|error| {
            SessionError::Unavailable(format!("could not create the stream output: {error}"))
        })?;
        *slot = Some(ActiveSession {
            output: output.clone(),
            mode,
        });
        drop(slot);
        self.hub.emit(&IpcEvent::session_phase(
            &output,
            phase::CASTING,
            Some(format!("launch {}", mode.to_wire())),
        ));
        Ok(LaunchOutcome {
            mode,
            session_url: String::new(),
        })
    }

    fn resume(&self, params: &LaunchParams) -> Result<LaunchOutcome, SessionError> {
        // A resume while a session is live is the same session: report what it
        // already committed to rather than creating a second output.
        if let Some(active) = self.active() {
            return Ok(LaunchOutcome {
                mode: active.mode,
                session_url: String::new(),
            });
        }
        self.launch(params)
    }

    fn cancel(&self) -> Result<(), SessionError> {
        let active = self.lock().take();
        let Some(active) = active else {
            return Ok(());
        };
        let removal = self.outputs.remove_output(&active.output);
        self.hub.emit(&IpcEvent::session_phase(
            &active.output,
            phase::DISCONNECTED,
            Some(String::from("cancelled")),
        ));
        removal.map_err(|error| {
            // The slot is already cleared above: a failed removal leaves an
            // orphan for reconciliation, not a stuck session.
            SessionError::Unavailable(format!(
                "could not remove output {}: {error}",
                active.output
            ))
        })
    }

    fn is_streaming(&self) -> bool {
        self.active().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct FakeOutputs {
        created: AtomicUsize,
        removed: Mutex<Vec<String>>,
        fail: Mutex<bool>,
    }

    impl OutputManager for FakeOutputs {
        fn create_headless(&self, _mode: StreamMode) -> Result<String, crate::output::OutputError> {
            if *self.fail.lock().unwrap() {
                return Err(crate::output::OutputError::HyprlandUnavailable);
            }
            let index = self.created.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("HEADLESS-{index}"))
        }

        fn remove_output(&self, name: &str) -> Result<(), crate::output::OutputError> {
            self.removed.lock().unwrap().push(name.to_owned());
            Ok(())
        }
    }

    fn manager(fail: bool) -> (Arc<FakeOutputs>, SessionManager) {
        let outputs = Arc::new(FakeOutputs::default());
        *outputs.fail.lock().unwrap() = fail;
        let manager = SessionManager::with_outputs(outputs.clone(), IpcHub::new());
        (outputs, manager)
    }

    fn params(mode: &str) -> LaunchParams {
        LaunchParams::from_pairs([("mode", mode)])
    }

    #[test]
    fn a_launch_creates_one_output_at_the_clamped_mode() {
        let (outputs, manager) = manager(false);
        let outcome = manager.launch(&params("3840x2160x120")).unwrap();
        assert_eq!(outcome.mode.to_wire(), "1920x1080x60");
        assert_eq!(outputs.created.load(Ordering::SeqCst), 1);
        let active = manager.active().unwrap();
        assert_eq!(active.output, "HEADLESS-1");
        assert!(manager.is_streaming());
    }

    #[test]
    fn an_absent_mode_falls_back_to_the_default() {
        let (_outputs, manager) = manager(false);
        let outcome = manager.launch(&LaunchParams::default()).unwrap();
        assert_eq!(outcome.mode, DEFAULT_MODE);
    }

    #[test]
    fn a_second_launch_is_refused_while_one_is_active() {
        let (_outputs, manager) = manager(false);
        manager.launch(&params("1280x720x60")).unwrap();
        let second = manager.launch(&params("1920x1080x60"));
        assert!(matches!(second, Err(SessionError::Busy(_))));
    }

    #[test]
    fn resume_reports_the_live_session_rather_than_creating_another() {
        let (outputs, manager) = manager(false);
        let first = manager.launch(&params("1280x720x60")).unwrap();
        let resumed = manager.resume(&params("1920x1080x60")).unwrap();
        assert_eq!(resumed.mode, first.mode);
        assert_eq!(outputs.created.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn resume_starts_a_session_when_none_is_live() {
        let (outputs, manager) = manager(false);
        manager.resume(&params("1280x720x60")).unwrap();
        assert_eq!(outputs.created.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_removes_exactly_the_output_it_created_and_frees_the_slot() {
        let (outputs, manager) = manager(false);
        manager.launch(&params("1920x1080x60")).unwrap();
        manager.cancel().unwrap();
        assert_eq!(
            outputs.removed.lock().unwrap().as_slice(),
            [String::from("HEADLESS-1")]
        );
        assert!(!manager.is_streaming());
        // The slot is free again.
        assert!(manager.launch(&params("1920x1080x60")).is_ok());
    }

    #[test]
    fn cancel_with_no_session_is_a_no_op() {
        let (outputs, manager) = manager(false);
        manager.cancel().unwrap();
        assert!(outputs.removed.lock().unwrap().is_empty());
    }

    #[test]
    fn a_failed_output_creation_is_unavailable_not_busy() {
        let (_outputs, manager) = manager(true);
        let result = manager.launch(&params("1920x1080x60"));
        assert!(matches!(result, Err(SessionError::Unavailable(_))));
        // The slot never became occupied.
        assert!(!manager.is_streaming());
    }
}
