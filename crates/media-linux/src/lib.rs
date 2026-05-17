//! Linux media backend — XShm capture + OpenH264 SW encode/decode +
//! VideoProducer adapter. See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.
//!
//! The crate compiles to an empty library on non-Linux targets.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod capture_source;
pub mod core_adapter;
pub mod error;
pub mod frame;
pub mod i420_to_bgra;
pub mod linux_sw_producer;
pub mod policy;
pub mod sw_pipeline;
#[cfg(feature = "vaapi-h264")]
pub mod vaapi_pipeline;
pub mod wayland_portal;
pub mod x11_capture;

pub use capture_source::{CaptureSource, CaptureSourceError};
pub use error::LinuxMediaError;
pub use frame::BgraFrame;
#[cfg(feature = "vaapi-h264")]
pub use vaapi_pipeline::{LinuxVaapiEncoder, VaapiVideoProducer};

/// Production wiring entry point — host calls this to obtain a boxed
/// `VideoProducer` for the Linux SW path. The capture source is injected
/// (the factory picks X11 or Wayland-portal); width/height come from the
/// capture source via `geometry()`.
#[cfg(target_os = "linux")]
pub fn build_video_producer_with(
    capture: Box<dyn CaptureSource>,
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let (w, h) = capture.geometry();
    let enc =
        sw_pipeline::LinuxSwEncoder::new(w, h, bitrate_bps, fps).context("LinuxSwEncoder::new")?;
    linux_sw_producer::LinuxSwProducer::new(capture, enc, fps).context("LinuxSwProducer::new")
}

/// Legacy entry point — X11-only convenience wrapper retained so callers
/// that still want X11 explicitly (smoke tests, the
/// `build_video_decoder`-paired helper) don't have to assemble the
/// `Box<dyn CaptureSource>` themselves. Internally equivalent to
/// `build_video_producer_with(Box::new(X11ShmCapturer::new()?), ...)`.
#[cfg(target_os = "linux")]
pub fn build_video_producer(
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let cap = x11_capture::X11ShmCapturer::new().context("X11ShmCapturer::new")?;
    build_video_producer_with(Box::new(cap), bitrate_bps, fps)
}

