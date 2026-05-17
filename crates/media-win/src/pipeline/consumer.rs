//! MfD3d11Consumer - Media Foundation H.265 decode. Stores the latest
//! decoded NV12 GPU texture in a mutex for test/inspection; Plan 3's viewer
//! bin will pull these textures and present them via D3D11 swapchain.

use std::sync::{Arc, Mutex};

use prdt_protocol::{ConsumerError, EncodedFrame, VideoConsumer};

use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::MediaError;
use crate::mf::H265Decoder;

pub struct MfD3d11Consumer {
    decoder: H265Decoder,
    /// Kept for backward compatibility with byte-oriented call sites; the
    /// texture path is now the default and this field is no longer populated
    /// by `submit`. Plan 3+ callers should prefer `take_latest_texture`.
    latest_output: Arc<Mutex<Option<Vec<u8>>>>,
    latest_texture: Arc<Mutex<Option<D3d11Texture>>>,
    needs_idr: bool,
}

impl MfD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        let decoder = H265Decoder::new(dev, width, height)?;
        Ok(Self {
            decoder,
            latest_output: Default::default(),
            latest_texture: Default::default(),
            needs_idr: true,
        })
    }

    /// Deprecated CPU-readback path. Since Plan 3 Task 2 the consumer pulls
    /// textures directly via `process_output_texture`, so this field is no
    /// longer populated and always returns `None`. Retained only so existing
    /// type-level references do not break; prefer `take_latest_texture`.
    pub fn take_latest_frame(&self) -> Option<Vec<u8>> {
        self.latest_output.lock().unwrap().take()
    }

    /// Consume the latest decoded GPU texture (takes ownership, leaves None).
    /// Returns `None` if no frame has been decoded yet or if the previous one
    /// was already consumed.
    pub fn take_latest_texture(&self) -> Option<D3d11Texture> {
        self.latest_texture.lock().unwrap().take()
    }

    /// Subresource index returned by the MFT for the most recent decoded
    /// frame. See `H265Decoder::last_subresource_index` for meaning.
    pub fn last_subresource_index(&self) -> u32 {
        self.decoder.last_subresource_index()
    }
}

// region: 8-bit-mf-consumer
//
// Protected by scripts/check-8bit-helpers-byte-identity.py.
// DO NOT modify any line between these markers without an explicit
// RALPLAN-DR review that updates the byte-identity guard.

// H265Decoder holds an IMFTransform (COM, !Send by default in the
// `windows` crate). MFTs are thread-agnostic as long as we don't drive them
// concurrently from multiple threads — which we don't: submit() takes
// &mut self. Mark Send so we satisfy the `VideoConsumer: Send` bound.
unsafe impl Send for MfD3d11Consumer {}

#[async_trait::async_trait]
impl VideoConsumer for MfD3d11Consumer {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError> {
        // MF expects timestamps in 100ns units. We use frame.timestamp_host_us * 10.
        let ts_hns = (frame.timestamp_host_us as i64).saturating_mul(10);
        self.decoder
            .process_input(&frame.nal_units, ts_hns)
            .map_err(|e| ConsumerError::Decode(e.to_string()))?;

        // Drain available outputs, zero-copy into ID3D11Texture2D.
        for _ in 0..5 {
            match self
                .decoder
                .process_output_texture()
                .map_err(|e| ConsumerError::Decode(e.to_string()))?
            {
                Some(tex) => {
                    *self.latest_texture.lock().unwrap() = Some(tex);
                    self.needs_idr = false;
                }
                None => break,
            }
        }
        Ok(())
    }

    fn needs_idr(&self) -> bool {
        self.needs_idr || self.decoder.needs_idr()
    }
}

// endregion: 8-bit-mf-consumer

// ============================================================================
// F8 — MfHevcMain10Consumer (sibling of MfD3d11Consumer)
// ============================================================================

#[cfg(feature = "media-win-hevc-main10")]
pub struct MfHevcMain10Consumer {
    decoder: crate::mf::MfHevcMain10Decoder,
    /// On D3D11VA path: holds (GPU P010 texture, hdr10_metadata).
    /// On SW fallback: holds the uploaded texture from CpuP010Uploader + per-frame metadata.
    latest: Arc<Mutex<Option<(D3d11Texture, Option<prdt_media_core::Hdr10Metadata>)>>>,
    /// Only `Some` on the SW fallback path (when `decoder.d3d11_aware() == false`).
    sw_uploader: Option<crate::CpuP010Uploader>,
    needs_idr: bool,
}

#[cfg(feature = "media-win-hevc-main10")]
impl MfHevcMain10Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        let decoder = crate::mf::MfHevcMain10Decoder::new(dev, width, height)?;
        let sw_uploader = if decoder.d3d11_aware() {
            None
        } else {
            Some(crate::CpuP010Uploader::new(dev, width, height)?)
        };
        Ok(Self {
            decoder,
            latest: Default::default(),
            sw_uploader,
            needs_idr: true,
        })
    }

    /// Consume the latest decoded (texture, hdr10) pair.
    pub fn take_latest(&self) -> Option<(D3d11Texture, Option<prdt_media_core::Hdr10Metadata>)> {
        self.latest.lock().unwrap().take()
    }
}

// SAFETY: MfHevcMain10Decoder holds an IMFTransform (COM, !Send by default).
// MFTs are thread-agnostic as long as they are not driven concurrently —
// which they are not: submit() takes &mut self.
#[cfg(feature = "media-win-hevc-main10")]
unsafe impl Send for MfHevcMain10Consumer {}

#[cfg(feature = "media-win-hevc-main10")]
#[async_trait::async_trait]
impl VideoConsumer for MfHevcMain10Consumer {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError> {
        let ts_hns = (frame.timestamp_host_us as i64).saturating_mul(10);
        self.decoder
            .process_input(&frame.nal_units, ts_hns)
            .map_err(|e| ConsumerError::Decode(e.to_string()))?;

        for _ in 0..5 {
            if self.decoder.d3d11_aware() {
                // D-1 path: zero-copy GPU texture (R13-isolated by CopyResource).
                match self
                    .decoder
                    .process_output_texture_p010()
                    .map_err(|e| ConsumerError::Decode(e.to_string()))?
                {
                    Some((tex, hdr10)) => {
                        *self.latest.lock().unwrap() = Some((tex, hdr10));
                        self.needs_idr = false;
                    }
                    None => break,
                }
            } else {
                // D-2 SW fallback: CPU Nv12Frame16 → CpuP010Uploader → GPU texture.
                match self
                    .decoder
                    .process_output_nv12_16()
                    .map_err(|e| ConsumerError::Decode(e.to_string()))?
                {
                    Some(nv12_frame) => {
                        let hdr10 = nv12_frame.hdr10;
                        let tex = self
                            .sw_uploader
                            .as_mut()
                            .expect("sw_uploader must be Some when !d3d11_aware")
                            .upload(&nv12_frame)
                            .map_err(|e| ConsumerError::Decode(format!("upload: {e}")))?;
                        *self.latest.lock().unwrap() = Some((tex, hdr10));
                        self.needs_idr = false;
                    }
                    None => break,
                }
            }
        }
        Ok(())
    }

    fn needs_idr(&self) -> bool {
        self.needs_idr || self.decoder.needs_idr()
    }
}
