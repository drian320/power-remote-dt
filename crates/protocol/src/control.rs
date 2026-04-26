use crate::frame::Codec;
use serde::{Deserialize, Serialize};

/// Rectangle in Windows virtual-desktop coordinate space (inclusive-left,
/// inclusive-top, exclusive-right, exclusive-bottom — matches Win32 RECT).
/// Used by HelloAck so the viewer can map local window coordinates into the
/// host's virtual desktop for MOUSEEVENTF_VIRTUALDESK injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl MonitorRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

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
        /// Rect of the monitor the host is capturing, in the host's
        /// virtual-desktop coord space.
        host_monitor_rect: MonitorRect,
        /// Bounding rect of the host's entire virtual desktop (all monitors).
        host_virtual_desktop_rect: MonitorRect,
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
    /// Noise handshake stage 1 (initiator → responder).
    NoiseE1 { payload: Vec<u8> },
    /// Noise handshake stage 2 (responder → initiator).
    NoiseE2 { payload: Vec<u8> },
    /// Bidirectional clipboard text update. Sender's clipboard just changed;
    /// receiver should update its local clipboard to match (subject to size
    /// and loop-back protection).
    ClipboardText { text: String },
    /// Begin a file transfer. Viewer → Host.
    FileTransferBegin {
        transfer_id: u64,
        filename: String,
        total_bytes: u64,
    },
    /// One chunk of file bytes.
    FileChunk {
        transfer_id: u64,
        chunk_seq: u32,
        bytes: Vec<u8>,
    },
    /// Transfer finished (success or aborted). Viewer → Host.
    FileTransferEnd { transfer_id: u64, success: bool },
    /// Viewer → Host periodic latency report. Fields mirror the viewer's
    /// `LatencyProbe` snapshot so the host side can log "what the viewer
    /// actually sees" without needing to read the viewer's stderr. The
    /// u32 widths cap individual measurements at ~71 minutes, which is
    /// fine — if glass-to-glass goes past that, a bench isn't the problem.
    LatencyReport {
        samples: u32,
        arrival_p50_us: u32,
        arrival_p95_us: u32,
        decode_p50_us: u32,
        decode_p95_us: u32,
        present_p50_us: u32,
        present_p95_us: u32,
        present_p99_us: u32,
    },
    /// Viewer → Host periodic liveness heartbeat. The host's watchdog
    /// uses the receive timestamp of these messages to decide whether
    /// the viewer is still alive. Empty payload — `Ping`/`Pong` and
    /// `LatencyReport` already carry timing data; this is purely a
    /// liveness signal that fires unconditionally every 1s.
    KeepAlive,
    /// Pre-Noise connectivity probe; both sides send these for each candidate
    /// and echo matching ProbeAck back. Used by
    /// `CustomUdpTransport::probe_and_commit_peer`.
    Probe { nonce: [u8; 16] },
    /// Reply to a Probe — echoes the received nonce back to the original sender.
    ProbeAck { nonce: [u8; 16] },
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
            Self::NoiseE1 { .. } => 10,
            Self::NoiseE2 { .. } => 11,
            Self::ClipboardText { .. } => 12,
            Self::FileTransferBegin { .. } => 13,
            Self::FileChunk { .. } => 14,
            Self::FileTransferEnd { .. } => 15,
            Self::LatencyReport { .. } => 16,
            Self::KeepAlive => 17,
            Self::Probe { .. } => 20,
            Self::ProbeAck { .. } => 21,
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
    fn noise_kinds_are_stable() {
        assert_eq!(
            ControlMessage::NoiseE1 {
                payload: vec![1, 2, 3]
            }
            .kind_u8(),
            10,
        );
        assert_eq!(
            ControlMessage::NoiseE2 {
                payload: vec![4, 5, 6]
            }
            .kind_u8(),
            11,
        );
    }

    #[test]
    fn clipboard_kind() {
        assert_eq!(
            ControlMessage::ClipboardText { text: "abc".into() }.kind_u8(),
            12,
        );
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

    #[test]
    fn probe_kinds_are_stable() {
        let p = ControlMessage::Probe { nonce: [0u8; 16] };
        assert_eq!(p.kind_u8(), 20);
        let a = ControlMessage::ProbeAck { nonce: [0u8; 16] };
        assert_eq!(a.kind_u8(), 21);
    }

    #[test]
    fn probe_roundtrip_bincode() {
        let msg = ControlMessage::Probe { nonce: [0x11; 16] };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn keep_alive_kind_is_stable() {
        assert_eq!(ControlMessage::KeepAlive.kind_u8(), 17);
    }

    #[test]
    fn keep_alive_roundtrip_bincode() {
        let msg = ControlMessage::KeepAlive;
        let bytes = bincode::serialize(&msg).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }
}
