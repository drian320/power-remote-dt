#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
use prdt_media_core::BgraFrame;
#[cfg(any(
    feature = "ffmpeg-encode-hevc-vaapi-any",
    feature = "ffmpeg-encode-hevc-nvenc-any"
))]
use prdt_media_core::{EncodeError, EncodedPacket, Encoder};
#[cfg(any(
    feature = "ffmpeg-encode-hevc-vaapi-any",
    feature = "ffmpeg-encode-hevc-nvenc-any"
))]
use prdt_media_sw::I420Frame;

#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
))]
use prdt_media_core::{DecodeError, Nv12Frame};

#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
))]
use crate::decoder_common::HevcDecoderBackend;
#[cfg(feature = "ffmpeg-decode-hevc-nvdec-any")]
use crate::hevc_nvdec_decoder::HevcNvdecFfmpegDecoder;
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
use crate::hevc_nvenc_encoder::HevcNvencFfmpegEncoder;
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
use crate::hevc_nvenc_npp_encoder::HevcNvencNppFfmpegEncoder;
#[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
use crate::hevc_sw_decoder::HevcSwFfmpegDecoder;
#[cfg(feature = "ffmpeg-decode-hevc-vaapi-any")]
use crate::hevc_vaapi_decoder::HevcVaapiFfmpegDecoder;
#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
use crate::hevc_vaapi_encoder::HevcVaapiFfmpegEncoder;

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
pub struct HevcVaapiFfmpegEncoderAdapter(pub HevcVaapiFfmpegEncoder);

// SAFETY: HevcVaapiFfmpegEncoder owns all its FFmpeg resources exclusively via
// NonNull pointers. It is never aliased and the caller ensures it is only used
// from one thread at a time (the encoder pipeline always runs single-threaded).
#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
unsafe impl Send for HevcVaapiFfmpegEncoderAdapter {}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
impl Encoder for HevcVaapiFfmpegEncoderAdapter {
    type Frame = I420Frame;

    fn encode(
        &mut self,
        frame: &I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        self.0.encode(frame, force_idr, ts_us)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Err(e) = self.0.set_target_bitrate(bps) {
            // Rate-limited (1/min) warn-log per plan Critic fold-in 6.
            self.0.maybe_warn_bitrate_failure(&e, bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
pub struct HevcNvencFfmpegEncoderAdapter(pub HevcNvencFfmpegEncoder);

// SAFETY: HevcNvencFfmpegEncoder owns all its FFmpeg + CUDA resources via
// NonNull pointers; never aliased; pipeline is single-threaded.
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
unsafe impl Send for HevcNvencFfmpegEncoderAdapter {}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
impl Encoder for HevcNvencFfmpegEncoderAdapter {
    type Frame = I420Frame;

    fn encode(
        &mut self,
        frame: &I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        self.0.encode(frame, force_idr, ts_us)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Err(e) = self.0.set_target_bitrate(bps) {
            self.0.maybe_warn_bitrate_failure(&e, bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
pub struct HevcNvencNppFfmpegEncoderAdapter(pub HevcNvencNppFfmpegEncoder);

// SAFETY: HevcNvencNppFfmpegEncoder owns all its FFmpeg + CUDA resources
// via NonNull pointers + CudaDevicePtr newtype wrappers; never aliased;
// pipeline is single-threaded. Same Send-safety argument as
// HevcNvencFfmpegEncoderAdapter above. Required by the producer spawn at
// crates/host/src/platform/linux.rs (host code moves the encoder into a
// tokio task at startup, then per-frame encode calls happen on a single
// owner thread).
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
unsafe impl Send for HevcNvencNppFfmpegEncoderAdapter {}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
impl Encoder for HevcNvencNppFfmpegEncoderAdapter {
    type Frame = BgraFrame;

    fn encode(
        &mut self,
        frame: &BgraFrame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        self.0.encode(frame, force_idr, ts_us)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Err(e) = self.0.set_target_bitrate(bps) {
            self.0.maybe_warn_bitrate_failure(&e, bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Decode side (P2). One generic adapter parameterised over `HevcDecoderBackend`
// — per Option O1 in the plan, the three backends share a uniform interface
// (unlike the encoders, where bitrate-control divergence justified per-backend
// structs), so collapsing the adapter to a single generic saves ~50 LoC and
// paves the way for P2.5's GPU-to-GPU path (the trait can grow a
// `drain_hw_frame` method later without touching this layer).
// ────────────────────────────────────────────────────────────────────────────

/// Thin wrapper holding any `HevcDecoderBackend`, plus the `Send` impl the
/// viewer needs to keep the consumer state inside a `tokio::sync::Mutex`.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
))]
pub struct HevcDecoderAdapter<B: HevcDecoderBackend>(pub B);

#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
))]
impl<B: HevcDecoderBackend> HevcDecoderAdapter<B> {
    /// Feed one Annex-B access unit. Defers to the backend; the unsafe
    /// libavcodec calls live one layer below this adapter.
    pub fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        self.0.feed_packet(packet, pts_us)
    }

    /// Pull a decoded NV12 frame if one is ready.
    pub fn drain_frame(&mut self) -> Result<Option<Nv12Frame>, DecodeError> {
        self.0.drain_frame()
    }

    pub fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }
}

#[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
pub type HevcSwFfmpegDecoderAdapter = HevcDecoderAdapter<HevcSwFfmpegDecoder>;
#[cfg(feature = "ffmpeg-decode-hevc-vaapi-any")]
pub type HevcVaapiFfmpegDecoderAdapter = HevcDecoderAdapter<HevcVaapiFfmpegDecoder>;
#[cfg(feature = "ffmpeg-decode-hevc-nvdec-any")]
pub type HevcNvdecFfmpegDecoderAdapter = HevcDecoderAdapter<HevcNvdecFfmpegDecoder>;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    #[test]
    fn adapter_satisfies_encoder_trait_bound() {
        // Compile-time assertion: HevcVaapiFfmpegEncoderAdapter implements Encoder<Frame=I420Frame>.
        fn _accepts_encoder<E: Encoder<Frame = I420Frame>>(_e: &mut E) {}
        let _ = std::marker::PhantomData::<HevcVaapiFfmpegEncoderAdapter>;
    }

    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    #[test]
    fn nvenc_adapter_satisfies_encoder_trait_bound() {
        // Compile-time assertion: HevcNvencFfmpegEncoderAdapter implements Encoder<Frame=I420Frame>.
        fn _accepts_encoder<E: Encoder<Frame = I420Frame>>(_e: &mut E) {}
        let _ = std::marker::PhantomData::<HevcNvencFfmpegEncoderAdapter>;
    }

    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    #[test]
    fn nvenc_npp_adapter_satisfies_encoder_trait_bound() {
        // Compile-time assertion: HevcNvencNppFfmpegEncoderAdapter implements Encoder<Frame=BgraFrame>.
        fn _accepts_encoder<E: Encoder<Frame = BgraFrame>>(_e: &mut E) {}
        let _ = std::marker::PhantomData::<HevcNvencNppFfmpegEncoderAdapter>;
    }

    #[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
    #[test]
    fn sw_decoder_adapter_compiles() {
        let _ = std::marker::PhantomData::<HevcSwFfmpegDecoderAdapter>;
    }
}
