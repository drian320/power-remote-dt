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

/// Tightly packed BGRA frame on CPU memory (4 bytes/pixel, row stride =
/// `width * 4` for unpadded captures).
///
/// Mirrors `Nv12Frame` / `I420Frame` for symmetry; the carrier lives here
/// so capture producers (`prdt_media_linux`'s `X11ShmCapturer`,
/// `prdt_media_win`'s DXGI capturer) and encode consumers
/// (`prdt_media_ffmpeg`'s P2.5 NPP encoder) reference one shared type
/// without a circular dep.
///
/// Used by the encode-side GPU-residency path
/// (`HevcNvencNppFfmpegEncoderAdapter`) so the encoder accepts BGRA
/// straight from `X11ShmCapturer` instead of routing through CPU
/// `bgra_to_i420` + `i420_to_nv12_into` before uploading.
#[derive(Debug, Clone)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly-packed BGRA, length >= `(width as usize) * 4 * (height as usize)`.
    pub bgra: Vec<u8>,
    /// Row stride in bytes. Equals `width * 4` for unpadded captures (the
    /// only case in P2.5); reserved for future strided/padded sources.
    pub stride: u32,
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

/// HDR10 SMPTE 2086 mastering display + MaxCLL/MaxFALL metadata.
///
/// Field units match the `DXGI_HDR_METADATA_HDR10` struct:
/// - `display_primaries` and `white_point`: CIE 1931 xy chromaticity,
///   units of 0.00002 (i.e. 50000 = 1.0).
/// - `min/max_mastering_luminance`: units of 0.0001 cd/m².
/// - `max_content_light_level` / `max_frame_average_light_level`: cd/m².
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hdr10Metadata {
    /// Display primaries in CIE 1931 xy chromaticity coords (units of 0.00002).
    /// Order: R (index 0), G (index 1), B (index 2).
    pub display_primaries: [(u16, u16); 3],
    /// White point in CIE 1931 xy (units of 0.00002).
    pub white_point: (u16, u16),
    /// Min display mastering luminance (units of 0.0001 cd/m²).
    pub min_mastering_luminance: u32,
    /// Max display mastering luminance (units of 0.0001 cd/m²).
    pub max_mastering_luminance: u32,
    /// MaxCLL (cd/m²).
    pub max_content_light_level: u16,
    /// MaxFALL (cd/m²).
    pub max_frame_average_light_level: u16,
}

/// P010LE-formatted Main10 4:2:0 frame on CPU memory. Y/UV planes carry
/// `u16` samples in 16-bit containers with the valid 10 bits in the
/// **high** part of each container (libavcodec P010LE convention).
#[derive(Debug, Clone)]
pub struct Nv12Frame16 {
    pub width: u32,
    pub height: u32,
    /// Y plane, length >= `stride_y * height` (in u16 elements).
    pub y: Vec<u16>,
    /// Interleaved UV plane (U,V,U,V,…), length >= `stride_uv * (height / 2)`.
    pub uv: Vec<u16>,
    /// Row stride in units of u16 elements.
    pub stride_y: u32,
    pub stride_uv: u32,
    pub pts_us: u64,
    /// HDR10 mastering display metadata, present when the bitstream carries it.
    pub hdr10: Option<Hdr10Metadata>,
}

/// Pixel-format-tagged decoded frame. Only the variants needed by a
/// given pipeline have to be matched; the OpenH264 path keeps using
/// `prdt_media_sw::I420Frame` directly today, so this enum starts with
/// only the new NV12 variant. An `I420` variant can land alongside if
/// a future refactor unifies the OpenH264 carrier through `media-core`.
#[derive(Debug, Clone)]
pub enum DecodedFrame {
    Nv12(Arc<Nv12Frame>),
    Nv12_10(Arc<Nv12Frame16>),
}
