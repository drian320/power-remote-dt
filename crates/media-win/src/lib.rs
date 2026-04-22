//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod error;
pub mod synthetic;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, TextureFormat};
pub use error::{MediaError, Result};
