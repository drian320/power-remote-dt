use crate::control::ControlMessage;
use crate::error::ProtocolError;
use crate::input::{InputEvent, MouseButton};

/// Magic byte identifying our protocol.
pub const MAGIC: u8 = 0x52; // 'R'

/// Current protocol version. Incremented on any breaking wire change.
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Length of the common header in bytes.
pub const HEADER_LEN: usize = 16;

/// Upper bound on a single chunk payload we send over UDP.
/// Derived: IPv4 MTU 1500 - IP 20 - UDP 8 - base header 16 - video header 26 = 1430.
/// We round down to 1200 for safety over tunneled paths.
pub const DEFAULT_CHUNK_PAYLOAD_LEN: usize = 1200;

/// Wire-level packet type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Video = 0,
    Input = 1,
    Control = 2,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Video),
            1 => Some(Self::Input),
            2 => Some(Self::Control),
            _ => None,
        }
    }
}

/// Fixed 16-byte header prefixed onto every UDP packet.
///
/// Layout (little-endian):
/// ```text
/// offset | size | field
/// 0      | 1    | magic (0x52)
/// 1      | 1    | version (0x01)
/// 2      | 1    | packet_type
/// 3      | 1    | flags
/// 4      | 8    | session_id (u64 LE)
/// 12     | 4    | payload_len (u32 LE)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub packet_type: PacketType,
    pub flags: u8,
    pub session_id: u64,
    pub payload_len: u32,
}

impl PacketHeader {
    /// Serialize the header into a 16-byte array.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = MAGIC;
        out[1] = PROTOCOL_VERSION;
        out[2] = self.packet_type as u8;
        out[3] = self.flags;
        out[4..12].copy_from_slice(&self.session_id.to_le_bytes());
        out[12..16].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }

    /// Parse the header from a raw UDP datagram. Validates magic and version.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: HEADER_LEN,
                actual: buf.len(),
            });
        }
        if buf[0] != MAGIC {
            return Err(ProtocolError::BadMagic {
                expected: MAGIC,
                actual: buf[0],
            });
        }
        if buf[1] != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(buf[1]));
        }
        let packet_type =
            PacketType::from_u8(buf[2]).ok_or(ProtocolError::UnknownPacketType(buf[2]))?;
        let flags = buf[3];
        let mut sid = [0u8; 8];
        sid.copy_from_slice(&buf[4..12]);
        let session_id = u64::from_le_bytes(sid);
        let mut plen = [0u8; 4];
        plen.copy_from_slice(&buf[12..16]);
        let payload_len = u32::from_le_bytes(plen);
        Ok(Self {
            packet_type,
            flags,
            session_id,
            payload_len,
        })
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn header_round_trip_video() {
        let h = PacketHeader {
            packet_type: PacketType::Video,
            flags: 0b0000_0001,
            session_id: 0xDEADBEEF_CAFEBABE,
            payload_len: 1200,
        };
        let buf = h.encode();
        assert_eq!(buf[0], MAGIC);
        assert_eq!(buf[1], PROTOCOL_VERSION);
        let parsed = PacketHeader::decode(&buf).expect("decode ok");
        assert_eq!(parsed, h);
    }

    #[test]
    fn header_round_trip_all_types() {
        for t in [PacketType::Video, PacketType::Input, PacketType::Control] {
            let h = PacketHeader {
                packet_type: t,
                flags: 0,
                session_id: 1,
                payload_len: 10,
            };
            let buf = h.encode();
            assert_eq!(PacketHeader::decode(&buf).unwrap(), h);
        }
    }

    #[test]
    fn header_rejects_short_buffer() {
        let buf = [MAGIC, PROTOCOL_VERSION, 0];
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(
            err,
            ProtocolError::PacketTooShort {
                expected: 16,
                actual: 3
            }
        ));
    }

    #[test]
    fn header_rejects_bad_magic() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = 0xAA;
        buf[1] = PROTOCOL_VERSION;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(
            err,
            ProtocolError::BadMagic {
                expected: 0x52,
                actual: 0xAA
            }
        ));
    }

    #[test]
    fn header_rejects_unsupported_version() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = MAGIC;
        buf[1] = 0xFF;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtocolError::UnsupportedVersion(0xFF)));
    }

    #[test]
    fn header_rejects_unknown_packet_type() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = MAGIC;
        buf[1] = PROTOCOL_VERSION;
        buf[2] = 0xAB;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownPacketType(0xAB)));
    }
}

