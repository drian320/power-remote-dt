//! Plan 2d step 2b: `CuvidDecoder` — thin wrapper over `CUvideoparser` +
//! `CUvideodecoder` that accepts annex-B HEVC bytes and produces NV12
//! frames in CPU memory.
//!
//! The parser is callback-driven: cuvidParseVideoData pumps bytes and
//! synchronously invokes our sequence / decode / display callbacks on
//! the calling thread. We keep a `Box<DecoderState>` pinned behind the
//! parser's `pUserData`, and recover it in the three extern "C"
//! callbacks below. `std::panic::catch_unwind` wraps each callback so
//! a Rust panic can't unwind across the FFI boundary (which would be UB
//! under MSVC's `-C panic=abort` release profile).
//!
//! This step delivers CPU-side NV12 bytes as the decoded output. Step 2c
//! replaces the CPU copy with CUDA-D3D11 interop so the frame stays on
//! the GPU all the way to Nv12Renderer.

// Module-level cfg gate is applied at the `mod decoder` site in nvdec/mod.rs,
// so a redundant `#![cfg]` here would trip clippy::duplicated_attributes.
// Allow a few lints that don't really help inside unsafe FFI glue:
//   - field_reassign_with_default: CUDA_MEMCPY2D has 15+ fields and setting
//     them post-default() is strictly more readable than a 15-line struct
//     literal with most fields left at their zero value.
//   - unnecessary_mut_passed: cuMemcpy2D_v2 takes `*mut CUDA_MEMCPY2D` in
//     the bindgen signature; passing `&mut` matches intent even when
//     clippy thinks `&` would suffice.
#![allow(clippy::field_reassign_with_default, clippy::unnecessary_mut_passed)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use super::cuda::{check, CudaContext};
use super::ffi;
use crate::error::MediaError;

/// One decoded NV12 frame in CPU memory. Y plane is `width * height`
/// bytes; UV plane follows, interleaved, at half vertical resolution.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: i64,
    /// Packed Y (height rows) + UV (height/2 rows, interleaved UVUV…).
    pub nv12: Vec<u8>,
}

/// State shared between the Rust side and the C callback trio. Owned
/// by `CuvidDecoder` via `Box<DecoderState>`; the parser keeps a raw
/// pointer to it for the duration of its life.
struct DecoderState {
    ctx: Arc<CudaContext>,
    decoder: Option<ffi::CUvideodecoder>,
    /// Set by the sequence callback; read by the display callback to
    /// size the output NV12 buffer. u32 fits any real resolution.
    width: u32,
    height: u32,
    /// Max decode surfaces the decoder was created with. Returned from
    /// pfnSequenceCallback so cuvid knows the parser's ring depth.
    surfaces: u32,
    /// Latest decoded frame the consumer can take. Protected by a
    /// Mutex because the C callback runs on a different thread than
    /// the Rust method that drains it (but in practice we drive from
    /// the same caller; the mutex is belt-and-suspenders).
    latest: Mutex<Option<DecodedFrame>>,
    /// Sticky error from a callback: any MediaError produced inside
    /// a callback gets stashed so `submit()` can surface it.
    error: Mutex<Option<MediaError>>,
}

/// `CUvideoparser` + owning state. Drop destroys the parser first, then
/// the state's decoder (if any), then the CUcontext (via Arc ref count).
pub struct CuvidDecoder {
    parser: ffi::CUvideoparser,
    // The state Box must outlive the parser because the parser holds a
    // raw pointer to it. We keep the ownership here and hand the raw
    // pointer to cuvidCreateVideoParser.
    state: Box<DecoderState>,
    ctx: Arc<CudaContext>,
}

unsafe impl Send for CuvidDecoder {}

