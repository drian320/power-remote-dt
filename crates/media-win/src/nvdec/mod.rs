//! Plan 2d: NVDEC (cuvid) H.265 decoder as an alternative to the MF-based
//! `MfD3d11Consumer`. Same `VideoConsumer` trait, same zero-copy D3D11
//! texture output, but sheds the Media Foundation shim (`IMFTransform` +
//! `IMFDXGIDeviceManager` + `IMFSample`) in favor of calling nvcuvid.dll
//! directly.
//!
//! Activation is gated on the build-time `prdt_nvdec_bindings` cfg, which
//! `build.rs` emits only when BOTH env vars are set:
//!   NV_CODEC_SDK_PATH  → provides Interface/nvcuvid.h, cuviddec.h
//!   CUDA_PATH          → provides include/cuda.h (CUDA Toolkit 13.x)
//!
//! When those aren't set the crate still builds; `NvdecD3d11Consumer::new`
//! just returns `MediaError::Other("NVDEC not available ...")` so callers
//! that opt in via `--decoder nvdec` get a clear explanation instead of a
//! link error.

pub mod consumer;
#[cfg(prdt_nvdec_bindings)]
pub mod cuda;
pub mod ffi;

pub use consumer::NvdecD3d11Consumer;
