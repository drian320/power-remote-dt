use crate::frame::Codec;
use serde::{Deserialize, Serialize};

/// Maximum byte length of `Hello.auth_payload`. Tofu sends empty; Pin and
/// Ephemeral send at most this many bytes. The T3 AuthValidator enforces this
/// cap server-side; producers must not exceed it.
pub const MAX_AUTH_PAYLOAD_BYTES: usize = 64;

/// Authentication method the viewer asserts in `Hello`. The host's configured
/// `AuthMode` is authoritative — a mismatch responds with
/// `HelloReject { PinRequired | EphemeralRequired }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum AuthMethod {
    /// Trust-on-first-use: no credential, host decides on consent.
    Tofu = 0,
    /// PIN-based: `auth_payload` carries the PIN bytes.
    Pin = 1,
    /// Ephemeral-token: `auth_payload` carries the one-time token bytes.
    Ephemeral = 2,
}

/// Per-session permissions granted by the host in `HelloAck`.
///
/// Security invariant: `Default::default() == deny_all()`. Any new field must
/// also default to the most-restrictive value so that legacy `KnownPeer`
/// entries loaded without this field (via `#[serde(default)]`) trigger
/// re-approval rather than silently gaining permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSet {
    pub input: bool,
    pub clipboard: bool,
    pub file_transfer: bool,
    pub audio: bool,
}

impl Default for PermissionSet {
    /// Returns `deny_all()` — intentional security default so that legacy
    /// KnownPeer entries (loaded without a permissions field) always require
    /// re-approval rather than silently regaining access.
    fn default() -> Self {
        Self::deny_all()
    }
}

impl PermissionSet {
    pub const fn all() -> Self {
        Self {
            input: true,
            clipboard: true,
            file_transfer: true,
            audio: true,
        }
    }
    pub const fn view_only() -> Self {
        Self {
            input: false,
            clipboard: false,
            file_transfer: false,
            audio: true,
        }
    }
    pub const fn deny_all() -> Self {
        Self {
            input: false,
            clipboard: false,
            file_transfer: false,
            audio: false,
        }
    }
}

/// Machine-readable reason a `HelloReject` was sent. Allows the viewer to
/// present targeted UX (PIN dialog, upgrade prompt, etc.) without parsing
/// the human-readable `reason` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum HelloRejectCode {
    Unspecified = 0,
    ProtocolVersionMismatch = 1,
    UnsupportedCodec = 2,
    PinRequired = 3,
    EphemeralRequired = 4,
    AuthFailed = 5,
    AuthLockout = 6,
    ConsentDenied = 7,
}

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

