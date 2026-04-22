use crate::frame::Codec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Viewer → Host.
    Hello {
        protocol_version: u8,
        req_width: u32,
        req_height: u32,
        req_fps: u32,
        codec: Codec,
    },
    /// Host → Viewer.
    HelloAck {
        session_id: u64,
        host_monotonic_base_us: u64,
        neg_width: u32,
        neg_height: u32,
        neg_fps: u32,
        neg_bitrate_bps: u32,
    },
    /// Bidirectional.
    Bye,
    /// Viewer → Host.
    Ping { ping_seq: u64, viewer_ts_us: u64 },
    /// Host → Viewer.
    Pong {
        ping_seq: u64,
        viewer_ts_us: u64,
        host_ts_us: u64,
    },
    /// Viewer → Host.
    RequestIdr,
    /// Bidirectional (viewer suggests, host confirms).
    SetBitrate { target_bps: u32 },
    /// Bidirectional debug channel; optional, Phase 0 not required.
    Stats {
        loss_rate_ppm: u32, // parts per million
        fps_millis: u32,    // fps * 1000
        bitrate_bps: u32,
    },
}

impl ControlMessage {
    /// Discriminant byte used in wire format (ControlPacket.control_kind).
    pub fn kind_u8(&self) -> u8 {
        match self {
            Self::Hello { .. } => 0,
            Self::HelloAck { .. } => 1,
            Self::Bye => 2,
            Self::Ping { .. } => 3,
            Self::Pong { .. } => 4,
            Self::RequestIdr => 5,
            Self::SetBitrate { .. } => 6,
            Self::Stats { .. } => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_kinds_are_stable() {
        let hello = ControlMessage::Hello {
            protocol_version: 1,
            req_width: 3840,
            req_height: 2160,
            req_fps: 60,
            codec: Codec::H265,
        };
        assert_eq!(hello.kind_u8(), 0);
        assert_eq!(ControlMessage::Bye.kind_u8(), 2);
        assert_eq!(ControlMessage::RequestIdr.kind_u8(), 5);
    }

    #[test]
    fn ping_pong_fields() {
        let p = ControlMessage::Ping {
            ping_seq: 7,
            viewer_ts_us: 1_000_000,
        };
        assert_eq!(p.kind_u8(), 3);
        if let ControlMessage::Ping {
            ping_seq,
            viewer_ts_us,
        } = p
        {
            assert_eq!(ping_seq, 7);
            assert_eq!(viewer_ts_us, 1_000_000);
        }
    }
}
