//! The session-control boundary used by `/launch`, `/resume`, and `/cancel`
//! (M2-T4/T6/T8/T10).
//!
//! `gs-http` deliberately does not know how a stream is created: creating a
//! `HEADLESS-*` output is a Hyprland concern, and binding UDP ports is an RTSP
//! concern. So the handlers talk to this trait, and the daemon supplies an
//! implementation. The default [`NullSessions`] refuses every launch, which is
//! what an M1-only build should do: `/launch` answers `503` rather than
//! pretending a session exists.
//!
//! # Why `StreamMode` is clamped here, not in the handler
//!
//! D14 says a requested mode is clamped to the server caps and the client is
//! told what it actually got. The clamp and the default-when-absent policy are
//! the session's business, so the handler forwards the raw [`LaunchParams`] and
//! trusts the returned [`LaunchOutcome::mode`].

use gs_proto::StreamMode;
use gs_proto::dto::LaunchParams;

/// What a successful launch or resume produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOutcome {
    /// The mode that was actually committed, already clamped (D14).
    pub mode: StreamMode,
    /// The RTSP URL the client should `DESCRIBE` next.
    pub session_url: String,
}

/// Why a session command failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// Another client already owns the single session slot (D17).
    #[error("{0}")]
    Busy(String),
    /// The host cannot create a session for an operational reason (for example
    /// the output could not be created).
    #[error("{0}")]
    Unavailable(String),
}

/// Creates and tears down the one active stream.
pub trait SessionController: std::fmt::Debug + Send + Sync {
    /// Start a session for `params`, creating the output before returning.
    ///
    /// # Errors
    ///
    /// [`SessionError::Busy`] when a session is already active,
    /// [`SessionError::Unavailable`] when the output cannot be created.
    fn launch(&self, params: &LaunchParams) -> Result<LaunchOutcome, SessionError>;

    /// Resume a paused session, or start one when none exists.
    ///
    /// # Errors
    ///
    /// As [`Self::launch`].
    fn resume(&self, params: &LaunchParams) -> Result<LaunchOutcome, SessionError> {
        self.launch(params)
    }

    /// Tear down the active session, if any.
    ///
    /// # Errors
    ///
    /// [`SessionError::Unavailable`] when the output cannot be removed.
    fn cancel(&self) -> Result<(), SessionError>;

    /// Whether a session is currently active.
    fn is_streaming(&self) -> bool;
}

/// A [`SessionController`] that refuses to launch anything.
///
/// Used by builds and tests that do not wire a real session manager. It is
/// explicit rather than silently succeeding so a half-built daemon is never
/// mistaken for a working one.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSessions;

impl SessionController for NullSessions {
    fn launch(&self, _params: &LaunchParams) -> Result<LaunchOutcome, SessionError> {
        Err(SessionError::Unavailable(String::from(
            "this daemon was built without a session manager",
        )))
    }

    fn cancel(&self) -> Result<(), SessionError> {
        Ok(())
    }

    fn is_streaming(&self) -> bool {
        false
    }
}
