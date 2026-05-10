//! NVIDIA NVENC encoder wrapper (Part B of Plan 2b).
//!
//! The `ffi` submodule is auto-generated from the NVIDIA Video Codec SDK
//! header `nvEncodeAPI.h`. Set `NV_CODEC_SDK_PATH` and rebuild to regenerate.
//!
//! Activation is gated on the build-time `prdt_nvenc_bindings` cfg, which
//! is set by `build.rs` only when the SDK was found and bindgen succeeded.
//! Without it, only the plain `NvencEncoderConfig` struct (used as a shared
//! config for the MF encoder too) is available; the FFI-backed types
//! (`NvencEncoder`, `NvEncLibrary`) are absent.

pub mod config;
#[cfg(prdt_nvenc_bindings)]
pub mod encoder;
#[cfg(prdt_nvenc_bindings)]
pub mod ffi;
#[cfg(prdt_nvenc_bindings)]
pub mod loader;

pub use config::NvencEncoderConfig;
#[cfg(prdt_nvenc_bindings)]
pub use encoder::NvencEncoder;
#[cfg(prdt_nvenc_bindings)]
pub use loader::NvEncLibrary;