/// VideoPacket payload header length (before chunk data).
pub const VIDEO_PAYLOAD_HDR_LEN: usize = 26;

/// Flags packed into VideoPacket.video_flags byte.
pub mod video_flags {
    pub const IS_KEYFRAME: u8 = 0b0000_0001;
    pub const IS_PARITY: u8 = 0b0000_0010;
}

/// A single video chunk on the wire. For a given frame_seq, the receiver
/// collects all chunks with `chunk_idx in [0, source_chunks + parity_chunks)`
/// and reconstructs the frame via FEC if necessary.
///
/// Payload layout (little-endian, starts at byte 16 of the UDP datagram):
/// ```text
/// offset | size | field
/// 0      | 8    | frame_seq
/// 8      | 8    | timestamp_host_us
/// 16     | 2    | chunk_idx
/// 18     | 2    | source_chunks (k)
/// 20     | 2    | parity_chunks (m)
/// 22     | 1    | video_flags
/// 23     | 1    | reserved
/// 24     | 2    | payload_bytes (valid bytes inside this chunk)
/// 26     | N    | chunk_payload (up to DEFAULT_CHUNK_PAYLOAD_LEN)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket {
    pub frame_seq: u64,
    pub timestamp_host_us: u64,
    pub chunk_idx: u16,
    pub source_chunks: u16,
    pub parity_chunks: u16,
    pub video_flags: u8,
    pub payload_bytes: u16,
    pub chunk_payload: Vec<u8>,
}

impl VideoPacket {
    pub fn is_keyframe(&self) -> bool {
        self.video_flags & video_flags::IS_KEYFRAME != 0
    }

    pub fn is_parity(&self) -> bool {
        self.video_flags & video_flags::IS_PARITY != 0
    }

    /// Serialize into a buffer (caller must prepend PacketHeader separately).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(VIDEO_PAYLOAD_HDR_LEN + self.chunk_payload.len());
        out.extend_from_slice(&self.frame_seq.to_le_bytes());
        out.extend_from_slice(&self.timestamp_host_us.to_le_bytes());
        out.extend_from_slice(&self.chunk_idx.to_le_bytes());
        out.extend_from_slice(&self.source_chunks.to_le_bytes());
        out.extend_from_slice(&self.parity_chunks.to_le_bytes());
        out.push(self.video_flags);
        out.push(0); // reserved
        out.extend_from_slice(&self.payload_bytes.to_le_bytes());
        out.extend_from_slice(&self.chunk_payload);
        out
    }

    /// Parse from a payload slice (body-only, not including common 16B header).
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < VIDEO_PAYLOAD_HDR_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: VIDEO_PAYLOAD_HDR_LEN,
                actual: buf.len(),
            });
        }
        let frame_seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let timestamp_host_us = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let chunk_idx = u16::from_le_bytes(buf[16..18].try_into().unwrap());
        let source_chunks = u16::from_le_bytes(buf[18..20].try_into().unwrap());
        let parity_chunks = u16::from_le_bytes(buf[20..22].try_into().unwrap());
        let video_flags = buf[22];
        let _reserved = buf[23];
        let payload_bytes = u16::from_le_bytes(buf[24..26].try_into().unwrap());

        let expected_payload_end = VIDEO_PAYLOAD_HDR_LEN + payload_bytes as usize;
        if buf.len() < expected_payload_end {
            return Err(ProtocolError::PayloadLengthMismatch {
                header: payload_bytes as u32,
                actual: buf.len() - VIDEO_PAYLOAD_HDR_LEN,
            });
        }
        let chunk_payload = buf[VIDEO_PAYLOAD_HDR_LEN..expected_payload_end].to_vec();
        Ok(Self {
            frame_seq,
            timestamp_host_us,
            chunk_idx,
            source_chunks,
            parity_chunks,
            video_flags,
            payload_bytes,
            chunk_payload,
        })
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;

    #[test]
    fn video_packet_round_trip() {
        let pkt = VideoPacket {
            frame_seq: 42,
            timestamp_host_us: 1_234_567,
            chunk_idx: 3,
            source_chunks: 8,
            parity_chunks: 2,
            video_flags: video_flags::IS_KEYFRAME,
            payload_bytes: 5,
            chunk_payload: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        };
        let buf = pkt.encode();
        assert_eq!(buf.len(), VIDEO_PAYLOAD_HDR_LEN + 5);
        let back = VideoPacket::decode(&buf).unwrap();
        assert_eq!(back, pkt);
        assert!(back.is_keyframe());
        assert!(!back.is_parity());
    }

    #[test]
    fn video_packet_parity_flag() {
        let pkt = VideoPacket {
            frame_seq: 1,
            timestamp_host_us: 0,
            chunk_idx: 9,
            source_chunks: 8,
            parity_chunks: 2,
            video_flags: video_flags::IS_PARITY,
            payload_bytes: 0,
            chunk_payload: vec![],
        };
        let buf = pkt.encode();
        let back = VideoPacket::decode(&buf).unwrap();
        assert!(back.is_parity());
        assert!(!back.is_keyframe());
    }

    #[test]
    fn video_packet_rejects_short() {
        let buf = [0u8; 4];
        assert!(VideoPacket::decode(&buf).is_err());
    }

    #[test]
    fn video_packet_rejects_length_mismatch() {
        // Header says payload_bytes = 99 but only 3 bytes of payload present.
        let mut buf = vec![0u8; VIDEO_PAYLOAD_HDR_LEN];
        buf[24..26].copy_from_slice(&99u16.to_le_bytes());
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert!(matches!(
            VideoPacket::decode(&buf).unwrap_err(),
            ProtocolError::PayloadLengthMismatch {
                header: 99,
                actual: 3
            }
        ));
    }
}

