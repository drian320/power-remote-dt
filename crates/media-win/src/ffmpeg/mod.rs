//! Windows FFmpeg coexistence path (`media-win-ffmpeg` feature).
//!
//! PR1: skeleton module tree only. Encoder and decoder bodies land in PR2 / PR3.
//! This file exists so `cargo check --features media-win-ffmpeg` proves the
//! build.rs env-var wiring and rusty_ffmpeg dep resolve correctly before any
//! encoder/decoder code depends on them.

#[cfg(feature = "media-win-ffmpeg-nvenc-any")]
pub mod nvenc_encoder;
#[cfg(feature = "media-win-ffmpeg-nvenc-any")]
pub use nvenc_encoder::HevcNvencFfmpegEncoderWindowsAdapter;

#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
pub mod nvenc_main10_encoder;
#[cfg(feature = "media-win-ffmpeg-nvenc-main10-any")]
pub use nvenc_main10_encoder::HevcNvencMain10FfmpegEncoderWindowsAdapter;

// PR3 — NVDEC decoder modules.
#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
pub(crate) mod hdr10_sei_win;
#[cfg(feature = "media-win-ffmpeg-hdr10-any")]
pub mod hdr10_sidedata;
#[cfg(feature = "media-win-ffmpeg-hdr10-any")]
pub use hdr10_sidedata::Hdr10SidedataTracker;

#[cfg(feature = "media-win-ffmpeg-nvdec-any")]
pub mod nvdec_decoder;
#[cfg(feature = "media-win-ffmpeg-nvdec-any")]
pub use nvdec_decoder::{HevcNvdecFfmpegDecoderWindows, HevcNvdecFfmpegDecoderWindowsConfig};

#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
pub mod nvdec_main10_decoder;
#[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
pub use nvdec_main10_decoder::{
    HevcNvdecMain10FfmpegDecoderWindows, HevcNvdecMain10FfmpegDecoderWindowsConfig,
};
