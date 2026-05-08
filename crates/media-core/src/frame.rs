/// One encoded video access unit (Annex-B byte stream of NAL units, or
/// equivalent for non-NAL codecs). Pipeline-level metadata (`seq`,
/// `width`, `height`, `codec`) lives on `prdt_protocol::EncodedFrame`
/// — `EncodedPacket` is the codec-output side of the boundary, before
/// the producer wraps it for the wire.
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp_us: u64,
}