/// InputPacket fixed-prefix length (before event-specific body).
pub const INPUT_PAYLOAD_HDR_LEN: usize = 17;

/// Wire representation of a single input event.
///
/// Layout (little-endian, after 16B common header):
/// ```text
/// offset | size | field
/// 0      | 8    | input_seq
/// 8      | 8    | timestamp_viewer_us
/// 16     | 1    | event_kind
/// 17     | N    | event_body  (kind-specific)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPacket {
    pub input_seq: u64,
    pub timestamp_viewer_us: u64,
    pub event: InputEvent,
}

impl InputPacket {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(INPUT_PAYLOAD_HDR_LEN + 9);
        out.extend_from_slice(&self.input_seq.to_le_bytes());
        out.extend_from_slice(&self.timestamp_viewer_us.to_le_bytes());
        out.push(self.event.kind_u8());
        match self.event {
            InputEvent::MouseMove { x, y, absolute } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.push(absolute as u8);
            }
            InputEvent::MouseButton { button, pressed } => {
                out.push(button as u8);
                out.push(pressed as u8);
            }
            InputEvent::MouseWheel { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            InputEvent::Key { scancode, pressed } => {
                out.extend_from_slice(&scancode.to_le_bytes());
                out.push(pressed as u8);
            }
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < INPUT_PAYLOAD_HDR_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: INPUT_PAYLOAD_HDR_LEN,
                actual: buf.len(),
            });
        }
        let input_seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let timestamp_viewer_us = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let event_kind = buf[16];
        let body = &buf[17..];
        let event = match event_kind {
            0 => {
                if body.len() < 9 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 9,
                        actual: buf.len(),
                    });
                }
                InputEvent::MouseMove {
                    x: i32::from_le_bytes(body[0..4].try_into().unwrap()),
                    y: i32::from_le_bytes(body[4..8].try_into().unwrap()),
                    absolute: body[8] != 0,
                }
            }
            1 => {
                if body.len() < 2 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 2,
                        actual: buf.len(),
                    });
                }
                let button = MouseButton::from_u8(body[0])
                    .ok_or(ProtocolError::UnknownEventKind(body[0]))?;
                InputEvent::MouseButton {
                    button,
                    pressed: body[1] != 0,
                }
            }
            2 => {
                if body.len() < 8 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 8,
                        actual: buf.len(),
                    });
                }
                InputEvent::MouseWheel {
                    dx: i32::from_le_bytes(body[0..4].try_into().unwrap()),
                    dy: i32::from_le_bytes(body[4..8].try_into().unwrap()),
                }
            }
            3 => {
                if body.len() < 5 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 5,
                        actual: buf.len(),
                    });
                }
                InputEvent::Key {
                    scancode: u32::from_le_bytes(body[0..4].try_into().unwrap()),
                    pressed: body[4] != 0,
                }
            }
            other => return Err(ProtocolError::UnknownEventKind(other)),
        };
        Ok(Self {
            input_seq,
            timestamp_viewer_us,
            event,
        })
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn input_packet_all_kinds_round_trip() {
        let cases = [
            InputEvent::MouseMove {
                x: 100,
                y: -50,
                absolute: true,
            },
            InputEvent::MouseMove {
                x: -1,
                y: 1,
                absolute: false,
            },
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            InputEvent::MouseButton {
                button: MouseButton::X2,
                pressed: false,
            },
            InputEvent::MouseWheel { dx: 0, dy: 120 },
            InputEvent::Key {
                scancode: 0x1E,
                pressed: true,
            },
            InputEvent::Key {
                scancode: 0xE0_5D,
                pressed: false,
            },
        ];
        for (i, ev) in cases.iter().enumerate() {
            let p = InputPacket {
                input_seq: i as u64,
                timestamp_viewer_us: 100 + i as u64,
                event: *ev,
            };
            let buf = p.encode();
            let back = InputPacket::decode(&buf).unwrap();
            assert_eq!(back, p, "round trip failed for {:?}", ev);
        }
    }

    #[test]
    fn input_packet_rejects_unknown_kind() {
        let mut buf = vec![0u8; INPUT_PAYLOAD_HDR_LEN + 4];
        buf[16] = 0x42;
        assert!(matches!(
            InputPacket::decode(&buf).unwrap_err(),
            ProtocolError::UnknownEventKind(0x42),
        ));
    }
}

