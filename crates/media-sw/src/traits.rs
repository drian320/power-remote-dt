//! Public traits for SW H.264 encode and decode, parallel to
//! `prdt_media_win::Hevc265Encoder` so the producer / consumer dispatch
//! layers can hold either backend behind a uniform interface.

use crate::error::MediaSwError;
use crate::nv12::I420Frame;
use prdt_protocol::frame::EncodedFrame;

/// SW H.264 encoder operating on CPU-side I420 frames.
pub trait SwH264Encoder: Send {
    /// Encode one I420 frame into a single H.264 access unit (Annex-B
    /// byte stream concatenation of NAL units). `force_idr == true`
    /// requests the next frame be an IDR with parameter sets.
    fn encode(
        &mut self,
        i420: &I420Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> std::result::Result<EncodedFrame, MediaSwError>;

    /// Best-effort target bitrate update (bits per second).
    fn set_target_bitrate(&mut self, bps: u32);

    /// Stable identifier for logs / bench output.
    fn backend_name(&self) -> &'static str;
}

/// SW H.264 decoder. Output is borrowed from the decoder's internal
/// buffer; the caller must consume it before the next decode call.
pub trait SwH264Decoder: Send {
    /// Decode an Annex-B H.264 access unit. Returns `Ok(Some(frame))`
    /// when a picture is available, `Ok(None)` when the decoder needs
    /// more input (e.g. SPS-only on the first call).
    fn decode(&mut self, nal_units: &[u8]) -> std::result::Result<Option<I420Frame>, MediaSwError>;

    fn backend_name(&self) -> &'static str;
}
