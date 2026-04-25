//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod dxgi;
pub mod error;
pub mod mf;
pub mod nvdec;
pub mod nvenc;
pub mod pipeline;
pub mod platform;
pub mod synthetic;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, DualPlaneYuvRenderer, Nv12Renderer, SwapChain, TextureFormat};
pub use dxgi::{enumerate_outputs_for_adapter, AcquiredFrame, DesktopDuplication, OutputInfo};
pub use error::{MediaError, Result};
pub use mf::H265Decoder;
pub use nvdec::NvdecD3d11Consumer;
pub use nvenc::{EncodedH265Frame, NvEncLibrary, NvencEncoder, NvencEncoderConfig};
pub use pipeline::{DxgiNvencProducer, MfD3d11Consumer};
pub use platform::MmcssScope;
