//! `NvdecD3d11Consumer` — Plan 2d NVDEC decoder wiring.
//!
//! With `prdt_nvdec_bindings` set (CUDA Toolkit + Video Codec SDK both
//! present at build time), this creates a primary CUDA context ready for
//! a CUvideoparser / CUvideodecoder pair. Until those wrappers land
//! (Plan 2d step 2b), `submit()` still errors out. The point of this
//! commit is to prove the CUDA context + D3D11 interop path comes up
//! cleanly, so the expensive-to-debug FFI bring-up can be done in small
//! verified steps instead of one monolithic 700-LOC diff.

use prdt_protocol::{ConsumerError, EncodedFrame, VideoConsumer};

#[cfg(prdt_nvdec_bindings)]
use super::cuda::CudaContext;
use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::MediaError;

/// Drop-in alternative to `MfD3d11Consumer` using nvcuvid.dll directly.
/// Construction creates a CUDA context and validates the driver path;
/// actual HEVC decoding via CUvideoparser + CUvideodecoder is Plan 2d
/// step 2b (pending).
pub struct NvdecD3d11Consumer {
    #[cfg(prdt_nvdec_bindings)]
    _ctx: CudaContext,
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}

impl NvdecD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        #[cfg(prdt_nvdec_bindings)]
        {
            let ctx = CudaContext::create_primary()?;
            tracing::info!(
                width,
                height,
                "NVDEC: CUDA context created; decoder pipeline stub (Plan 2d step 2b pending)",
            );
            Ok(Self {
                _ctx: ctx,
                _dev: dev.clone(),
                _width: width,
                _height: height,
            })
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            let _ = (dev, width, height);
            Err(MediaError::Other(
                "NVDEC not available: install CUDA Toolkit (set CUDA_PATH) and \
                 rebuild. Until then the viewer should use the default MF \
                 decoder (MfD3d11Consumer)."
                    .into(),
            ))
        }
    }

    /// Drain the latest decoded GPU texture, if any. Mirrors
    /// `MfD3d11Consumer::take_latest_texture` so viewer code can be
    /// decoder-agnostic behind a trait-object.
    pub fn take_latest_texture(&self) -> Option<D3d11Texture> {
        None
    }
}

#[async_trait::async_trait]
impl VideoConsumer for NvdecD3d11Consumer {
    async fn submit(&mut self, _frame: EncodedFrame) -> Result<(), ConsumerError> {
        Err(ConsumerError::Decode(
            "NvdecD3d11Consumer not yet implemented".into(),
        ))
    }

    fn needs_idr(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;

    /// When the Plan 2d bindings are present AND the dev box has an NVIDIA
    /// driver, construction should succeed (CUDA context creation is the
    /// only work `new` actually does right now — decode is still stubbed).
    /// When the bindings aren't compiled in, `new` must return a clear
    /// `NVDEC not available` error instead of silently doing nothing.
    #[test]
    fn construction_matches_feature_availability() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let result = NvdecD3d11Consumer::new(&dev, 1920, 1080);

        #[cfg(prdt_nvdec_bindings)]
        {
            match result {
                Ok(_c) => { /* CUDA context came up cleanly */ }
                Err(MediaError::Other(msg)) => {
                    // Only legitimate reason to fail with bindings present is
                    // "no CUDA device" — we treat that as skipped, not failed.
                    assert!(msg.contains("CUDA"), "unexpected error: {msg}",);
                    eprintln!("skipping: {msg}");
                }
                Err(other) => panic!("unexpected error: {other}"),
            }
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            let err = result.expect_err("new should fail without bindings");
            match err {
                MediaError::Other(msg) => {
                    assert!(
                        msg.contains("NVDEC") && msg.contains("not available"),
                        "unexpected error: {msg}",
                    );
                }
                other => panic!("expected MediaError::Other, got {other}"),
            }
        }
    }
}
