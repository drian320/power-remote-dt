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
//! Production output is a GPU-side dual-plane D3D11 texture pair
//! (R8 Y + R8G8 UV) populated via CUDA-D3D11 device-to-device
//! `cuMemcpy2D_v2`, eliminating the CPU NV12 bounce entirely.
//! The CPU NV12 path is kept behind `#[cfg(any(test, feature = "cpu-nv12"))]`
//! so unit tests can still exercise pixel-level comparison.

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
use crate::d3d11::D3d11Device;
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

/// One decoded frame as a pair of D3D11 textures sitting in GPU memory,
/// already populated by the display callback via CUDA-D3D11 device-to-device
/// `cuMemcpy2D_v2`. Cloning is cheap (refcount bump on the inner ID3D11Texture2D).
#[derive(Clone)]
pub struct DualPlaneFrame {
    /// R8 texture, width × height. Holds the Y (luma) plane.
    pub y_tex: crate::d3d11::D3d11Texture,
    /// R8G8 texture, (width/2) × (height/2). Each element is (Cb, Cr).
    pub uv_tex: crate::d3d11::D3d11Texture,
    /// Width of the original NV12 frame in pixels (Y plane size).
    pub width: u32,
    /// Height of the original NV12 frame in pixels (Y plane size).
    pub height: u32,
    pub timestamp_us: i64,
}

/// CUDA-side handle for a registered D3D11 texture pair. The `Drop` impl
/// unregisters both resources on the same CUDA context they were registered on.
struct DualCache {
    y_tex: crate::d3d11::D3d11Texture,
    uv_tex: crate::d3d11::D3d11Texture,
    y_cuda_res: ffi::CUgraphicsResource,
    uv_cuda_res: ffi::CUgraphicsResource,
    width: u32,
    height: u32,
}

unsafe impl Send for DualCache {}

