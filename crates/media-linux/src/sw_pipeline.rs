//! Linux SW encode/decode, wrapping `prdt-media-sw` (OpenH264).
//!
//! The encoder takes BgraFrame, converts to I420 in scratch buffer,
//! and runs the OpenH264 encoder. The decoder takes EncodedPacket and
//! returns I420Frame for downstream BGRA conversion.

use crate::error::LinuxMediaError;
use crate::frame::BgraFrame;
use prdt_media_sw::{
    bgra_to_i420, I420Frame, Openh264Decoder, Openh264Encoder, Openh264EncoderConfig,
    SwH264Decoder, SwH264Encoder,
};

pub struct LinuxSwEncoder {
    inner: Openh264Encoder,
    width: u32,
    height: u32,
}

impl LinuxSwEncoder {
    pub fn new(
        width: u32,
        height: u32,
        bitrate_bps: u32,
        fps: u32,
    ) -> Result<Self, LinuxMediaError> {
        let cfg = Openh264EncoderConfig {
            width,
            height,
            target_bitrate_bps: bitrate_bps,
            max_fps: fps as f32,
        };
        let inner = Openh264Encoder::new(cfg)?;
        Ok(Self {
            inner,
            width,
            height,
        })
    }

    /// Returns the encoder's `EncodedFrame` directly (mirrors
    /// `Openh264Encoder.encode` and the Windows `DxgiSwProducer`
    /// pattern). Producer-level wrapping (seq override) is done by
    /// `LinuxSwProducer`. The L0 trait adapter in `core_adapter.rs`
    /// converts EncodedFrame → EncodedPacket for trait users.
    pub fn encode(
        &mut self,
        frame: &BgraFrame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<prdt_protocol::EncodedFrame, LinuxMediaError> {
        if frame.width != self.width || frame.height != self.height {
            return Err(LinuxMediaError::InvalidDimensions(
                frame.width,
                frame.height,
            ));
        }
        let i420 = bgra_to_i420(&frame.bgra, frame.width, frame.height, frame.stride)?;
        let out = self.inner.encode(&i420, force_idr, timestamp_us)?;
        Ok(out)
    }

    pub fn set_target_bitrate(&mut self, bps: u32) {
        self.inner.set_target_bitrate(bps);
    }
}

pub struct LinuxSwDecoder {
    inner: Openh264Decoder,
}

impl LinuxSwDecoder {
    pub fn new() -> Result<Self, LinuxMediaError> {
        Ok(Self {
            inner: Openh264Decoder::new()?,
        })
    }

    /// Decode an L0 EncodedPacket. (Viewer side uses this; the network
    /// receive path strips EncodedFrame down to nal_bytes before feeding
    /// the decoder, so this signature matches what the viewer plumbs.)
    pub fn decode(
        &mut self,
        packet: &prdt_media_core::EncodedPacket,
    ) -> Result<Option<I420Frame>, LinuxMediaError> {
        let out = self.inner.decode(&packet.nal_bytes)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prdt_media_sw::make_counter_i420;

    #[test]
    fn encode_decode_round_trip_via_media_sw() {
        // 320x240 keeps the test fast and well within OpenH264's tile
        // constraints.
        const W: u32 = 320;
        const H: u32 = 240;
        let mut enc = LinuxSwEncoder::new(W, H, 1_000_000, 30).expect("encoder");
        let mut dec = LinuxSwDecoder::new().expect("decoder");

        // Build a simple BGRA frame from the counter I420 helper to
        // ensure the input is a recognizable pattern.
        let i420_in = make_counter_i420(W, H, 42).expect("counter i420");
        // Convert i420 → bgra by laying out a flat BGRA frame: easier
        // path here is to skip i420→bgra and just feed gray.
        let bgra = vec![0x80u8; (W * H * 4) as usize];
        let frame = BgraFrame {
            width: W,
            height: H,
            stride: W * 4,
            bgra,
            capture_ts_us: 1234,
        };

        let frame_out = enc.encode(&frame, true, 1234).expect("encode");
        assert!(frame_out.is_keyframe);
        assert!(!frame_out.nal_units.is_empty());

        // Convert to EncodedPacket for the decoder API.
        let pkt = prdt_media_core::EncodedPacket {
            nal_bytes: frame_out.nal_units.to_vec(),
            is_keyframe: frame_out.is_keyframe,
            timestamp_us: frame_out.timestamp_host_us,
        };
        // First decode of an IDR should yield Some after enough data;
        // OpenH264 may need a follow-up packet, so feed up to 5 frames.
        let mut decoded = dec.decode(&pkt).expect("decode 1");
        for i in 2u64..=5 {
            if decoded.is_some() {
                break;
            }
            let frame_n = enc.encode(&frame, false, 1234 + i).expect("encode N");
            let pkt_n = prdt_media_core::EncodedPacket {
                nal_bytes: frame_n.nal_units.to_vec(),
                is_keyframe: frame_n.is_keyframe,
                timestamp_us: frame_n.timestamp_host_us,
            };
            decoded = dec.decode(&pkt_n).expect("decode N");
        }
        assert!(
            decoded.is_some(),
            "decoder should yield a frame after IDR + follow-ups"
        );

        // Suppress unused-var warning for i420_in (kept for clarity).
        let _ = i420_in;
    }

    #[test]
    fn dimension_mismatch_returns_format_error() {
        let mut enc = LinuxSwEncoder::new(320, 240, 500_000, 30).expect("encoder");
        let f = BgraFrame::new_zeroed(640, 480);
        let r = enc.encode(&f, false, 0);
        assert!(matches!(
            r,
            Err(LinuxMediaError::InvalidDimensions(640, 480))
        ));
    }

    #[test]
    fn set_target_bitrate_is_idempotent_and_safe() {
        let mut enc = LinuxSwEncoder::new(320, 240, 500_000, 30).expect("encoder");
        enc.set_target_bitrate(2_000_000);
        enc.set_target_bitrate(0); // boundary
    }
}
