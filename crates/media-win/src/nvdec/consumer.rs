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
use std::sync::{Arc, Mutex};

#[cfg(prdt_nvdec_bindings)]
use super::cuda::CudaContext;
#[cfg(prdt_nvdec_bindings)]
use super::decoder::{CuvidDecoder, DecodedFrame};
#[cfg(prdt_nvdec_bindings)]
use crate::d3d11::TextureFormat;
use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::MediaError;

/// Drop-in alternative to `MfD3d11Consumer` using nvcuvid.dll directly.
/// Construction creates a CUDA context and validates the driver path;
/// actual HEVC decoding via CUvideoparser + CUvideodecoder is Plan 2d
/// step 2b (pending).
pub struct NvdecD3d11Consumer {
    #[cfg(prdt_nvdec_bindings)]
    _ctx: Arc<CudaContext>,
    #[cfg(prdt_nvdec_bindings)]
    decoder: CuvidDecoder,
    /// Cached NV12 D3D11 texture we reuse across frames. Lazily created
    /// on the first `take_latest_texture` call once the decoder's
    /// actual output size is known.
    #[cfg(prdt_nvdec_bindings)]
    nv12_cache: Mutex<Option<D3d11Texture>>,
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}

impl NvdecD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        #[cfg(prdt_nvdec_bindings)]
        {
            let ctx = Arc::new(CudaContext::create_primary()?);
            let decoder = CuvidDecoder::new_hevc(Arc::clone(&ctx), dev.clone(), width, height)?;
            tracing::info!(
                width,
                height,
                "NVDEC: CUDA context + HEVC parser/decoder ready",
            );
            Ok(Self {
                _ctx: ctx,
                decoder,
                nv12_cache: Mutex::new(None),
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

    /// Pop the latest decoded NV12 frame as raw CPU bytes. Tests use
    /// this to verify pixel-level correctness; the production viewer
    /// uses `take_latest_texture` instead.
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU texture, if any. Mirrors
    /// `MfD3d11Consumer::take_latest_texture` so viewer code can be
    /// decoder-agnostic. Uploads the latest CPU NV12 bytes into a
    /// cached NV12 D3D11 texture via UpdateSubresource and returns a
    /// clone. Must be called on the thread that owns the D3D11
    /// immediate context (i.e., the viewer's event-loop thread).
    pub fn take_latest_texture(&self) -> Option<D3d11Texture> {
        #[cfg(prdt_nvdec_bindings)]
        {
            let frame = self.decoder.take_latest_frame()?;
            match self.upload_nv12_to_cache(&frame) {
                Ok(tex) => Some(tex),
                Err(e) => {
                    tracing::warn!(%e, "NVDEC: D3D11 NV12 upload failed");
                    None
                }
            }
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            None
        }
    }

    #[cfg(prdt_nvdec_bindings)]
    fn upload_nv12_to_cache(&self, frame: &DecodedFrame) -> Result<D3d11Texture, MediaError> {
        use windows::Win32::Graphics::Direct3D11::D3D11_BOX;

        let mut slot = self.nv12_cache.lock().unwrap();
        let cache_needs_rebuild = match slot.as_ref() {
            Some(t) => t.width() != frame.width || t.height() != frame.height,
            None => true,
        };
        if cache_needs_rebuild {
            *slot = Some(D3d11Texture::new_default(
                &self._dev,
                frame.width,
                frame.height,
                TextureFormat::Nv12,
            )?);
        }
        let tex = slot.as_ref().unwrap().clone();

        // Upload Y (subresource 0, width x height) and UV (subresource 1,
        // width x height/2) from the packed CPU NV12 buffer. For NV12
        // textures D3D11 accepts two UpdateSubresource calls against
        // subresource 0 and 1 with the appropriate row pitches.
        let w = frame.width as usize;
        let h = frame.height as usize;
        let y_plane_ptr = frame.nv12.as_ptr();
        let uv_plane_ptr = unsafe { y_plane_ptr.add(w * h) };
        let y_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: frame.width,
            bottom: frame.height,
            back: 1,
        };
        let uv_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: frame.width,
            bottom: frame.height / 2,
            back: 1,
        };
        self._dev.with_context(|ctx| {
            use windows::core::Interface;
            let res: windows::Win32::Graphics::Direct3D11::ID3D11Resource =
                tex.raw().cast().expect("NV12 tex is ID3D11Resource");
            unsafe {
                ctx.UpdateSubresource(
                    &res,
                    0,
                    Some(&y_box),
                    y_plane_ptr as *const _,
                    frame.width,
                    0,
                );
                ctx.UpdateSubresource(
                    &res,
                    1,
                    Some(&uv_box),
                    uv_plane_ptr as *const _,
                    frame.width,
                    0,
                );
            }
        });
        Ok(tex)
    }
}

