//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod core_adapter;
#[cfg(feature = "i420-upload")]
pub mod cpu_i420_upload;
pub mod d3d11;
pub mod dxgi;
pub mod encoder_trait;
pub mod error;
pub mod mf;
pub mod nvdec;
pub mod nvenc;
pub mod pipeline;
pub mod platform;
pub mod synthetic;

#[cfg(feature = "i420-upload")]
pub use cpu_i420_upload::CpuI420Uploader;

#[cfg(prdt_nvdec_bindings)]
pub use crate::nvdec::decoder::DualPlaneFrame;
pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{
    D3d11Device, D3d11Texture, DualPlaneYuvRenderer, Nv12Renderer, SwapChain, TextureFormat,
};
pub use dxgi::{enumerate_outputs_for_adapter, AcquiredFrame, DesktopDuplication, OutputInfo};
pub use encoder_trait::{EncodedH265Frame, Hevc265Encoder, HwHevcEncoder};
pub use error::{MediaError, Result};
pub use mf::{H265Decoder, MfH265Encoder};
pub use nvdec::NvdecD3d11Consumer;
pub use nvenc::NvencEncoderConfig;
#[cfg(prdt_nvenc_bindings)]
pub use nvenc::{NvEncLibrary, NvencEncoder};
#[cfg(prdt_nvenc_bindings)]
pub use pipeline::DxgiNvencProducer;
pub use pipeline::MfD3d11Consumer;
pub use platform::MmcssScope;
