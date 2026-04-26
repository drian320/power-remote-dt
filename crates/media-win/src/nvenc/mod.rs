//! NVIDIA NVENC encoder wrapper (Part B of Plan 2b).
//!
//! The `ffi` submodule is auto-generated from the NVIDIA Video Codec SDK
//! header `nvEncodeAPI.h`. Set `NV_CODEC_SDK_PATH` and rebuild to regenerate.

pub mod config;
pub mod encoder;
pub mod ffi;
pub mod loader;

pub use config::NvencEncoderConfig;
pub use encoder::NvencEncoder;
pub use loader::NvEncLibrary;
