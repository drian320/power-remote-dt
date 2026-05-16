use std::sync::Arc;

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

/// NV12 (4:2:0, interleaved chroma) frame on CPU memory. Mirrors
/// `prdt_media_sw::I420Frame` in spirit but for the NV12 layout that
/// libavcodec's hardware decoders (`hevc_vaapi`, `hevc_cuvid`) emit
/// natively after `av_hwframe_transfer_data`. The carrier lives in
/// `media-core` so `media-ffmpeg`, `media-linux`, and `viewer` can
/// reference one shared type without a circular dependency.
#[derive(Debug, Clone)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    /// Y plane, length >= `stride_y * height`.
    pub y: Vec<u8>,
    /// Interleaved UV plane (U,V,U,V,…), length >= `stride_uv * (height / 2)`.
    pub uv: Vec<u8>,
    pub stride_y: u32,
    pub stride_uv: u32,
    /// Source-side presentation timestamp in microseconds. The encoded
    /// frame's `timestamp_host_us` is plumbed through to the decoded
    /// frame so latency probes can close the loop without an extra side
    /// channel.
    pub pts_us: u64,
}

/// Pixel-format-tagged decoded frame. Only the variants needed by a
/// given pipeline have to be matched; the OpenH264 path keeps using
/// `prdt_media_sw::I420Frame` directly today, so this enum starts with
/// only the new NV12 variant. An `I420` variant can land alongside if
/// a future refactor unifies the OpenH264 carrier through `media-core`.
#[derive(Debug, Clone)]
pub enum DecodedFrame {
    Nv12(Arc<Nv12Frame>),
}