impl CuvidDecoder {
    /// Create a fresh HEVC decoder bound to `ctx`. `max_w` / `max_h` are
    /// capacity hints the bitstream's real sequence header may raise —
    /// we currently fail if it does.
    pub fn new_hevc(ctx: Arc<CudaContext>, max_w: u32, max_h: u32) -> Result<Self, MediaError> {
        let state = Box::new(DecoderState {
            ctx: Arc::clone(&ctx),
            decoder: None,
            width: max_w,
            height: max_h,
            surfaces: 0,
            latest: Mutex::new(None),
            error: Mutex::new(None),
        });

        let mut params: ffi::CUVIDPARSERPARAMS = unsafe { std::mem::zeroed() };
        params.CodecType = ffi::cudaVideoCodec_enum::cudaVideoCodec_HEVC;
        // 20 surfaces is the cuvid sample default and fits typical
        // B-frame display queues with lots of headroom.
        params.ulMaxNumDecodeSurfaces = 20;
        params.ulClockRate = 1_000_000; // micros
        params.ulMaxDisplayDelay = 0; // lowest latency
        params.pUserData = &*state as *const _ as *mut c_void;
        params.pfnSequenceCallback = Some(handle_video_sequence);
        params.pfnDecodePicture = Some(handle_picture_decode);
        params.pfnDisplayPicture = Some(handle_picture_display);

        let mut parser: ffi::CUvideoparser = std::ptr::null_mut();
        {
            let _guard = ctx.push()?;
            unsafe {
                check(
                    "cuvidCreateVideoParser",
                    ffi::cuvidCreateVideoParser(&mut parser, &mut params),
                )?;
            }
        }

        Ok(Self { parser, state, ctx })
    }

    /// Feed a chunk of annex-B HEVC bytes to the parser. `pts_us` is an
    /// arbitrary monotonic timestamp the display callback will see;
    /// prdt's higher layers use `now_monotonic_us()` as the value.
    pub fn submit(&mut self, nalu_bytes: &[u8], pts_us: i64) -> Result<(), MediaError> {
        let mut pkt: ffi::CUVIDSOURCEDATAPACKET = unsafe { std::mem::zeroed() };
        // Bitfield: timestamp-valid flag lives in `flags`. The raw bit is
        // CUVID_PKT_TIMESTAMP (1 << 0); bindgen emits that as a constant.
        pkt.flags = ffi::CUvideopacketflags::CUVID_PKT_TIMESTAMP as u64 as ::std::os::raw::c_ulong;
        pkt.payload_size = nalu_bytes.len() as ::std::os::raw::c_ulong;
        pkt.payload = nalu_bytes.as_ptr();
        pkt.timestamp = pts_us;

        let _guard = self.ctx.push()?;
        unsafe {
            check(
                "cuvidParseVideoData",
                ffi::cuvidParseVideoData(self.parser, &mut pkt),
            )?;
        }
        // Surface any sticky error from the callbacks that ran during
        // this parse call. `.take()` ensures a second submit starts
        // clean if the caller decides to keep going.
        if let Some(e) = self.state.error.lock().unwrap().take() {
            return Err(e);
        }
        Ok(())
    }

    /// Pop the latest decoded frame, if one arrived since the last call.
    pub fn take_latest_frame(&self) -> Option<DecodedFrame> {
        self.state.latest.lock().unwrap().take()
    }
}

impl Drop for CuvidDecoder {
    fn drop(&mut self) {
        if !self.parser.is_null() {
            let _ = self.ctx.push();
            unsafe {
                let r = ffi::cuvidDestroyVideoParser(self.parser);
                if r != ffi::cudaError_enum::CUDA_SUCCESS {
                    tracing::warn!(code = r as u32, "cuvidDestroyVideoParser failed");
                }
            }
        }
        if let Some(dec) = self.state.decoder.take() {
            let _ = self.ctx.push();
            unsafe {
                let r = ffi::cuvidDestroyDecoder(dec);
                if r != ffi::cudaError_enum::CUDA_SUCCESS {
                    tracing::warn!(code = r as u32, "cuvidDestroyDecoder failed");
                }
            }
        }
    }
}

// --- Callbacks ------------------------------------------------------------
//
// Signatures come from cuviddec.h typedefs. cuvid calls them synchronously
// on the thread that called cuvidParseVideoData, so no cross-thread sync
// on `DecoderState` fields is strictly required — but we hold per-field
// mutexes so the Rust side can safely observe `latest`/`error` without
// worrying about races with a future async parser.