/// Tightly packed BGRA8 cursor bitmap carried inside
/// [`ControlMessage::CursorUpdate`]. `width == 0 && height == 0` means
/// "cursor invisible" — viewer hides compositing but does NOT show the
/// OS-native cursor inside the capture region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBitmap {
    pub width: u16,
    pub height: u16,
    /// BGRA8 tightly packed: `len() == width * height * 4` when both
    /// dimensions are non-zero, else `Vec::new()`.
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Viewer → Host.
    Hello {
        protocol_version: u8,
        req_width: u32,
        req_height: u32,
        req_fps: u32,
        /// Post-Phase-0 semantics: this is the codec the host has been told
        /// to negotiate for. Pre-Phase-0 the viewer set it to its preferred
        /// codec. The field name is kept (not renamed) so existing wire
        /// captures still decode; the host now interprets it as a
        /// negotiation request, not a viewer-side selection. The host
        /// replies with HelloReject if the codec is not in its supported
        /// set.
        codec: Codec,
        /// P6: authentication method the viewer is using.
        auth_method: AuthMethod,
        /// P6: opaque authentication payload.
        /// Tofu ⇒ empty; Pin ⇒ PIN bytes; Ephemeral ⇒ token bytes.
        /// Must not exceed `MAX_AUTH_PAYLOAD_BYTES`. The T3 AuthValidator
        /// enforces this cap and rejects oversized payloads.
        auth_payload: Vec<u8>,
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
        /// Codec the host has chosen for this session. Always one of
        /// `host_supported_codecs`. Post-Phase-0 the producer/consumer
        /// dispatch on this value.
        negotiated_codec: Codec,
        /// Full set of codecs the host can drive. The viewer uses this for
        /// `--codec auto` fallback selection in later phases.
        host_supported_codecs: Vec<Codec>,
        /// P6: permissions granted to this viewer session.
        granted_permissions: PermissionSet,
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
    /// Host → Viewer. Cursor metadata update for cursor_mode=Metadata path.
    ///
    /// Sent only when negotiated protocol_version >= 4 AND the host's
    /// capture backend supports `SPA_META_Cursor` extraction (Linux portal
    /// only; Windows DXGI bakes the cursor into the frame and never emits
    /// this variant).
    ///
    /// Coordinates are in capture-region-relative LOGICAL pixels; the
    /// origin is the top-left of `HelloAck.host_monitor_rect`. May fall
    /// outside the rect when the cursor is at an edge; viewer clamps.
    ///
    /// `bitmap == None` is a position-only update (viewer reuses cached
    /// bitmap). `bitmap == Some { width: 0, height: 0, .. }` is "cursor
    /// invisible". Otherwise `bitmap.bgra.len() == width * height * 4`
    /// (decode validator enforces).
    CursorUpdate {
        id: u32,
        position_x: i32,
        position_y: i32,
        hotspot_x: i32,
        hotspot_y: i32,
        bitmap: Option<CursorBitmap>,
    },
    /// Pre-Noise connectivity probe; both sides send these for each candidate
    /// and echo matching ProbeAck back. Used by
    /// `CustomUdpTransport::probe_and_commit_peer`.
    Probe { nonce: [u8; 16] },
    /// Reply to a Probe — echoes the received nonce back to the original sender.
    ProbeAck { nonce: [u8; 16] },
    /// Host → Viewer. Sent in response to a Hello whose requested codec is
    /// not in the host's supported set, or whose protocol_version is
    /// otherwise incompatible. The viewer should surface `reason` and exit.
    HelloReject {
        reason: String,
        /// P6: machine-readable rejection code.
        code: HelloRejectCode,
    },
    // DO NOT INSERT VARIANTS ABOVE THIS LINE — bincode discriminants are wire-stable
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
            Self::CursorUpdate { .. } => 18,
            Self::Probe { .. } => 20,
            Self::ProbeAck { .. } => 21,
            Self::HelloReject { .. } => 22,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_kinds_are_stable() {
        let hello = ControlMessage::Hello {
            protocol_version: 3,
            req_width: 3840,
            req_height: 2160,
            req_fps: 60,
            codec: Codec::H265,
            auth_method: AuthMethod::Tofu,
            auth_payload: vec![],
        };
        assert_eq!(hello.kind_u8(), 0);
        assert_eq!(ControlMessage::Bye.kind_u8(), 2);
        assert_eq!(ControlMessage::RequestIdr.kind_u8(), 5);
    }

    #[test]
    fn helloreject_kind_is_stable() {
        let r = ControlMessage::HelloReject {
            reason: "nope".into(),
            code: HelloRejectCode::Unspecified,
        };
        assert_eq!(r.kind_u8(), 22);
    }

    #[test]
    fn helloreject_round_trip() {
        let msg = ControlMessage::HelloReject {
            reason: "host does not support h264".to_string(),
            code: HelloRejectCode::Unspecified,
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn helloack_negotiated_codec_round_trip() {
        let msg = ControlMessage::HelloAck {
            session_id: 0xCAFEBABE,
            host_monotonic_base_us: 42,
            neg_width: 1920,
            neg_height: 1080,
            neg_fps: 60,
            neg_bitrate_bps: 30_000_000,
            host_monitor_rect: MonitorRect::new(0, 0, 1920, 1080),
            host_virtual_desktop_rect: MonitorRect::new(0, 0, 1920, 1080),
            negotiated_codec: Codec::H264,
            host_supported_codecs: vec![Codec::H265, Codec::H264],
            granted_permissions: PermissionSet::all(),
        };
        let bytes = bincode::serialize(&msg).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn auth_method_round_trip() {
        for m in [AuthMethod::Tofu, AuthMethod::Pin, AuthMethod::Ephemeral] {
            let bytes = bincode::serialize(&m).unwrap();
            let back: AuthMethod = bincode::deserialize(&bytes).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn auth_method_discriminants_stable() {
        // bincode 1.3 fixint encodes variant_index as u32 LE (4 bytes).
        // repr(u8) affects in-memory ABI only, not wire bytes.
        // If bincode is ever upgraded to v2 (which honours repr(u8)),
        // these assertions will fire loudly, warning of a wire-format change.
        let tofu = bincode::serialize(&AuthMethod::Tofu).unwrap();
        assert_eq!(
            tofu.len(),
            4,
            "bincode 1.3 fixint encodes variant_index as u32"
        );
        assert_eq!(tofu, [0, 0, 0, 0]);

        let pin = bincode::serialize(&AuthMethod::Pin).unwrap();
        assert_eq!(pin.len(), 4);
        assert_eq!(pin, [1, 0, 0, 0]);

        let eph = bincode::serialize(&AuthMethod::Ephemeral).unwrap();
        assert_eq!(eph.len(), 4);
        assert_eq!(eph, [2, 0, 0, 0]);
    }

    #[test]
    fn permission_set_round_trip_and_constructors() {
        let s = PermissionSet {
            input: true,
            clipboard: false,
            file_transfer: true,
            audio: false,
        };
        let bytes = bincode::serialize(&s).unwrap();
        let back: PermissionSet = bincode::deserialize(&bytes).unwrap();
        assert_eq!(s, back);

        let all = PermissionSet::all();
        assert!(all.input && all.clipboard && all.file_transfer && all.audio);

        let vo = PermissionSet::view_only();
        assert!(!vo.input && !vo.clipboard && !vo.file_transfer && vo.audio);

        let deny = PermissionSet::deny_all();
        assert!(!deny.input && !deny.clipboard && !deny.file_transfer && !deny.audio);
    }

    #[test]
    fn hello_reject_code_round_trip_and_discriminants() {
        // bincode 1.3 fixint: variant_index is u32 LE (4 bytes), not u8.
        // Asserts both the index value and the exact 4-byte encoding so that
        // a bincode v2 upgrade (which would honour repr(u8) → 1 byte) fires.
        let codes: &[(HelloRejectCode, [u8; 4])] = &[
            (HelloRejectCode::Unspecified, [0, 0, 0, 0]),
            (HelloRejectCode::ProtocolVersionMismatch, [1, 0, 0, 0]),
            (HelloRejectCode::UnsupportedCodec, [2, 0, 0, 0]),
            (HelloRejectCode::PinRequired, [3, 0, 0, 0]),
            (HelloRejectCode::EphemeralRequired, [4, 0, 0, 0]),
            (HelloRejectCode::AuthFailed, [5, 0, 0, 0]),
            (HelloRejectCode::AuthLockout, [6, 0, 0, 0]),
            (HelloRejectCode::ConsentDenied, [7, 0, 0, 0]),
        ];
        for (c, expected_bytes) in codes {
            let bytes = bincode::serialize(c).unwrap();
            assert_eq!(
                bytes.len(),
                4,
                "{c:?}: bincode 1.3 fixint encodes variant_index as u32"
            );
            assert_eq!(
                bytes.as_slice(),
                expected_bytes,
                "{c:?} wire encoding changed"
            );
            let back: HelloRejectCode = bincode::deserialize(&bytes).unwrap();
            assert_eq!(*c, back);
        }
    }

    #[test]
    fn hello_round_trip_with_auth_fields() {
        let h = ControlMessage::Hello {
            protocol_version: 3,
            req_width: 1920,
            req_height: 1080,
            req_fps: 60,
            codec: Codec::H265,
            auth_method: AuthMethod::Pin,
            auth_payload: b"correct horse battery staple".to_vec(),
        };
        let bytes = bincode::serialize(&h).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn hello_ack_round_trip_with_permissions() {
        let h = ControlMessage::HelloAck {
            session_id: 0xDEADBEEF,
            host_monotonic_base_us: 42,
            neg_width: 1920,
            neg_height: 1080,
            neg_fps: 60,
            neg_bitrate_bps: 30_000_000,
            host_monitor_rect: MonitorRect::new(0, 0, 1920, 1080),
            host_virtual_desktop_rect: MonitorRect::new(0, 0, 1920, 1080),
            negotiated_codec: Codec::H265,
            host_supported_codecs: vec![Codec::H265, Codec::H264],
            granted_permissions: PermissionSet::view_only(),
        };
        let bytes = bincode::serialize(&h).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn hello_reject_round_trip_with_code() {
        let r = ControlMessage::HelloReject {
            reason: "PIN required".into(),
            code: HelloRejectCode::PinRequired,
        };
        let bytes = bincode::serialize(&r).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn permission_set_default_is_deny_all() {
        // Security invariant: Default must equal deny_all so that legacy
        // KnownPeer entries loaded without a permissions field (via
        // #[serde(default)]) require re-approval rather than gaining access.
        assert_eq!(PermissionSet::default(), PermissionSet::deny_all());
    }

    #[test]
    fn auth_payload_at_max_size_round_trips() {
        let h = ControlMessage::Hello {
            protocol_version: 3,
            req_width: 1920,
            req_height: 1080,
            req_fps: 60,
            codec: Codec::H265,
            auth_method: AuthMethod::Pin,
            auth_payload: vec![0xABu8; MAX_AUTH_PAYLOAD_BYTES],
        };
        let bytes = bincode::serialize(&h).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn hello_ack_round_trip_with_deny_all_permissions() {
        let h = ControlMessage::HelloAck {
            session_id: 0xBEEFCAFE,
            host_monotonic_base_us: 0,
            neg_width: 1920,
            neg_height: 1080,
            neg_fps: 30,
            neg_bitrate_bps: 5_000_000,
            host_monitor_rect: MonitorRect::new(0, 0, 1920, 1080),
            host_virtual_desktop_rect: MonitorRect::new(0, 0, 1920, 1080),
            negotiated_codec: Codec::H264,
            host_supported_codecs: vec![Codec::H264],
            granted_permissions: PermissionSet::deny_all(),
        };
        let bytes = bincode::serialize(&h).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn kind_u8_values_unchanged_by_p6() {
        // P6 only extends existing variants; no new discriminants.
        assert_eq!(
            ControlMessage::Hello {
                protocol_version: 3,
                req_width: 0,
                req_height: 0,
                req_fps: 0,
                codec: Codec::H265,
                auth_method: AuthMethod::Tofu,
                auth_payload: vec![],
            }
            .kind_u8(),
            0
        );
        assert_eq!(
            ControlMessage::HelloReject {
                reason: "".into(),
                code: HelloRejectCode::Unspecified,
            }
            .kind_u8(),
            22
        );
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
