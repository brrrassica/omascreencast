//! Capture backends behind one trait.
//!
//! **Scaffold only.** The trait lands in M3-T1, the `wlr-screencopy` dmabuf
//! backend in M3-T2, the shm backend in M3-T3, the test-pattern backend in
//! M2-T7, and the `wf-recorder` stopgap is removed by M3-T11 (D18).
//!
//! The contract is declared now, and deliberately, because it encodes the
//! non-negotiable pipeline rule from `plans/towards-0.2.0.md` §2.4: frames are
//! **stamped at capture** and **dropped at capture**, never queued.

#![deny(missing_docs)]

pub mod test_pattern;

pub use test_pattern::{TestPattern, TestPatternError};

use std::time::Duration;

/// A captured frame's metadata; the pixel payload lives in the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Monotonic capture timestamp. Never a send timestamp.
    pub captured_at: Duration,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

/// A capture backend.
///
/// Implementations must not buffer: if the consumer is slow, the frame is
/// dropped and the drop is counted, because a growing queue converts latency
/// into memory.
pub trait Capture: Send {
    /// The backend's error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Which path this backend ended up using, for logging (D18: the shm path
    /// must be reported as a first-class choice, not a fallback).
    fn backend_name(&self) -> &'static str;

    /// Begin capturing.
    ///
    /// # Errors
    ///
    /// Returns a backend-specific failure when the compositor refuses the
    /// capture request.
    fn start(&mut self) -> Result<(), Self::Error>;

    /// Wait for the next frame, returning `None` when the capture ended.
    ///
    /// # Errors
    ///
    /// Returns a backend-specific failure when the capture stream breaks.
    fn next_frame(&mut self) -> Result<Option<FrameMeta>, Self::Error>;

    /// Stop capturing and release compositor resources.
    ///
    /// # Errors
    ///
    /// Returns a backend-specific failure when releasing resources fails; the
    /// caller still treats the capture as stopped.
    fn stop(&mut self) -> Result<(), Self::Error>;
}
