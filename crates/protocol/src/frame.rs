use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Codec discriminator. For Phase 0 we only support H.265, but keep it
/// open so Phase 3+ can slot in AV1 without a protocol-breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Codec {
    H265 = 0,
    H264 = 1,
    Av1 = 2,
    /// HEVC Main10 (10-bit 4:2:0). variant_index=3 on the wire.
    /// Pre-PR1 clients reject this discriminant at the bincode layer;
    /// hosts MUST NOT advertise it in HelloAck unless the inbound
    /// Hello.codec was itself H265Main10 (R15 mitigation).
    H265Main10 = 3,
}

impl Codec {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::H265),
            1 => Some(Self::H264),
            2 => Some(Self::Av1),
            3 => Some(Self::H265Main10),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::H265 => "h265",
            Self::H264 => "h264",
            Self::Av1 => "av1",
            Self::H265Main10 => "h265-main10",
        }
    }
}

/// A single encoded video frame - one or more NAL units concatenated.
/// Zero-copy: `nal_units` is `Bytes` so the producer can retain ownership
/// of an underlying encoder buffer if it wants.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub seq: u64,
    pub timestamp_host_us: u64,
    pub is_keyframe: bool,
    pub nal_units: Bytes,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
}

impl EncodedFrame {
    pub fn new_h265(
        seq: u64,
        timestamp_host_us: u64,
        is_keyframe: bool,
        nal_units: Bytes,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            seq,
            timestamp_host_us,
            is_keyframe,
            nal_units,
            width,
            height,
            codec: Codec::H265,
        }
    }

    pub fn new_h264(
        seq: u64,
        timestamp_host_us: u64,
        is_keyframe: bool,
        nal_units: Bytes,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            seq,
            timestamp_host_us,
            is_keyframe,
            nal_units,
            width,
            height,
            codec: Codec::H264,
        }
    }

    pub fn byte_len(&self) -> usize {
        self.nal_units.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trip() {
        for v in 0u8..=2 {
            let c = Codec::from_u8(v).unwrap();
            assert_eq!(c as u8, v);
        }
        assert!(Codec::from_u8(42).is_none());
    }

    #[test]
    fn codec_from_u8_h265_main10() {
        assert_eq!(Codec::from_u8(3), Some(Codec::H265Main10));
    }

    #[test]
    fn codec_h265_main10_name() {
        assert_eq!(Codec::H265Main10.name(), "h265-main10");
    }

    // bincode 1.3 fixint encodes variant_index as 4-byte LE u32.
    #[test]
    fn codec_h265_main10_bincode_wire_bytes() {
        let c = Codec::H265Main10;
        let bytes = bincode::serialize(&c).expect("serialize");
        assert_eq!(
            bytes,
            [3, 0, 0, 0],
            "bincode 1.3 fixint encodes variant_index as u32 LE"
        );
        let back: Codec = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn encoded_frame_construction() {
        let f = EncodedFrame::new_h265(
            1,
            12345,
            true,
            Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x40, 0x01]),
            3840,
            2160,
        );
        assert_eq!(f.seq, 1);
        assert_eq!(f.timestamp_host_us, 12345);
        assert!(f.is_keyframe);
        assert_eq!(f.width, 3840);
        assert_eq!(f.height, 2160);
        assert_eq!(f.codec, Codec::H265);
        assert_eq!(f.byte_len(), 6);
    }
}