/// VAAPI counterpart to `build_video_producer_with`. Constructs a
/// `LinuxVaapiEncoder` from the given (width, height, bitrate, fps) and
/// returns a `VaapiVideoProducer`.
///
/// Unlike the SW path which can rely on `capture.geometry()` (the
/// X11ShmCapturer knows its size at construction time), the WaylandPortal
/// capturer reports `(0, 0)` until the pipewire stream completes format
/// negotiation — which happens AFTER the encoder needs to allocate its
/// fixed-size surface pool. We therefore pass the explicit dimensions
/// from the handshake-negotiated `ProducerConfig` (i.e. the resolution
/// the viewer requested). If the compositor later negotiates a different
/// frame size, the producer's `resize_warned` path logs it; full
/// renegotiation lands in a follow-up.
#[cfg(all(target_os = "linux", feature = "vaapi-h264"))]
pub fn build_vaapi_video_producer_with(
    capture: Box<dyn CaptureSource>,
    width: u32,
    height: u32,
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<vaapi_pipeline::VaapiVideoProducer> {
    use anyhow::Context as _;
    let enc = vaapi_pipeline::LinuxVaapiEncoder::new(width, height, bitrate_bps, fps)
        .context("LinuxVaapiEncoder::new")?;
    vaapi_pipeline::VaapiVideoProducer::new(capture, enc, fps).context("VaapiVideoProducer::new")
}

/// Production wiring entry point — viewer calls this to obtain a SW decoder.
#[cfg(target_os = "linux")]
pub fn build_video_decoder() -> anyhow::Result<sw_pipeline::LinuxSwDecoder> {
    sw_pipeline::LinuxSwDecoder::new().map_err(Into::into)
}

// ────────────────────────────────────────────────────────────────────────────
// P2 — FFmpeg HEVC decode factories. Mirror the encoder-side build
// functions; gated per backend so the viewer crate can subscribe to any
// subset (sw-only, vaapi-only, nvdec-only, all three) without dragging
// in unused FFI symbols. Each returns the `*Adapter` type from
// prdt-media-ffmpeg so the viewer doesn't need a direct dep on the
// crate behind these forwards.
// ────────────────────────────────────────────────────────────────────────────

// Re-export the adapter types + Nv12Frame so the viewer can name them
// through the existing prdt_media_linux dep edge without taking a direct
// dependency on prdt-media-ffmpeg (which is opt-in here per the feature
// graph in crates/media-linux/Cargo.toml).
#[cfg(all(
    target_os = "linux",
    any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    )
))]
pub use prdt_media_ffmpeg::{HevcDecoderAdapter, HevcDecoderBackend};
// Nv12Frame lives in prdt-media-core; re-export through here so the
// viewer can name it without a direct media-core dep.
#[cfg(all(
    target_os = "linux",
    any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    )
))]
pub use prdt_media_core::Nv12Frame;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-nvdec-any"))]
pub use prdt_media_ffmpeg::HevcNvdecFfmpegDecoderAdapter;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-sw-any"))]
pub use prdt_media_ffmpeg::HevcSwFfmpegDecoderAdapter;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-vaapi-any"))]
pub use prdt_media_ffmpeg::HevcVaapiFfmpegDecoderAdapter;
// P3.2 — Main10 decode re-exports.
#[cfg(all(
    target_os = "linux",
    any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any",
    )
))]
pub use prdt_media_core::Nv12Frame16;
#[cfg(all(
    target_os = "linux",
    any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any",
    )
))]
pub use prdt_media_ffmpeg::HevcDecoderBackend10;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-nvdec-main10-any"))]
pub use prdt_media_ffmpeg::HevcNvdecMain10FfmpegDecoder;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-sw-main10-any"))]
pub use prdt_media_ffmpeg::HevcSwMain10FfmpegDecoder;
#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-vaapi-main10-any"))]
pub use prdt_media_ffmpeg::HevcVaapiMain10FfmpegDecoder;

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-sw-any"))]
pub fn build_ffmpeg_sw_hevc_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcSwFfmpegDecoderAdapter> {
    use anyhow::Context as _;
    let dec =
        prdt_media_ffmpeg::HevcSwFfmpegDecoder::new(prdt_media_ffmpeg::HevcSwFfmpegDecoderConfig {
            width,
            height,
        })
        .context("HevcSwFfmpegDecoder::new")?;
    // HevcSwFfmpegDecoderAdapter is a type alias for HevcDecoderAdapter<B>;
    // construct the generic directly because type aliases aren't callable.
    Ok(prdt_media_ffmpeg::HevcDecoderAdapter(dec))
}

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-vaapi-any"))]
pub fn build_ffmpeg_vaapi_hevc_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcVaapiFfmpegDecoderAdapter> {
    use anyhow::Context as _;
    let dec = prdt_media_ffmpeg::HevcVaapiFfmpegDecoder::new(
        prdt_media_ffmpeg::HevcVaapiFfmpegDecoderConfig {
            width,
            height,
            render_node: None,
        },
    )
    .context("HevcVaapiFfmpegDecoder::new")?;
    Ok(prdt_media_ffmpeg::HevcDecoderAdapter(dec))
}

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-nvdec-any"))]
pub fn build_ffmpeg_nvdec_hevc_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcNvdecFfmpegDecoderAdapter> {
    use anyhow::Context as _;
    let dec = prdt_media_ffmpeg::HevcNvdecFfmpegDecoder::new(
        prdt_media_ffmpeg::HevcNvdecFfmpegDecoderConfig {
            width,
            height,
            cuda_device_index: None,
        },
    )
    .context("HevcNvdecFfmpegDecoder::new")?;
    Ok(prdt_media_ffmpeg::HevcDecoderAdapter(dec))
}

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-sw-main10-any"))]
pub fn build_ffmpeg_sw_hevc_main10_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcSwMain10FfmpegDecoder> {
    use anyhow::Context as _;
    prdt_media_ffmpeg::HevcSwMain10FfmpegDecoder::new(
        prdt_media_ffmpeg::HevcSwMain10FfmpegDecoderConfig { width, height },
    )
    .context("HevcSwMain10FfmpegDecoder::new")
}

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-vaapi-main10-any"))]
pub fn build_ffmpeg_vaapi_hevc_main10_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcVaapiMain10FfmpegDecoder> {
    use anyhow::Context as _;
    prdt_media_ffmpeg::HevcVaapiMain10FfmpegDecoder::new(
        prdt_media_ffmpeg::HevcVaapiMain10FfmpegDecoderConfig {
            width,
            height,
            render_node: None,
        },
    )
    .context("HevcVaapiMain10FfmpegDecoder::new")
}

#[cfg(all(target_os = "linux", feature = "ffmpeg-decode-hevc-nvdec-main10-any"))]
pub fn build_ffmpeg_nvdec_hevc_main10_decoder(
    width: u32,
    height: u32,
) -> anyhow::Result<prdt_media_ffmpeg::HevcNvdecMain10FfmpegDecoder> {
    use anyhow::Context as _;
    prdt_media_ffmpeg::HevcNvdecMain10FfmpegDecoder::new(
        prdt_media_ffmpeg::HevcNvdecMain10FfmpegDecoderConfig {
            width,
            height,
            cuda_device_index: None,
        },
    )
    .context("HevcNvdecMain10FfmpegDecoder::new")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A12.a regression-guard: `build_video_decoder()` must still
    /// construct a `LinuxSwDecoder` after the P2 factory additions.
    /// Verifies the existing H.264 decode path is not broken by the new
    /// FFmpeg HEVC factory functions added alongside it.
    #[test]
    fn build_video_decoder_constructs_linux_sw_decoder() {
        // If this compiles and doesn't panic, the OpenH264 SW decoder path
        // is intact. The return type is LinuxSwDecoder (type-checked by rustc).
        let _dec = build_video_decoder().expect("build_video_decoder must succeed");
    }
}
