//! A test-pattern capture backend (M2-T7).
//!
//! This backend proves the protocol without touching Wayland: it emits a moving
//! pattern at the requested mode and rate, so `DESCRIBE`/`SETUP`/`PLAY` and the
//! packetiser can be exercised end to end on a machine with no compositor.
//!
//! It obeys the pipeline rule from `plans/towards-0.2.0.md` §2.4 that the real
//! backends must obey: frames are **stamped at capture** (the timestamp is taken
//! when the frame is produced, never when it is sent) and the backend never
//! queues. A consumer that falls behind simply misses frames; there is no
//! buffer to grow.

use std::time::{Duration, Instant};

use crate::{Capture, FrameMeta};

/// Why the test-pattern backend could not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestPatternError {
    /// A frame was requested before [`Capture::start`].
    #[error("test-pattern capture has not been started")]
    NotStarted,
    /// A dimension was zero.
    #[error("test-pattern dimensions must be non-zero, got {width}x{height}")]
    InvalidDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The framerate was zero.
    #[error("test-pattern framerate must be greater than zero")]
    InvalidFps,
}

/// A capture backend that emits a moving pattern at a fixed rate.
#[derive(Debug)]
pub struct TestPattern {
    width: u32,
    height: u32,
    interval: Duration,
    started: Option<Instant>,
    next_deadline: Duration,
    frame_index: u64,
}

impl TestPattern {
    /// Build a test-pattern backend for `width`x`height` at `fps`.
    ///
    /// # Errors
    ///
    /// [`TestPatternError::InvalidDimensions`] or
    /// [`TestPatternError::InvalidFps`] when an argument is zero.
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, TestPatternError> {
        if width == 0 || height == 0 {
            return Err(TestPatternError::InvalidDimensions { width, height });
        }
        if fps == 0 {
            return Err(TestPatternError::InvalidFps);
        }
        Ok(Self {
            width,
            height,
            interval: Duration::from_secs_f64(1.0 / f64::from(fps)),
            started: None,
            next_deadline: Duration::ZERO,
            frame_index: 0,
        })
    }

    /// The frame dimensions.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// How many frames have been produced since the last [`Capture::start`].
    #[must_use]
    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Render the current frame as a packed RGB buffer.
    ///
    /// The pattern shifts by one pixel per frame, so a decoder that is dropping
    /// or stalling frames is visible as a jump rather than a smooth scroll.
    #[must_use]
    pub fn render(&self) -> Vec<u8> {
        let width = self.width as usize;
        let height = self.height as usize;
        let mut buffer = vec![0u8; width * height * 3];
        let phase = (self.frame_index % 256) as usize;
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                buffer[index] = ((x + phase) % 256) as u8;
                buffer[index + 1] = ((y + phase) % 256) as u8;
                buffer[index + 2] = (phase % 256) as u8;
            }
        }
        buffer
    }
}

impl Capture for TestPattern {
    type Error = TestPatternError;

    fn backend_name(&self) -> &'static str {
        "test-pattern"
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        self.started = Some(Instant::now());
        self.next_deadline = self.interval;
        self.frame_index = 0;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<FrameMeta>, Self::Error> {
        let started = self.started.ok_or(TestPatternError::NotStarted)?;
        let now = started.elapsed();
        if now < self.next_deadline {
            std::thread::sleep(self.next_deadline - now);
        }
        // Stamp at capture: the timestamp is taken here, not when the frame is
        // eventually sent.
        let captured_at = started.elapsed();
        self.next_deadline += self.interval;
        self.frame_index += 1;
        Ok(Some(FrameMeta {
            captured_at,
            width: self.width,
            height: self.height,
        }))
    }

    fn stop(&mut self) -> Result<(), Self::Error> {
        self.started = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_validates_its_arguments() {
        assert!(matches!(
            TestPattern::new(0, 1080, 60),
            Err(TestPatternError::InvalidDimensions { .. })
        ));
        assert!(matches!(
            TestPattern::new(1920, 0, 60),
            Err(TestPatternError::InvalidDimensions { .. })
        ));
        assert!(matches!(
            TestPattern::new(1920, 1080, 0),
            Err(TestPatternError::InvalidFps)
        ));
    }

    #[test]
    fn frames_are_stamped_in_increasing_order() {
        let mut pattern = TestPattern::new(64, 48, 1000).unwrap();
        pattern.start().unwrap();
        let mut previous = Duration::ZERO;
        for _ in 0..3 {
            let frame = pattern.next_frame().unwrap().unwrap();
            assert_eq!(frame.width, 64);
            assert_eq!(frame.height, 48);
            assert!(frame.captured_at >= previous, "timestamps must not go back");
            previous = frame.captured_at;
        }
        assert_eq!(pattern.frame_index(), 3);
    }

    #[test]
    fn a_frame_before_start_is_an_error() {
        let mut pattern = TestPattern::new(64, 48, 60).unwrap();
        assert_eq!(pattern.next_frame(), Err(TestPatternError::NotStarted));
    }

    #[test]
    fn stopping_returns_the_backend_to_the_unstarted_state() {
        let mut pattern = TestPattern::new(64, 48, 1000).unwrap();
        pattern.start().unwrap();
        pattern.next_frame().unwrap();
        pattern.stop().unwrap();
        assert_eq!(pattern.next_frame(), Err(TestPatternError::NotStarted));
    }

    #[test]
    fn render_produces_a_packed_rgb_buffer() {
        let pattern = TestPattern::new(4, 3, 60).unwrap();
        assert_eq!(pattern.render().len(), 4 * 3 * 3);
    }

    #[test]
    fn the_pattern_moves_between_frames() {
        let mut pattern = TestPattern::new(4, 3, 1000).unwrap();
        pattern.start().unwrap();
        let first = pattern.render();
        pattern.next_frame().unwrap();
        let second = pattern.render();
        assert_ne!(first, second);
    }

    #[test]
    fn the_backend_reports_its_name() {
        let pattern = TestPattern::new(64, 48, 60).unwrap();
        assert_eq!(pattern.backend_name(), "test-pattern");
    }
}
