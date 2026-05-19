//! Shared abstraction over Windows H.265 hardware encoders.
//!
//! Two implementations:
//! - `crate::nvenc::NvencEncoder` (NVIDIA HW only, lowest latency)
//! - `crate::mf::MfH265Encoder` (any DXGI adapter via Media Foundation MFT)
//!
//! Both produce Annex-B H.265 NAL units consumable by the existing
//! `MfD3d11Consumer` / `NvdecD3d11Consumer` decoders without any
//! transport-layer change.
//!
//! Future: a `Dx12Hevc265Encoder` trait taking `&D3d12Resource` will be
//! added when DX12 Video Encode is wired in. The two trait families
//! stay separate because D3D11 and D3D12 textures are not interchangeable.

use crate::d3d11::D3d11Texture;
use crate::error::MediaError;

/// One encoded H.265 access unit (Annex-B byte stream).
#[derive(Debug, Clone)]
pub struct EncodedH265Frame {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp: u64,
}

/// HW H.265 encoder operating on D3D11 input textures.
pub trait Hevc265Encoder: Send {
    /// Encode a `B8G8R8A8_UNORM` D3D11 texture into a single H.265
    /// access unit. `force_idr == true` requests an IDR + parameter
    /// sets at the next encode opportunity.
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError>;

    /// Best-effort target bitrate update (bits per second). The encoder
    /// may take effect on the next IDR or sooner depending on backend.
    fn set_target_bitrate(&mut self, bps: u32);

    /// Stable identifier for logs / bench output.
    fn backend_name(&self) -> &'static str;
}

use crate::mf::MfH265Encoder;
#[cfg(prdt_nvenc_bindings)]
use crate::nvenc::NvencEncoder;

/// Runtime-dispatched HW H.265 encoder. Used by the producer layer so
/// the rest of the pipeline (transport, decoder selection, etc.) does
/// not care which backend is in use.
pub enum HwHevcEncoder {
    #[cfg(prdt_nvenc_bindings)]
    Nvenc(Box<NvencEncoder>),
    Mf(Box<MfH265Encoder>),
    #[cfg(feature = "media-win-ffmpeg-nvenc-any")]
    FfmpegNvec(Box<crate::ffmpeg::HevcNvencFfmpegEncoderWindowsAdapter>),
    #[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
    FfmpegNvecMain10(Box<crate::ffmpeg::HevcNvencMain10FfmpegEncoderWindowsAdapter>),
}

impl Hevc265Encoder for HwHevcEncoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        match self {
            #[cfg(prdt_nvenc_bindings)]
            Self::Nvenc(e) => e.encode(texture, force_idr, timestamp_us),
            Self::Mf(e) => e.encode(texture, force_idr, timestamp_us),
            #[cfg(feature = "media-win-ffmpeg-nvenc-any")]
            Self::FfmpegNvec(e) => e.encode(texture, force_idr, timestamp_us),
            #[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
            Self::FfmpegNvecMain10(e) => e.encode(texture, force_idr, timestamp_us),
        }
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            #[cfg(prdt_nvenc_bindings)]
            Self::Nvenc(e) => e.set_target_bitrate(bps),
            Self::Mf(e) => e.set_target_bitrate(bps),
            #[cfg(feature = "media-win-ffmpeg-nvenc-any")]
            Self::FfmpegNvec(e) => e.set_target_bitrate(bps),
            #[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
            Self::FfmpegNvecMain10(e) => e.set_target_bitrate(bps),
        }
    }

    fn backend_name(&self) -> &'static str {
        match self {
            #[cfg(prdt_nvenc_bindings)]
            Self::Nvenc(e) => e.backend_name(),
            Self::Mf(e) => e.backend_name(),
            #[cfg(feature = "media-win-ffmpeg-nvenc-any")]
            Self::FfmpegNvec(e) => e.backend_name(),
            #[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
            Self::FfmpegNvecMain10(e) => e.backend_name(),
        }
    }
}

#[cfg(prdt_nvenc_bindings)]
impl From<NvencEncoder> for HwHevcEncoder {
    fn from(e: NvencEncoder) -> Self {
        Self::Nvenc(Box::new(e))
    }
}

impl From<MfH265Encoder> for HwHevcEncoder {
    fn from(e: MfH265Encoder) -> Self {
        Self::Mf(Box::new(e))
    }
}

#[cfg(feature = "media-win-ffmpeg-nvenc-any")]
impl From<crate::ffmpeg::HevcNvencFfmpegEncoderWindowsAdapter> for HwHevcEncoder {
    fn from(e: crate::ffmpeg::HevcNvencFfmpegEncoderWindowsAdapter) -> Self {
        Self::FfmpegNvec(Box::new(e))
    }
}

#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
impl From<crate::ffmpeg::HevcNvencMain10FfmpegEncoderWindowsAdapter> for HwHevcEncoder {
    fn from(e: crate::ffmpeg::HevcNvencMain10FfmpegEncoderWindowsAdapter) -> Self {
        Self::FfmpegNvecMain10(Box::new(e))
    }
}
