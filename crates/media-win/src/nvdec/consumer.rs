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
use std::sync::Arc;

#[cfg(prdt_nvdec_bindings)]
use super::cuda::CudaContext;
#[cfg(prdt_nvdec_bindings)]
use super::decoder::{CuvidDecoder, DecodedFrame};
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
    _dev: D3d11Device,
    _width: u32,
    _height: u32,
}

impl NvdecD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        #[cfg(prdt_nvdec_bindings)]
        {
            let ctx = Arc::new(CudaContext::create_primary()?);
            let decoder = CuvidDecoder::new_hevc(Arc::clone(&ctx), width, height)?;
            tracing::info!(
                width,
                height,
                "NVDEC: CUDA context + HEVC parser/decoder ready (CPU output path; \
                 Plan 2d step 2c will add CUDA-D3D11 zero-copy)",
            );
            Ok(Self {
                _ctx: ctx,
                decoder,
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

    /// Plan 2d step 2b exposes the decoded NV12 bytes on the CPU side.
    /// Step 2c will swap this for a GPU-resident `D3d11Texture` via
    /// CUDA-D3D11 interop; for now `take_latest_texture` returns None.
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU texture, if any. Mirrors
    /// `MfD3d11Consumer::take_latest_texture` so viewer code can be
    /// decoder-agnostic behind a trait-object. Step 2c makes this
    /// return the real texture.
    pub fn take_latest_texture(&self) -> Option<D3d11Texture> {
        None
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
        let mut combined = Vec::<u8>::new();
        for i in 0..5 {
            let tex = make_counter_texture(&dev, w, h, i).expect("counter tex");
            let ts = i as u64 * 16_666;
            let force_idr = i == 0;
            let frame = enc.encode(&tex, force_idr, ts).expect("encode");
            combined.extend_from_slice(&frame.nal_bytes);
            let ts_i64 = ts as i64;
            let res = consumer.decoder.submit(&frame.nal_bytes, ts_i64);
            if let Err(e) = res {
                panic!("submit frame {i} failed: {e}");
            }
        }

        let got = consumer.take_latest_nv12();
        let got = got.expect("NVDEC should have produced at least one frame");
        assert_eq!(got.width, w);
        assert_eq!(got.height, h);
        // NV12 = width*height (Y) + width*height/2 (UV interleaved).
        assert_eq!(got.nv12.len() as u32, w * h * 3 / 2);
    }
}