unsafe extern "C" fn handle_video_sequence(
    user_data: *mut c_void,
    format: *mut ffi::CUVIDEOFORMAT,
) -> ::std::os::raw::c_int {
    let state = &mut *(user_data as *mut DecoderState);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let fmt = &*format;
        if fmt.codec != ffi::cudaVideoCodec_enum::cudaVideoCodec_HEVC {
            record_error(state, MediaError::Other("NVDEC: non-HEVC codec".into()));
            return 0;
        }
        if fmt.bit_depth_luma_minus8 != 0 {
            record_error(
                state,
                MediaError::Other(format!(
                    "NVDEC: unsupported bit depth {}",
                    fmt.bit_depth_luma_minus8 + 8
                )),
            );
            return 0;
        }

        let surfaces = fmt.min_num_decode_surfaces.max(4) as u32;
        state.surfaces = surfaces;
        state.width = fmt.coded_width;
        state.height = fmt.coded_height;

        if state.decoder.is_some() {
            // Re-configuration isn't implemented — return the existing
            // surface count to acknowledge the new sequence.
            return surfaces as i32;
        }

        let mut create: ffi::CUVIDDECODECREATEINFO = std::mem::zeroed();
        create.CodecType = ffi::cudaVideoCodec_enum::cudaVideoCodec_HEVC;
        create.ulWidth = fmt.coded_width as ::std::os::raw::c_ulong;
        create.ulHeight = fmt.coded_height as ::std::os::raw::c_ulong;
        create.ulNumDecodeSurfaces = surfaces as ::std::os::raw::c_ulong;
        create.ChromaFormat = fmt.chroma_format;
        create.OutputFormat = ffi::cudaVideoSurfaceFormat_enum::cudaVideoSurfaceFormat_NV12;
        create.bitDepthMinus8 = fmt.bit_depth_luma_minus8 as ::std::os::raw::c_ulong;
        create.DeinterlaceMode = ffi::cudaVideoDeinterlaceMode_enum::cudaVideoDeinterlaceMode_Weave;
        create.ulTargetWidth = fmt.coded_width as ::std::os::raw::c_ulong;
        create.ulTargetHeight = fmt.coded_height as ::std::os::raw::c_ulong;
        create.ulNumOutputSurfaces = 2;
        create.ulCreationFlags =
            ffi::cudaVideoCreateFlags_enum::cudaVideoCreate_PreferCUVID as ::std::os::raw::c_ulong;

        // cuvidCreateDecoder must run with the CUcontext current.
        let _g = match state.ctx.push() {
            Ok(g) => g,
            Err(e) => {
                record_error(state, e);
                return 0;
            }
        };
        let mut dec: ffi::CUvideodecoder = std::ptr::null_mut();
        let r = ffi::cuvidCreateDecoder(&mut dec, &mut create);
        if r != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuvidCreateDecoder: CUresult={}", r as u32)),
            );
            return 0;
        }
        state.decoder = Some(dec);
        tracing::info!(
            width = fmt.coded_width,
            height = fmt.coded_height,
            surfaces,
            "NVDEC: decoder created from sequence header",
        );
        surfaces as i32
    }));
    match result {
        Ok(v) => v,
        Err(_) => {
            record_error(
                state,
                MediaError::Other("NVDEC: panic in sequence callback".into()),
            );
            0
        }
    }
}

unsafe extern "C" fn handle_picture_decode(
    user_data: *mut c_void,
    pic_params: *mut ffi::CUVIDPICPARAMS,
) -> ::std::os::raw::c_int {
    let state = &mut *(user_data as *mut DecoderState);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(dec) = state.decoder else {
            record_error(
                state,
                MediaError::Other("NVDEC: decode without decoder".into()),
            );
            return 0;
        };
        let _g = match state.ctx.push() {
            Ok(g) => g,
            Err(e) => {
                record_error(state, e);
                return 0;
            }
        };
        let r = ffi::cuvidDecodePicture(dec, pic_params);
        if r != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuvidDecodePicture: CUresult={}", r as u32)),
            );
            return 0;
        }
        1
    }));
    result.unwrap_or_else(|_| {
        record_error(
            state,
            MediaError::Other("NVDEC: panic in decode callback".into()),
        );
        0
    })
}

