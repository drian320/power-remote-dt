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
#[cfg(all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12")))]
use super::decoder::DecodedFrame;
#[cfg(prdt_nvdec_bindings)]
use super::decoder::{CuvidDecoder, DualPlaneFrame};
use crate::d3d11::D3d11Device;
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
            let decoder = CuvidDecoder::new_hevc(Arc::clone(&ctx), dev.clone(), width, height)?;
            tracing::info!(
                width,
                height,
                "NVDEC: CUDA context + HEVC parser/decoder ready",
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

    /// Pop the latest decoded NV12 frame as raw CPU bytes. Test / opt-in
    /// feature path only — production viewer uses `take_latest_dual_plane`.
    #[cfg(all(prdt_nvdec_bindings, any(test, feature = "cpu-nv12")))]
    pub fn take_latest_nv12(&self) -> Option<DecodedFrame> {
        self.decoder.take_latest_frame()
    }

    /// Drain the latest decoded GPU dual-plane frame: a (R8 Y, R8G8 UV)
    /// D3D11 texture pair already populated via CUDA-D3D11 device-to-device
    /// copy. Mirrors `MfD3d11Consumer::take_latest_texture` shape but the
    /// downstream renderer is `DualPlaneYuvRenderer` rather than
    /// `Nv12Renderer`. Must be called on the thread that owns the D3D11
    /// immediate context (the viewer's event-loop thread).
    ///
    /// Returns `Arc<DualPlaneFrame>` so the underlying decoder can publish
    /// the frame lock-free via `arc-swap` without copying the texture
    /// handles. Cheap to clone via `Arc::clone` if multiple stages need
    /// to observe the same frame.
    #[cfg(prdt_nvdec_bindings)]
    pub fn take_latest_dual_plane(&self) -> Option<Arc<DualPlaneFrame>> {
        self.decoder.take_latest_dual_plane()
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
    #[cfg(prdt_nvdec_bindings)]
    use crate::d3d11::{D3d11Texture, TextureFormat};

    /// Probe: can we grab both Y and UV as separate CUarrays from an
    /// NV12 D3D11 texture registered with CUDA when the texture is
    /// created with SHADER_RESOURCE-only BindFlags? If yes, we can do
    /// true zero-copy output without needing a dual R8 + R8G8 path.
    #[cfg(prdt_nvdec_bindings)]
    #[test]
    fn probe_nv12_shader_resource_only_interop() {
        use super::super::cuda::{check, CudaContext};
        use super::super::ffi;
        use windows::core::Interface;

        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        if !adapter.is_nvidia() {
            return;
        }
        let dev = match D3d11Device::create(&adapter) {
            Ok(d) => d,
            Err(_) => return,
        };
        let ctx = match CudaContext::create_primary() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _g = ctx.push().expect("push");

        let tex = D3d11Texture::new_for_cuda_interop(&dev, 256, 256, TextureFormat::Nv12)
            .expect("NV12 interop tex");

        let mut cuda_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        unsafe {
            let res_ptr: *mut std::ffi::c_void = tex.raw().as_raw() as *mut _;
            match check(
                "cuGraphicsD3D11RegisterResource",
                ffi::cuGraphicsD3D11RegisterResource(&mut cuda_res, res_ptr as *mut _, 0),
            ) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("SHADER_RESOURCE-only NV12 registration failed: {e}");
                    return;
                }
            }

            let mut local = cuda_res;
            let map_r = ffi::cuGraphicsMapResources(1, &mut local, std::ptr::null_mut());
            assert!(
                map_r == ffi::cudaError_enum::CUDA_SUCCESS,
                "cuGraphicsMapResources failed: {}",
                map_r as u32
            );

            let mut y_array: ffi::CUarray = std::ptr::null_mut();
            let ry = ffi::cuGraphicsSubResourceGetMappedArray(&mut y_array, cuda_res, 0, 0);
            let mut uv_array: ffi::CUarray = std::ptr::null_mut();
            let ruv = ffi::cuGraphicsSubResourceGetMappedArray(&mut uv_array, cuda_res, 1, 0);

            let _ = ffi::cuGraphicsUnmapResources(1, &mut local, std::ptr::null_mut());
            let _ = ffi::cuGraphicsUnregisterResource(cuda_res);

            // Record outcome — this is a diagnostic probe, not a strict
            // assertion. If both planes come back as non-null CUarrays,
            // true zero-copy is achievable.
            eprintln!(
                "NV12 SHADER_RESOURCE-only interop probe: Y={} UV={}",
                if ry == ffi::cudaError_enum::CUDA_SUCCESS && !y_array.is_null() {
                    "OK"
                } else {
                    "FAIL"
                },
                if ruv == ffi::cudaError_enum::CUDA_SUCCESS && !uv_array.is_null() {
                    "OK"
                } else {
                    "FAIL"
                },
            );
        }
    }

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
            // NvdecD3d11Consumer holds raw NVDEC handles and does not derive
            // Debug, so `Result::expect_err` (which would format the Ok variant
            // in its panic message) cannot satisfy its `T: Debug` bound. Match
            // directly on the result instead.
            let err = match result {
                Ok(_) => panic!("new should fail without bindings"),
                Err(e) => e,
            };
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

    /// Probe: can we register R8 (Y) and R8G8 (UV) D3D11 textures with CUDA
    /// and pull a non-null CUarray for each? This validates the dual-plane
    /// zero-copy approach BEFORE we rewire the display callback. If this
    /// FAILs on the host's driver, the entire Plan 2d zero-copy strategy
    /// must be reconsidered — escalate rather than carrying on.
    #[cfg(prdt_nvdec_bindings)]
    #[test]
    fn dual_plane_textures_register_with_cuda() {
        use super::super::cuda::{check, CudaContext};
        use super::super::ffi;
        use windows::core::Interface;

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
        let ctx = match CudaContext::create_primary() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _g = ctx.push().expect("push");

        // Build the two textures: Y = R8 (W×H), UV = R8G8 (W/2 × H/2).
        let (w, h) = (256u32, 256u32);
        let y_tex = D3d11Texture::new_for_cuda_interop(&dev, w, h, TextureFormat::R8)
            .expect("Y R8 interop tex");
        let uv_tex = D3d11Texture::new_for_cuda_interop(&dev, w / 2, h / 2, TextureFormat::R8G8)
            .expect("UV R8G8 interop tex");

        // Register both with CUDA.
        let mut y_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        let mut uv_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        unsafe {
            let y_ptr: *mut std::ffi::c_void = y_tex.raw().as_raw() as *mut _;
            let uv_ptr: *mut std::ffi::c_void = uv_tex.raw().as_raw() as *mut _;
            check(
                "cuGraphicsD3D11RegisterResource(Y R8)",
                ffi::cuGraphicsD3D11RegisterResource(&mut y_res, y_ptr as *mut _, 0),
            )
            .expect("Y R8 register must succeed (Plan 2d hard requirement)");
            check(
                "cuGraphicsD3D11RegisterResource(UV R8G8)",
                ffi::cuGraphicsD3D11RegisterResource(&mut uv_res, uv_ptr as *mut _, 0),
            )
            .expect("UV R8G8 register must succeed (Plan 2d hard requirement)");

            // Map them, fetch CUarrays, confirm non-null.
            let mut resources = [y_res, uv_res];
            let map_r =
                ffi::cuGraphicsMapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
            assert!(
                map_r == ffi::cudaError_enum::CUDA_SUCCESS,
                "cuGraphicsMapResources failed: {}",
                map_r as u32
            );

            let mut y_array: ffi::CUarray = std::ptr::null_mut();
            let ry = ffi::cuGraphicsSubResourceGetMappedArray(&mut y_array, y_res, 0, 0);
            let mut uv_array: ffi::CUarray = std::ptr::null_mut();
            let ruv = ffi::cuGraphicsSubResourceGetMappedArray(&mut uv_array, uv_res, 0, 0);

            let _ = ffi::cuGraphicsUnmapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
            let _ = ffi::cuGraphicsUnregisterResource(y_res);
            let _ = ffi::cuGraphicsUnregisterResource(uv_res);

            assert!(
                ry == ffi::cudaError_enum::CUDA_SUCCESS,
                "Y array fetch CUresult={}",
                ry as u32
            );
            assert!(!y_array.is_null(), "Y CUarray was null");
            assert!(
                ruv == ffi::cudaError_enum::CUDA_SUCCESS,
                "UV array fetch CUresult={}",
                ruv as u32
            );
            assert!(!uv_array.is_null(), "UV CUarray was null");
        }
    }

    /// End-to-end: encode a synthetic frame with NVENC, feed the resulting
    /// HEVC NAL stream into the NVDEC consumer, and confirm that a decoded
    /// dual-plane GPU frame of the expected dimensions and formats comes out.
    /// Proves the parser + decoder + CUDA-D3D11 zero-copy path works
    /// end-to-end against a real NVIDIA driver.
    #[cfg(all(prdt_nvdec_bindings, prdt_nvenc_bindings))]
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
        let mut enc = NvencEncoder::new(
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

        let dual = consumer
            .take_latest_dual_plane()
            .expect("NVDEC should have produced at least one dual-plane frame");
        assert_eq!(dual.width, w);
        assert_eq!(dual.height, h);
        assert_eq!(dual.y_tex_raw().format(), crate::d3d11::TextureFormat::R8);
        assert_eq!(
            dual.uv_tex_raw().format(),
            crate::d3d11::TextureFormat::R8G8
        );
    }
}