#[async_trait::async_trait]
impl VideoConsumer for NvdecD3d11Consumer {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError> {
        #[cfg(prdt_nvdec_bindings)]
        {
            self.decoder
                .submit(&frame.nal_units, frame.timestamp_host_us as i64)
                .map_err(|e| ConsumerError::Decode(e.to_string()))
        }
        #[cfg(not(prdt_nvdec_bindings))]
        {
            let _ = frame;
            Err(ConsumerError::Decode(
                "NvdecD3d11Consumer not available (CUDA_PATH was unset at build time)".into(),
            ))
        }
    }

    fn needs_idr(&self) -> bool {
        // Parser handles IDR detection internally; ask caller for one on
        // construction so the decoder sees a keyframe before any deltas.
        false
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
                Ok(_c) => { /* CUDA context + parser + decoder came up cleanly */ }
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

    /// End-to-end: encode a synthetic frame with NVENC, feed the resulting
    /// HEVC NAL stream into the NVDEC consumer, and confirm that a
    /// decoded NV12 buffer of the expected dimensions comes out. This is
    /// the narrowest useful test for Plan 2d step 2b — it proves the
    /// parser + decoder + CPU-copy path works end-to-end against a real
    /// NVIDIA driver.
    #[cfg(prdt_nvdec_bindings)]
    #[test]
    fn decode_single_nvenc_frame_round_trip() {
        use crate::nvenc::{NvencEncoder, NvencEncoderConfig};
        use crate::synthetic::make_counter_texture;

        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        if !adapter.is_nvidia() {
            eprintln!("skipping: non-NVIDIA adapter");
            return;
        }
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let (w, h) = (256u32, 256u32);

        // Encode one IDR + one P-frame so the decoder has enough to
        // actually emit a display picture.
        let enc = NvencEncoder::new(
            &dev,
            &NvencEncoderConfig {
                width: w,
                height: h,
                fps_numerator: 60,
                fps_denominator: 1,
                bitrate_bps: 5_000_000,
                gop_length: 30,
            },
        )
        .expect("NvencEncoder");

        let mut consumer = NvdecD3d11Consumer::new(&dev, w, h).expect("NvdecD3d11Consumer");

        // NVENC needs a few frames to flush the pipeline; push 5.
        for i in 0..5 {
            let tex = make_counter_texture(&dev, w, h, i).expect("counter tex");
            let ts = i as u64 * 16_666;
            let force_idr = i == 0;
            let frame = enc.encode(&tex, force_idr, ts).expect("encode");
            consumer
                .decoder
                .submit(&frame.nal_bytes, ts as i64)
                .unwrap_or_else(|e| panic!("submit frame {i} failed: {e}"));
        }

        // take_latest_texture returns a fully populated D3D11 NV12
        // texture — the path the viewer actually exercises.
        let gpu = consumer
            .take_latest_texture()
            .expect("NVDEC should have produced at least one NV12 texture");
        assert_eq!(gpu.width(), w);
        assert_eq!(gpu.height(), h);
    }
}