impl DualCache {
    /// Build a fresh dual cache for `(width, height)`. `width` is the Y plane
    /// width in pixels; the UV texture is half that in each dimension.
    /// Caller must hold the CUDA context push BEFORE calling this.
    fn new(
        dev: &crate::d3d11::D3d11Device,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaError> {
        use crate::d3d11::{D3d11Texture, TextureFormat};

        let y_tex = D3d11Texture::new_for_cuda_interop(dev, width, height, TextureFormat::R8)?;
        let uv_tex =
            D3d11Texture::new_for_cuda_interop(dev, width / 2, height / 2, TextureFormat::R8G8)?;

        let mut y_cuda_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        let mut uv_cuda_res: ffi::CUgraphicsResource = std::ptr::null_mut();
        unsafe {
            use windows::core::Interface;
            let y_ptr: *mut std::ffi::c_void = y_tex.raw().as_raw() as *mut _;
            let uv_ptr: *mut std::ffi::c_void = uv_tex.raw().as_raw() as *mut _;
            super::cuda::check(
                "cuGraphicsD3D11RegisterResource(Y)",
                ffi::cuGraphicsD3D11RegisterResource(&mut y_cuda_res, y_ptr as *mut _, 0),
            )?;
            super::cuda::check(
                "cuGraphicsD3D11RegisterResource(UV)",
                ffi::cuGraphicsD3D11RegisterResource(&mut uv_cuda_res, uv_ptr as *mut _, 0),
            )?;
        }
        Ok(Self {
            y_tex,
            uv_tex,
            y_cuda_res,
            uv_cuda_res,
            width,
            height,
        })
    }
}

impl Drop for DualCache {
    fn drop(&mut self) {
        unsafe {
            // Best-effort unregister; failing here only leaks until the
            // CUDA context is destroyed.
            let _ = ffi::cuGraphicsUnregisterResource(self.y_cuda_res);
            let _ = ffi::cuGraphicsUnregisterResource(self.uv_cuda_res);
        }
    }
}

/// State shared between the Rust side and the C callback trio. Owned
/// by `CuvidDecoder` via `Box<DecoderState>`; the parser keeps a raw
/// pointer to it for the duration of its life.
struct DecoderState {
    ctx: Arc<CudaContext>,
    dev: D3d11Device,
    decoder: Option<ffi::CUvideodecoder>,
    /// Set by the sequence callback; read by the display callback to
    /// size the output NV12 buffer. u32 fits any real resolution.
    width: u32,
    height: u32,
    /// Max decode surfaces the decoder was created with. Returned from
    /// pfnSequenceCallback so cuvid knows the parser's ring depth.
    surfaces: u32,
    /// Latest decoded NV12 frame in CPU memory. Populated by the display
    /// callback only when `cpu-nv12` feature is on (or under cfg(test)).
    /// Production uses the dual-plane GPU path below.
    #[cfg(any(test, feature = "cpu-nv12"))]
    latest: Mutex<Option<DecodedFrame>>,
    /// CUDA-registered dual-plane D3D11 cache. Lazily built on the first
    /// display callback once the decode resolution is known. Populated in
    /// place by every subsequent display callback via device-to-device
    /// `cuMemcpy2D_v2`.
    dual_cache: Mutex<Option<DualCache>>,
    /// Latest decoded GPU dual-plane frame. Holds clones (refcount bumps)
    /// of the textures inside `dual_cache`, plus the timestamp.
    latest_dual: Mutex<Option<DualPlaneFrame>>,
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
    /// we currently fail if it does. `dev` is used in the display
    /// callback to create the dual-plane D3D11 interop textures.
    pub fn new_hevc(
        ctx: Arc<CudaContext>,
        dev: D3d11Device,
        max_w: u32,
        max_h: u32,
    ) -> Result<Self, MediaError> {
        let state = Box::new(DecoderState {
            ctx: Arc::clone(&ctx),
            dev,
            decoder: None,
            width: max_w,
            height: max_h,
            surfaces: 0,
            #[cfg(any(test, feature = "cpu-nv12"))]
            latest: Mutex::new(None),
            dual_cache: Mutex::new(None),
            latest_dual: Mutex::new(None),
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

    /// CPU-side NV12 frame (test / opt-in feature only). Production callers
    /// use `take_latest_dual_plane`.
    #[cfg(any(test, feature = "cpu-nv12"))]
    pub fn take_latest_frame(&self) -> Option<DecodedFrame> {
        self.state.latest.lock().unwrap().take()
    }

    /// GPU-side dual-plane frame: a (R8 Y, R8G8 UV) D3D11 texture pair already
    /// populated by the display callback via CUDA-D3D11 device-to-device copy.
    pub fn take_latest_dual_plane(&self) -> Option<DualPlaneFrame> {
        self.state.latest_dual.lock().unwrap().take()
    }
}

impl Drop for CuvidDecoder {
    fn drop(&mut self) {
        let _g = self.ctx.push();

        // Explicitly drop dual_cache while the CUDA context is pushed.
        // Without this, the implicit `Box<DecoderState>` drop runs after
        // `_g` falls out of scope, causing cuGraphicsUnregisterResource to
        // fail with CUDA_ERROR_INVALID_CONTEXT and silently leak the
        // graphics resources until the primary context is destroyed.
        *self.state.dual_cache.lock().unwrap() = None;

        if !self.parser.is_null() {
            unsafe {
                let r = ffi::cuvidDestroyVideoParser(self.parser);
                if r != ffi::cudaError_enum::CUDA_SUCCESS {
                    tracing::warn!(code = r as u32, "cuvidDestroyVideoParser failed");
                }
            }
        }
        if let Some(dec) = self.state.decoder.take() {
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

        // Production path: cuMemcpy2D_v2 directly from cuvid's pitched device
        // memory into CUDA-D3D11-mapped CUarrays for R8 (Y) and R8G8 (UV)
        // textures. Builds the dual cache on the first call. Test / opt-in
        // feature path additionally copies into a CPU NV12 buffer for
        // pixel-level cross-checking.
        let w = state.width as usize;
        let h = state.height as usize;

        // Lazily build the dual cache if it doesn't exist or the resolution
        // changed. The CUDA context is already pushed by the `_g` guard above.
        {
            let mut slot = state.dual_cache.lock().unwrap();
            let needs_rebuild = match slot.as_ref() {
                Some(c) => c.width != state.width || c.height != state.height,
                None => true,
            };
            if needs_rebuild {
                match DualCache::new(&state.dev, state.width, state.height) {
                    Ok(c) => *slot = Some(c),
                    Err(e) => {
                        record_error(state, e);
                        let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
                        return 0;
                    }
                }
            }
        }

        // GPU-side copy. Map both resources, fetch the two CUarrays, copy,
        // then unmap. Keeping the lock for the whole copy is fine — the
        // display callback is the only writer.
        let mut copy_ok = true;
        let cache_guard = state.dual_cache.lock().unwrap();
        let cache = cache_guard.as_ref().expect("dual_cache populated above");
        let mut resources = [cache.y_cuda_res, cache.uv_cuda_res];
        let map_r =
            ffi::cuGraphicsMapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());
        if map_r != ffi::cudaError_enum::CUDA_SUCCESS {
            record_error(
                state,
                MediaError::Other(format!("cuGraphicsMapResources: CUresult={}", map_r as u32)),
            );
            let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
            return 0;
        }

        let mut y_array: ffi::CUarray = std::ptr::null_mut();
        let mut uv_array: ffi::CUarray = std::ptr::null_mut();
        let ry =
            ffi::cuGraphicsSubResourceGetMappedArray(&mut y_array, cache.y_cuda_res, 0, 0);
        let ruv = ffi::cuGraphicsSubResourceGetMappedArray(
            &mut uv_array,
            cache.uv_cuda_res,
            0,
            0,
        );
        if ry != ffi::cudaError_enum::CUDA_SUCCESS
            || y_array.is_null()
            || ruv != ffi::cudaError_enum::CUDA_SUCCESS
            || uv_array.is_null()
        {
            copy_ok = false;
        }

        if copy_ok {
            // Y: device → R8 array. WidthInBytes = w (1 byte/pixel).
            let mut params_y: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_y.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_y.srcDevice = dev_ptr;
            params_y.srcPitch = pitch as usize;
            params_y.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_ARRAY;
            params_y.dstArray = y_array;
            params_y.WidthInBytes = w;
            params_y.Height = h;
            if ffi::cuMemcpy2D_v2(&mut params_y) != ffi::cudaError_enum::CUDA_SUCCESS {
                copy_ok = false;
            }
        }

        if copy_ok {
            // UV: device(+pitch*h) → R8G8 array. R8G8 is 2 bytes/pixel and
            // the UV plane is half-resolution per dim, so the row width in
            // bytes equals the Y plane row width: 2 * (w/2) = w.
            let mut params_uv: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_uv.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_uv.srcDevice = dev_ptr + (pitch as u64) * (h as u64);
            params_uv.srcPitch = pitch as usize;
            params_uv.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_ARRAY;
            params_uv.dstArray = uv_array;
            params_uv.WidthInBytes = w;
            params_uv.Height = h / 2;
            if ffi::cuMemcpy2D_v2(&mut params_uv) != ffi::cudaError_enum::CUDA_SUCCESS {
                copy_ok = false;
            }
        }

        let _ =
            ffi::cuGraphicsUnmapResources(2, resources.as_mut_ptr(), std::ptr::null_mut());

        if copy_ok {
            *state.latest_dual.lock().unwrap() = Some(DualPlaneFrame {
                y_tex: cache.y_tex.clone(),
                uv_tex: cache.uv_tex.clone(),
                width: state.width,
                height: state.height,
                timestamp_us: (*disp).timestamp,
            });
        }
        drop(cache_guard);

        // Test/feature path: also produce a CPU NV12 copy for pixel-level
        // cross-checking against the dual-plane texture pair.
        #[cfg(any(test, feature = "cpu-nv12"))]
        if copy_ok {
            let mut nv12 = vec![0u8; w * h * 3 / 2];
            let mut cpu_ok = true;
            let mut params_y_cpu: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
            params_y_cpu.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
            params_y_cpu.srcDevice = dev_ptr;
            params_y_cpu.srcPitch = pitch as usize;
            params_y_cpu.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
            params_y_cpu.dstHost = nv12.as_mut_ptr() as *mut c_void;
            params_y_cpu.dstPitch = w;
            params_y_cpu.WidthInBytes = w;
            params_y_cpu.Height = h;
            if ffi::cuMemcpy2D_v2(&mut params_y_cpu) != ffi::cudaError_enum::CUDA_SUCCESS {
                cpu_ok = false;
            }
            if cpu_ok {
                let mut params_uv_cpu: ffi::CUDA_MEMCPY2D = ffi::CUDA_MEMCPY2D::default();
                params_uv_cpu.srcMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_DEVICE;
                params_uv_cpu.srcDevice = dev_ptr + (pitch as u64) * (h as u64);
                params_uv_cpu.srcPitch = pitch as usize;
                params_uv_cpu.dstMemoryType = ffi::CUmemorytype_enum::CU_MEMORYTYPE_HOST;
                params_uv_cpu.dstHost = nv12[w * h..].as_mut_ptr() as *mut c_void;
                params_uv_cpu.dstPitch = w;
                params_uv_cpu.WidthInBytes = w;
                params_uv_cpu.Height = h / 2;
                if ffi::cuMemcpy2D_v2(&mut params_uv_cpu) != ffi::cudaError_enum::CUDA_SUCCESS {
                    cpu_ok = false;
                }
            }
            if cpu_ok {
                *state.latest.lock().unwrap() = Some(DecodedFrame {
                    width: state.width,
                    height: state.height,
                    timestamp_us: (*disp).timestamp,
                    nv12,
                });
            }
        }

        let _ = ffi::cuvidUnmapVideoFrame64(dec, dev_ptr);
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
