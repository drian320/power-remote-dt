//! `NvdecD3d11Consumer` — placeholder wiring for the Plan 2d NVDEC decoder.
//!
//! When `prdt_nvdec_bindings` is set (build.rs generated real bindings),
//! this module will host the real CUvideodecoder + CUDA-D3D11 interop
//! code. Until CUDA Toolkit is installed on this dev machine, `new()`
//! returns a `NotAvailable` runtime error. Upstream callers (viewer main
//! behind a future `--decoder nvdec` flag) can fall back to MF gracefully.

use prdt_protocol::{ConsumerError, EncodedFrame, VideoConsumer};

use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::MediaError;

/// Drop-in alternative to `MfD3d11Consumer` using nvcuvid.dll directly.
/// Construction currently errors out when CUDA Toolkit isn't present; the
/// actual FFI lives behind the `prdt_nvdec_bindings` cfg.
pub struct NvdecD3d11Consumer {
    // Once the bindgen path is live, this struct holds:
    //   - CUcontext (pushed during each decode call)
    //   - CUvideodecoder
    //   - CUvideoparser (or direct submit without parser)
    //   - An ID3D11Texture2D ring bound via CUDA-D3D11 interop
    //   - Latest-texture slot like MfD3d11Consumer
    // For now we keep the slot so upstream can still build.
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}

impl NvdecD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        #[cfg(prdt_nvdec_bindings)]
        {
            // Real init lives here in a follow-up commit. For now even with
            // bindings present we bail so CI / dev builds behave identically
            // until the full FFI wiring lands.
            return Err(MediaError::Other(
                "NVDEC bindings are present but the CUcontext + CUvideodecoder \
                 wiring is not yet implemented — track Plan 2d step 2 in \
                 PHASE0-STATUS.md"
                    .into(),
            ));
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

    #[test]
    fn construction_without_cuda_returns_not_available() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return, // no adapter in headless CI — skip
        };
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let err = match NvdecD3d11Consumer::new(&dev, 1920, 1080) {
            Ok(_) => panic!("NvdecD3d11Consumer::new unexpectedly succeeded"),
            Err(e) => e,
        };
        match err {
            MediaError::Other(msg) => {
                assert!(
                    msg.contains("NVDEC")
                        && (msg.contains("not available") || msg.contains("not yet implemented")),
                    "unexpected error string: {msg}",
                );
            }
            other => panic!("expected MediaError::Other, got {other:?}"),
        }
    }
}
