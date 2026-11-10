//! The control channel, RTP packetisation, and pacing.
//!
//! **Scaffold only.** Lands in M3-T6 (packetiser), M3-T7 (pacer), M3-T8
//! (control channel), and M3-T9 (IDR on request). The pipeline rules in
//! `plans/towards-0.2.0.md` §2.4 are non-negotiable: stamp at capture, drop at
//! capture, pace with a token bucket, honour IDR requests out-of-band.

#![deny(missing_docs)]

/// The control-channel packet types the host must answer in 0.2.0.
///
/// Input is explicitly out of scope (D8): a type that carries input is
/// acknowledged and discarded rather than parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlPacket {
    /// Client requests a keyframe.
    RequestIdr,
    /// Client reports loss statistics.
    LossStats,
    /// Client or server terminates the session.
    Termination,
    /// Periodic keep-alive.
    Ping,
    /// Any input-bearing packet; acknowledged and discarded (D8).
    InputDiscarded,
}

impl ControlPacket {
    /// Classify an opcode. Unknown opcodes are treated as ignorable input so a
    /// newer client cannot wedge the session by sending something new.
    #[must_use]
    pub fn classify(opcode: u8) -> Self {
        match opcode {
            0x01 => Self::RequestIdr,
            0x02 => Self::LossStats,
            0x03 => Self::Termination,
            0x04 => Self::Ping,
            _ => Self::InputDiscarded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_opcodes_map_to_known_packets() {
        assert_eq!(ControlPacket::classify(0x01), ControlPacket::RequestIdr);
        assert_eq!(ControlPacket::classify(0x02), ControlPacket::LossStats);
        assert_eq!(ControlPacket::classify(0x03), ControlPacket::Termination);
        assert_eq!(ControlPacket::classify(0x04), ControlPacket::Ping);
    }

    #[test]
    fn unknown_opcodes_degrade_to_discarded_input() {
        for opcode in [0x00, 0x05, 0x10, 0xFF] {
            assert_eq!(
                ControlPacket::classify(opcode),
                ControlPacket::InputDiscarded
            );
        }
    }
}