unsafe extern "C" fn handle_picture_display(
    user_data: *mut c_void,
    disp: *mut ffi::CUVIDPARSERDISPINFO,
) -> ::std::os::raw::c_int {
    let state = &mut *(user_data as *mut DecoderState);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(dec) = state.decoder else {
            record_error(
                state,
                MediaError::Other("NVDEC: display without decoder".into()),
            );
            return 0;
        };
        let _g = match state.ctx.push() {
            Ok(g) => g,
            Err(e) => {
                record_error(state, e);
                return 0;
            }
        };

        let mut proc_params: ffi::CUVIDPROCPARAMS = std::mem::zeroed();
        proc_params.progressive_frame = (*disp).progressive_frame;
        proc_params.second_field = (*disp).repeat_first_field + 1;
        proc_params.top_field_first = (*disp).top_field_first;
        proc_params.unpaired_field = ((*disp).repeat_first_field < 0) as i32;

        let mut dev_ptr: ffi::CUdeviceptr = 0;
        let mut pitch: ::std::os::raw::c_uint = 0;
        let r = ffi::cuvidMapVideoFrame64(
            dec,
            (*disp).picture_index,
            &mut dev_ptr,
            &mut pitch,
            &mut proc_params,
        );
        if r != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuvidMapVideoFrame64: CUresult={}", r as u32)),
            );
            return 0;
        }

        // Copy Y + UV planes into one CPU buffer. cuMemcpy2D handles
        // the source pitch (pitched linear memory on the device side)
        // and writes tightly-packed rows on the host side.
        let w = state.width as usize;
        let h = state.height as usize;
        let mut nv12 = vec![0u8; w * h * 3 / 2];

        let mut copy_ok = true;
        // Y plane: height rows of width bytes.
        let mut params_y: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
        params_y.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
        params_y.srcDevice = dev_ptr;
        params_y.srcPitch = pitch as usize;
        params_y.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
        params_y.dstHost = nv12.as_mut_ptr() as *mut c_void;
        params_y.dstPitch = w;
        params_y.WidthInBytes = w;
        params_y.Height = h;
        let ry = ffi::cuMemcpy2D_v2(&mut params_y);
        if ry != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuMemcpy2D (Y): CUresult={}", ry as u32)),
            );
            copy_ok = false;
        }

        if copy_ok {
            // UV plane: pitch*height bytes offset on device, h/2 rows of width bytes on host.
            let mut params_uv: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_uv.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_uv.srcDevice = dev_ptr + (pitch as u64) * (h as u64);
            params_uv.srcPitch = pitch as usize;
            params_uv.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
            params_uv.dstHost = nv12[w * h..].as_mut_ptr() as *mut c_void;
            params_uv.dstPitch = w;
            params_uv.WidthInBytes = w;
            params_uv.Height = h / 2;
            let ruv = ffi::cuMemcpy2D_v2(&mut params_uv);
            if ruv != ffi::cudaError_enum::CUDA_SUCCESS {
                record_error(
                    state,
                    MediaError::Other(format!("cuMemcpy2D (UV): CUresult={}", ruv as u32)),
                );
                copy_ok = false;
            }
        }

        let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);

        if copy_ok {
            *state.latest.lock().unwrap() = Some(DecodedFrame {
                width: state.width,
                height: state.height,
                timestamp_us: (*disp).timestamp,
                nv12,
            });
        }
        1
    }));
    result.unwrap_or_else(|_| {
        record_error(
            state,
            MediaError::Other("NVDEC: panic in display callback".into()),
        );
        0
    })
}

fn record_error(state: &DecoderState, err: MediaError) {
    let mut slot = state.error.lock().unwrap();
    if slot.is_none() {
        *slot = Some(err);
    }
}
