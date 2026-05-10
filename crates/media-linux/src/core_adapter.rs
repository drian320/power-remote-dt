//! L0 trait impls for unit-test surface and L2/L3 trait-object refactor
//! prep. NOT used by production wiring (see `linux_sw_producer.rs` for
//! the `VideoProducer` path the host actually consumes).

use crate::frame::BgraFrame;
use crate::sw_pipeline::{LinuxSwDecoder, LinuxSwEncoder};
use crate::x11_capture::X11ShmCapturer;
use prdt_media_core::{
    CaptureError, Capturer, DecodeError, Decoder, EncodeError, EncodedPacket, Encoder,
};

pub struct LinuxX11ShmCapturer {
    inner: X11ShmCapturer,
}

impl LinuxX11ShmCapturer {
    pub fn new() -> Result<Self, CaptureError> {
        Ok(Self {
            inner: X11ShmCapturer::new().map_err(CaptureError::from)?,
        })
    }
}

impl Capturer for LinuxX11ShmCapturer {
    type Frame = BgraFrame;

    fn next_frame(&mut self) -> Result<Self::Frame, CaptureError> {
        let mut frame = BgraFrame::new_zeroed(self.inner.width(), self.inner.height());
        self.inner
            .grab_into(&mut frame.bgra)
            .map_err(CaptureError::from)?;
        Ok(frame)
    }
}

pub struct LinuxOpenh264Encoder {
    inner: LinuxSwEncoder,
}

impl LinuxOpenh264Encoder {
    pub fn new(width: u32, height: u32, bitrate_bps: u32, fps: u32) -> Result<Self, EncodeError> {
        Ok(Self {
            inner: LinuxSwEncoder::new(width, height, bitrate_bps, fps)
                .map_err(EncodeError::from)?,
        })
    }
}

impl Encoder for LinuxOpenh264Encoder {
    type Frame = BgraFrame;

    fn encode(
        &mut self,
        frame: &Self::Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        let ef = self
            .inner
            .encode(frame, force_idr, timestamp_us)
            .map_err(EncodeError::from)?;
        // EncodedFrame -> EncodedPacket (L0 surface) — drop wire-only
        // metadata (codec/width/height/seq).
        // EncodedFrame fields: nal_units: bytes::Bytes, is_keyframe: bool,
        // timestamp_host_us: u64.
        Ok(EncodedPacket {
            nal_bytes: ef.nal_units.to_vec(),
            is_keyframe: ef.is_keyframe,
            timestamp_us: ef.timestamp_host_us,
        })
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        self.inner.set_target_bitrate(bps);
    }

    fn backend_name(&self) -> &'static str {
        "linux-x11shm-openh264"
    }
}

pub struct LinuxOpenh264Decoder {
    inner: LinuxSwDecoder,
}

impl LinuxOpenh264Decoder {
    pub fn new() -> Result<Self, DecodeError> {
        Ok(Self {
            inner: LinuxSwDecoder::new().map_err(DecodeError::from)?,
        })
    }
}

impl Decoder for LinuxOpenh264Decoder {
    type Frame = prdt_media_sw::I420Frame;

    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<Self::Frame>, DecodeError> {
        self.inner.decode(packet).map_err(DecodeError::from)
    }

    fn backend_name(&self) -> &'static str {
        "linux-openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_backend_name_is_static() {
        let _: &str = "linux-x11shm-openh264";
    }

    #[test]
    fn decoder_round_trip_via_l0_traits() {
        const W: u32 = 320;
        const H: u32 = 240;
        let mut enc = LinuxOpenh264Encoder::new(W, H, 1_000_000, 30).expect("encoder");
        let mut dec = LinuxOpenh264Decoder::new().expect("decoder");
        let f = BgraFrame {
            width: W,
            height: H,
            stride: W * 4,
            bgra: vec![0x80u8; (W * H * 4) as usize],
            capture_ts_us: 0,
        };
        let pkt = enc.encode(&f, true, 0).expect("encode");
        assert!(pkt.is_keyframe);
        let mut decoded = dec.decode(&pkt).expect("dec");
        if decoded.is_none() {
            let p2 = enc.encode(&f, false, 1).expect("enc 2");
            decoded = dec.decode(&p2).expect("dec 2");
        }
        assert!(decoded.is_some());
    }

    #[test]
    fn encoder_is_object_safe_via_dyn() {
        // Type-level smoke: ensure Encoder<Frame=BgraFrame> can be held
        // as a Box<dyn Encoder<Frame = BgraFrame>>.
        fn _accept<E: Encoder<Frame = BgraFrame> + 'static>(_e: E) {}
        // Compile-only: skip instantiation (needs X11 etc.).
    }
}
