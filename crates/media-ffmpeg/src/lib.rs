//! FFmpeg-backed media codecs (HW only). LGPL dynamic-link to system libavcodec.
//! No SW codecs here — those live in `prdt-media-sw` (OpenH264).

#![cfg_attr(
    all(feature = "ffmpeg", target_os = "linux"),
    deny(clippy::undocumented_unsafe_blocks)
)]
// hwdevice/hwframes/options are pub(crate) and only consumed by hevc_vaapi_encoder
// (Task #5). Allow dead_code until the encoder implementation is in place.
#![cfg_attr(all(feature = "ffmpeg", target_os = "linux"), allow(dead_code))]

#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-encode-hevc-vaapi' is not available on this target (Linux-only in P1)"
);

#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-encode-hevc-nvenc' is not available on this target (Linux-only in P1.5; \
     Windows already has native NVENC via media-win)"
);

pub mod error;

#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
pub mod core_adapter;
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc", target_os = "linux"))]
mod cuda_hwdevice;
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc", target_os = "linux"))]
mod cuda_hwframes;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
pub mod hevc_vaapi_encoder;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
mod hwdevice;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
mod hwframes;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-vaapi",
        feature = "ffmpeg-encode-hevc-nvenc"
    ),
    target_os = "linux"
))]
mod options;

pub use error::FfmpegError;

#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
pub use core_adapter::HevcVaapiFfmpegEncoderAdapter;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi", target_os = "linux"))]
pub use hevc_vaapi_encoder::{HevcVaapiFfmpegEncoder, HevcVaapiFfmpegEncoderConfig};