/// Serialize a ControlMessage as: [1B kind][bincode body].
pub fn encode_control(msg: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    let kind = msg.kind_u8();
    let mut out = Vec::with_capacity(32);
    out.push(kind);
    bincode::serialize_into(&mut out, msg)?;
    Ok(out)
}

/// Deserialize a ControlMessage from the same layout.
pub fn decode_control(buf: &[u8]) -> Result<ControlMessage, ProtocolError> {
    if buf.is_empty() {
        return Err(ProtocolError::PacketTooShort {
            expected: 1,
            actual: 0,
        });
    }
    let kind = buf[0];
    // We don't trust `kind` blindly; bincode will decode the whole tagged enum.
    // We keep the leading byte as a fast-path dispatch hint for future optimization.
    if kind > 7 {
        return Err(ProtocolError::UnknownControlKind(kind));
    }
    let msg: ControlMessage = bincode::deserialize(&buf[1..])?;
    Ok(msg)
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use crate::frame::Codec;

    #[test]
    fn control_hello_round_trip() {
        let msg = ControlMessage::Hello {
            protocol_version: 1,
            req_width: 3840,
            req_height: 2160,
            req_fps: 60,
            codec: Codec::H265,
        };
        let buf = encode_control(&msg).unwrap();
        assert_eq!(buf[0], msg.kind_u8());
        let back = decode_control(&buf).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn control_all_kinds_round_trip() {
        let cases = [
            ControlMessage::Bye,
            ControlMessage::RequestIdr,
            ControlMessage::Ping {
                ping_seq: 1,
                viewer_ts_us: 2,
            },
            ControlMessage::Pong {
                ping_seq: 1,
                viewer_ts_us: 2,
                host_ts_us: 3,
            },
            ControlMessage::SetBitrate {
                target_bps: 50_000_000,
            },
            ControlMessage::Stats {
                loss_rate_ppm: 500,
                fps_millis: 59_940,
                bitrate_bps: 50_000_000,
            },
        ];
        for msg in cases {
            let buf = encode_control(&msg).unwrap();
            let back = decode_control(&buf).unwrap();
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn control_rejects_unknown_kind() {
        let buf = vec![0xFF];
        assert!(matches!(
            decode_control(&buf).unwrap_err(),
            ProtocolError::UnknownControlKind(0xFF),
        ));
    }
}
