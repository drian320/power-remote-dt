use crate::error::ProtocolError;

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
mod tests {
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
