use prdt_media_core::{EncodeError, EncodedPacket, Encoder};

struct DummyEncoder;

impl Encoder for DummyEncoder {
    type Frame = ();

    fn encode(
        &mut self,
        _frame: &Self::Frame,
        _force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        Ok(EncodedPacket {
            nal_bytes: Vec::new(),
            is_keyframe: false,
            timestamp_us,
        })
    }

    fn set_target_bitrate(&mut self, _bps: u32) {}
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

#[test]
fn encoder_is_dyn_compatible() {
    let mut enc: Box<dyn Encoder<Frame = ()>> = Box::new(DummyEncoder);
    let p = enc.encode(&(), false, 12_345).expect("encode");
    assert_eq!(p.timestamp_us, 12_345);
    assert!(!p.is_keyframe);
    assert_eq!(enc.backend_name(), "dummy");
}
