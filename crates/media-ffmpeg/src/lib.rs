//! FFmpeg-backed media codecs (HW only). LGPL dynamic-link to system libavcodec.
//! No SW codecs here — those live in `prdt-media-sw` (OpenH264).
//!
//! Exception: P2 added a SW HEVC decoder (`hevc_sw_decoder`) wrapping
//! libavcodec's portable `hevc` decoder. That's not a sound transition
//! ("SW codecs go to prdt-media-sw") but P2 needed a universal fallback
//! that shares the FFmpeg decode scaffolding (parser, packet plumbing,
//! NV12 carrier) with the two HW decoders. Keeping it here avoids
//! pulling rusty_ffmpeg into prdt-media-sw for one codec entry.

#![cfg_attr(
    all(feature = "ffmpeg", target_os = "linux"),
    deny(clippy::undocumented_unsafe_blocks)
)]
// hwdevice/hwframes/options are pub(crate) and only consumed by hevc_vaapi_encoder
// (Task #5). Allow dead_code until the encoder implementation is in place.
#![cfg_attr(all(feature = "ffmpeg", target_os = "linux"), allow(dead_code))]

#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-encode-hevc-vaapi' is not available on this target (Linux-only in P1)"
);

#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-encode-hevc-nvenc' is not available on this target (Linux-only in P1.5; \
     Windows already has native NVENC via media-win)"
);

#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-npp-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-encode-hevc-nvenc-npp' is not available on this target (Linux-only in P2.5)"
);

// Per A11: the NPP marker should always be enabled via one of the three
// -npp{,-ffmpeg5,-ffmpeg7} variants, each of which transitively enables the
// matching NVENC ABI variant. This arm catches catastrophic feature graph
// corruption (NPP marker set without an ABI variant).
#[cfg(all(
    feature = "ffmpeg-encode-hevc-nvenc-npp-any",
    not(any(
        feature = "ffmpeg-encode-hevc-nvenc-npp",
        feature = "ffmpeg-encode-hevc-nvenc-npp-ffmpeg5",
        feature = "ffmpeg-encode-hevc-nvenc-npp-ffmpeg7",
    )),
))]
compile_error!(
    "ffmpeg-encode-hevc-nvenc-npp-any was force-enabled without any NPP ABI variant; \
     enable one of ffmpeg-encode-hevc-nvenc-npp{,-ffmpeg5,-ffmpeg7}"
);

#[cfg(all(feature = "ffmpeg-decode-hevc-sw-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-decode-hevc-sw' is not available on this target (Linux-only in P2; \
     Windows already has Media Foundation HEVC decode via media-win)"
);

#[cfg(all(feature = "ffmpeg-decode-hevc-vaapi-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-decode-hevc-vaapi' is not available on this target (Linux-only in P2)"
);

#[cfg(all(feature = "ffmpeg-decode-hevc-nvdec-any", not(target_os = "linux")))]
compile_error!(
    "feature 'ffmpeg-decode-hevc-nvdec' is not available on this target (Linux-only in P2; \
     Windows already has NVDEC HEVC decode via media-win)"
);

pub mod error;

#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any",
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
pub mod core_adapter;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-nvenc-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
mod cuda_hwdevice;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-nvenc-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
mod cuda_hwframes;
#[cfg(all(
    any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
pub mod decoder_common;
#[cfg(all(feature = "ffmpeg-decode-hevc-nvdec-any", target_os = "linux"))]
pub mod hevc_nvdec_decoder;
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", target_os = "linux"))]
pub mod hevc_nvenc_encoder;
#[cfg(all(feature = "ffmpeg-decode-hevc-sw-any", target_os = "linux"))]
pub mod hevc_sw_decoder;
#[cfg(all(feature = "ffmpeg-decode-hevc-vaapi-any", target_os = "linux"))]
pub mod hevc_vaapi_decoder;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi-any", target_os = "linux"))]
pub mod hevc_vaapi_encoder;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-vaapi-any"
    ),
    target_os = "linux"
))]
mod hwdevice;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-vaapi-any"
    ),
    target_os = "linux"
))]
mod hwframes;
#[cfg(all(
    any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ),
    target_os = "linux"
))]
mod options;

pub use error::FfmpegError;

#[cfg(all(
    any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
pub use core_adapter::HevcDecoderAdapter;
#[cfg(all(feature = "ffmpeg-decode-hevc-nvdec-any", target_os = "linux"))]
pub use core_adapter::HevcNvdecFfmpegDecoderAdapter;
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", target_os = "linux"))]
pub use core_adapter::HevcNvencFfmpegEncoderAdapter;
#[cfg(all(feature = "ffmpeg-decode-hevc-sw-any", target_os = "linux"))]
pub use core_adapter::HevcSwFfmpegDecoderAdapter;
#[cfg(all(feature = "ffmpeg-decode-hevc-vaapi-any", target_os = "linux"))]
pub use core_adapter::HevcVaapiFfmpegDecoderAdapter;
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi-any", target_os = "linux"))]
pub use core_adapter::HevcVaapiFfmpegEncoderAdapter;
#[cfg(all(
    any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ),
    target_os = "linux"
))]
pub use decoder_common::HevcDecoderBackend;
#[cfg(all(feature = "ffmpeg-decode-hevc-nvdec-any", target_os = "linux"))]
pub use hevc_nvdec_decoder::{HevcNvdecFfmpegDecoder, HevcNvdecFfmpegDecoderConfig};
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", target_os = "linux"))]
pub use hevc_nvenc_encoder::{HevcNvencFfmpegEncoder, HevcNvencFfmpegEncoderConfig};
#[cfg(all(feature = "ffmpeg-decode-hevc-sw-any", target_os = "linux"))]
pub use hevc_sw_decoder::{HevcSwFfmpegDecoder, HevcSwFfmpegDecoderConfig};
#[cfg(all(feature = "ffmpeg-decode-hevc-vaapi-any", target_os = "linux"))]
pub use hevc_vaapi_decoder::{HevcVaapiFfmpegDecoder, HevcVaapiFfmpegDecoderConfig};
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi-any", target_os = "linux"))]
pub use hevc_vaapi_encoder::{HevcVaapiFfmpegEncoder, HevcVaapiFfmpegEncoderConfig};
