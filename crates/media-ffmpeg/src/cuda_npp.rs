//! Thin hand-rolled CUDA + NPP wrapper for the P2.5 GPU-side BGRA→NV12
//! conversion path.
//!
//! Replaces the CPU `bgra_to_i420` + `i420_to_nv12_into` + `hw_upload`
//! chain on the NVENC encode hot path with a single device-resident HtoD
//! upload + on-GPU NPP color conversion + planar→NV12 pack.
//!
//! CURRENT SYMBOL COUNT: 8 — if > 12, re-evaluate dep choice per ADR
//! follow-up F10 (cust / cudarc crate evaluation).
//!
//! Plan §3 specified 6 symbols using the CUDA driver API (`cuMemAlloc`,
//! `cuMemFree`, `cuMemcpy2D`, `cuMemcpy2DAsync`, `cuStreamSynchronize`,
//! `nppiBGRToYUV420_8u_AC4P3R`). Implementation deviation: we use the
//! CUDA *runtime* API (libcudart) instead of the *driver* API (libcuda).
//! `cudaMalloc`/`cudaFree`/`cudaMemcpy2D`/`cudaMemcpy2DAsync`/
//! `cudaStreamSynchronize`/`cudaDriverGetVersion` replace the driver-API
//! `cu*` set; `nppiBGRToYUV420_8u_AC4P3R` + `nppiYCbCr420_8u_P3P2R` cover
//! the BGRA→I420 + I420→NV12 NPP steps (the latter is what plan §3 Risk
//! row 1 had slated for a dlsym probe — including it directly stays well
//! under the F10 12-symbol tripwire).
//!
//! Why runtime API: libcudart ships in the CUDA toolkit (already pulled
//! by Step 0's `cuda-cudart-dev-12-4`) and resolves at link time on any
//! host where the toolkit is installed, including the dev container.
//! `libcuda.so.1` only ships with the NVIDIA *driver* — the dev container
//! has the toolkit but no driver, so direct driver-API linking refuses to
//! link there. The runtime API is also higher-level and idiomatic for
//! code that doesn't manage its own CUDA context (NPP shares the current
//! thread context which libavcodec's `av_hwdevice_ctx_create` installs).
//!
//! Functional impact: none — the runtime API calls dispatch into the
//! driver at runtime, and the same `libcuda.so.1` from the NVIDIA driver
//! is exercised on the smoke runner. The committed stub
//! (`tests/fixtures/libnppicc-stub.c`) was built for the driver-API
//! symbol set and will need an update to match the runtime-API names if
//! it is ever needed as a fallback (tracked in commit message).
//!
//! Drop-safety: every device resource is wrapped in a `NewType` with its
//! own `Drop`, and `CudaNppContext::drop` runs `cudaStreamSynchronize`
//! before any field drops (Rust drops fields in declaration order, so
//! resources free reverse of creation). Per plan §3 W2.

#![cfg(all(feature = "ffmpeg-encode-hevc-nvenc-npp-any", target_os = "linux"))]

use std::os::raw::{c_int, c_void};
use std::ptr;

use rusty_ffmpeg::ffi::AVFrame;

use crate::error::FfmpegError;

// ----- Opaque CUDA / NPP types --------------------------------------------

/// CUDA stream handle (opaque pointer in the runtime API too).
pub(crate) type CudaStreamHandle = *mut c_void;
/// `cudaError_t` enum. `cudaSuccess = 0`.
pub(crate) type CudaError = c_int;
/// NPP status enum (`NppStatus`). Success = 0; positive = warning; negative = error.
pub(crate) type NppStatus = c_int;

const CUDA_SUCCESS: CudaError = 0;
const NPP_NO_ERROR: NppStatus = 0;

/// `cudaMemcpyKind` discriminants used by `cudaMemcpy2D` / `cudaMemcpy2DAsync`.
const CUDA_MEMCPY_HOST_TO_DEVICE: c_int = 1;
#[allow(dead_code)]
const CUDA_MEMCPY_DEVICE_TO_DEVICE: c_int = 3;

/// Minimum CUDA driver version we accept (12000 = CUDA 12.0). Driver ≥ 535
/// supports this via forward-compat per P1.5 A8 and plan Risk row 11.
const MIN_DRIVER_VERSION: c_int = 12000;

/// NPP region-of-interest size used by `nppiBGRToYUV420_8u_AC4P3R` +
/// `nppiYCbCr420_8u_P3P2R`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct NppiSize {
    pub width: c_int,
    pub height: c_int,
}

// ----- Symbol declarations -------------------------------------------------
//
// All `cuda*` symbols are in libcudart; `nppi*` are in libnppicc. Both
// libraries are pulled into the link via `build.rs` under the NPP feature.

extern "C" {
    fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> CudaError;
    fn cudaFree(dev_ptr: *mut c_void) -> CudaError;
    fn cudaMemcpy2D(
        dst: *mut c_void,
        dpitch: usize,
        src: *const c_void,
        spitch: usize,
        width: usize,
        height: usize,
        kind: c_int,
    ) -> CudaError;
    // ci-allow: cuda-direct. Per P2.5 O1 A10 carve-out — this is the
    // deliberate replacement for av_hwframe_transfer_data on the NPP path.
    #[allow(dead_code)]
    fn cudaMemcpy2DAsync(
        dst: *mut c_void,
        dpitch: usize,
        src: *const c_void,
        spitch: usize,
        width: usize,
        height: usize,
        kind: c_int,
        stream: CudaStreamHandle,
    ) -> CudaError;
    fn cudaStreamSynchronize(stream: CudaStreamHandle) -> CudaError;
    fn cudaDriverGetVersion(driver_version: *mut c_int) -> CudaError;
    fn nppiBGRToYUV420_8u_AC4P3R(
        p_src: *const u8,
        n_src_step: c_int,
        p_dst: *const *mut u8,
        r_dst_step: *const c_int,
        o_size_roi: NppiSize,
    ) -> NppStatus;
    fn nppiYCbCr420_8u_P3P2R(
        p_src: *const *const u8,
        r_src_step: *const c_int,
        p_dst_y: *mut u8,
        n_dst_y_step: c_int,
        p_dst_cbcr: *mut u8,
        n_dst_cbcr_step: c_int,
        o_size_roi: NppiSize,
    ) -> NppStatus;
}

// ----- RAII wrappers -------------------------------------------------------

/// Owned device pointer; frees via `cudaFree` on drop.
pub(crate) struct CudaDevicePtr(*mut c_void);

impl CudaDevicePtr {
    fn alloc(bytes: usize) -> Result<Self, FfmpegError> {
        let mut p: *mut c_void = ptr::null_mut();
        // SAFETY: out-param p is a local *mut c_void; cudaMalloc writes into it.
        let ret = unsafe { cudaMalloc(&mut p, bytes) };
        if ret != CUDA_SUCCESS {
            return Err(FfmpegError::HwDevice(format!(
                "cudaMalloc({bytes}) returned {ret}"
            )));
        }
        Ok(Self(p))
    }

    pub(crate) fn raw(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for CudaDevicePtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 is the sole owner of the device allocation.
            let _ = unsafe { cudaFree(self.0) };
        }
    }
}

/// Optional per-encoder CUDA stream. P2.5 ships with the default stream
/// (synchronous semantics) — per-encoder streams are follow-up F6.
pub(crate) struct CudaStream(CudaStreamHandle);

impl CudaStream {
    fn default_stream() -> Self {
        Self(ptr::null_mut())
    }

    pub(crate) fn raw(&self) -> CudaStreamHandle {
        self.0
    }
}

// Default stream has no resource to free — Drop is a no-op until F6
// upgrades to a real per-encoder stream.

// ----- Public surface ------------------------------------------------------

/// Per-encoder CUDA + NPP scratch buffer state.
///
/// Drop order (reverse-creation, RAII-managed):
/// 1. `cudaStreamSynchronize` in `Drop::drop` (drain in-flight async ops).
/// 2. `dev_bgra` frees via `CudaDevicePtr::drop`.
/// 3. `dev_y` frees via `CudaDevicePtr::drop`.
/// 4. `dev_u` frees via `CudaDevicePtr::drop`.
/// 5. `dev_v` frees via `CudaDevicePtr::drop`.
/// 6. `stream` — default-stream no-op (placeholder for F6 per-encoder stream).
pub(crate) struct CudaNppContext {
    // Field order MATTERS — Rust drops in declaration order so the
    // device buffers free before the stream's placeholder.
    width: u32,
    height: u32,
    bgra_stride: u32,
    dev_bgra: CudaDevicePtr,
    dev_y: CudaDevicePtr,
    dev_u: CudaDevicePtr,
    dev_v: CudaDevicePtr,
    stream: CudaStream,
}

impl CudaNppContext {
    /// Allocate per-encoder device buffers + verify CUDA driver version.
    ///
    /// `bgra_stride` is the row pitch (bytes per row) of the upcoming BGRA
    /// uploads — set to `width * 4` for tightly-packed BGRA from
    /// `X11ShmCapturer` (matches the existing capture layout). Callers that
    /// pass a different stride must use the per-frame entry point's stride
    /// override (not implemented in P2.5 — captured stride matches
    /// `width * 4` for all currently-supported capture sources).
    pub(crate) fn new(width: u32, height: u32) -> Result<Self, FfmpegError> {
        // Probe driver version first — fail loud on too-old drivers (W1).
        let mut driver: c_int = 0;
        // SAFETY: out-param is a local c_int.
        let ret = unsafe { cudaDriverGetVersion(&mut driver) };
        if ret != CUDA_SUCCESS {
            return Err(FfmpegError::HwDevice(format!(
                "cudaDriverGetVersion returned {ret} (CUDA runtime not loadable)"
            )));
        }
        if driver < MIN_DRIVER_VERSION {
            return Err(FfmpegError::HwDevice(format!(
                "CUDA driver too old: {driver} < {MIN_DRIVER_VERSION} (need >= 12.0); \
                 upgrade NVIDIA driver to >= 535"
            )));
        }

        let w = width as usize;
        let h = height as usize;
        let bgra_bytes = w * 4 * h;
        let y_bytes = w * h;
        let u_bytes = (w / 2) * (h / 2);
        let v_bytes = u_bytes;

        let dev_bgra = CudaDevicePtr::alloc(bgra_bytes)?;
        let dev_y = CudaDevicePtr::alloc(y_bytes)?;
        let dev_u = CudaDevicePtr::alloc(u_bytes)?;
        let dev_v = CudaDevicePtr::alloc(v_bytes)?;

        Ok(Self {
            width,
            height,
            bgra_stride: width * 4,
            dev_bgra,
            dev_y,
            dev_u,
            dev_v,
            stream: CudaStream::default_stream(),
        })
    }

    /// Convert a host-side BGRA frame into the NV12 planes of `dst_hw_frame`
    /// (a CUDA-surface `AVFrame` obtained from the encoder's hw_frames pool
    /// via `av_hwframe_get_buffer`).
    ///
    /// Pipeline (all device-resident after the first cudaMemcpy2D):
    ///   1. `cudaMemcpy2D` HtoD: host BGRA → `dev_bgra` (single PCIe Tx).
    ///   2. NPP `nppiBGRToYUV420_8u_AC4P3R`: `dev_bgra` → planar I420.
    ///   3. NPP `nppiYCbCr420_8u_P3P2R`: planar I420 → Y + interleaved-UV,
    ///      written directly into the AVFrame's `data[0]` / `data[1]`
    ///      device pointers.
    ///   4. `cudaStreamSynchronize`: per-frame fence so NVENC sees a fully
    ///      written surface before `avcodec_send_frame`.
    pub(crate) fn convert_bgra_to_nv12_into_av_frame(
        &mut self,
        bgra: &[u8],
        dst_hw_frame: *mut AVFrame,
    ) -> Result<(), FfmpegError> {
        let w = self.width as usize;
        let h = self.height as usize;
        let bgra_row = w * 4;
        let expected = bgra_row * h;
        if bgra.len() < expected {
            return Err(FfmpegError::HwFrames(format!(
                "bgra slice too small: {} < expected {expected}",
                bgra.len()
            )));
        }

        // 1. HtoD upload.
        // SAFETY: src/dst pointers are valid for the lifetimes of `bgra` and
        // `self.dev_bgra`; strides match the documented per-frame layout.
        let ret = unsafe {
            cudaMemcpy2D(
                self.dev_bgra.raw(),
                bgra_row,
                bgra.as_ptr().cast(),
                self.bgra_stride as usize,
                bgra_row,
                h,
                CUDA_MEMCPY_HOST_TO_DEVICE,
            )
        };
        if ret != CUDA_SUCCESS {
            return Err(FfmpegError::Transfer(ret));
        }

        // 2. NPP BGRA → planar YCbCr 4:2:0.
        let dst_planes: [*mut u8; 3] = [
            self.dev_y.raw() as *mut u8,
            self.dev_u.raw() as *mut u8,
            self.dev_v.raw() as *mut u8,
        ];
        let dst_steps: [c_int; 3] = [
            self.width as c_int,
            (self.width / 2) as c_int,
            (self.width / 2) as c_int,
        ];
        let roi = NppiSize {
            width: self.width as c_int,
            height: self.height as c_int,
        };
        // SAFETY: all pointers point to device allocations owned by self;
        // strides and ROI match the allocation sizes.
        let npp_ret = unsafe {
            nppiBGRToYUV420_8u_AC4P3R(
                self.dev_bgra.raw() as *const u8,
                bgra_row as c_int,
                dst_planes.as_ptr(),
                dst_steps.as_ptr(),
                roi,
            )
        };
        if npp_ret != NPP_NO_ERROR {
            return Err(FfmpegError::Transfer(npp_ret));
        }

        // 3. NPP planar I420 → NV12 (Y + interleaved UV) into the AVFrame's
        //    CUDA planes.
        // SAFETY: dst_hw_frame is a valid AVFrame from av_hwframe_get_buffer
        // with data[0]/data[1] populated with device pointers (libavutil
        // stores CUdeviceptr-cast-as-*mut u8 in AVFrame.data for the CUDA
        // pixel format).
        let (dst_y, dst_uv, dst_y_stride, dst_uv_stride) = unsafe {
            (
                (*dst_hw_frame).data[0],
                (*dst_hw_frame).data[1],
                (*dst_hw_frame).linesize[0],
                (*dst_hw_frame).linesize[1],
            )
        };
        if dst_y.is_null() || dst_uv.is_null() {
            return Err(FfmpegError::HwFrames("AVFrame CUDA planes are null".into()));
        }
        let src_planes: [*const u8; 3] = [
            self.dev_y.raw() as *const u8,
            self.dev_u.raw() as *const u8,
            self.dev_v.raw() as *const u8,
        ];
        let src_steps: [c_int; 3] = [
            self.width as c_int,
            (self.width / 2) as c_int,
            (self.width / 2) as c_int,
        ];
        // SAFETY: src/dst planes are valid device pointers; ROI + strides
        // match the planar sizes and the AVFrame's published linesizes.
        let pack_ret = unsafe {
            nppiYCbCr420_8u_P3P2R(
                src_planes.as_ptr(),
                src_steps.as_ptr(),
                dst_y,
                dst_y_stride,
                dst_uv,
                dst_uv_stride,
                roi,
            )
        };
        if pack_ret != NPP_NO_ERROR {
            return Err(FfmpegError::Transfer(pack_ret));
        }

        // 4. Per-frame fence — NVENC's avcodec_send_frame consumes the
        //    AVFrame synchronously w.r.t. the host but expects the CUDA
        //    surface to be fully written. With the default stream this is
        //    effectively a no-op; once F6 upgrades to a per-encoder stream
        //    this becomes the explicit sync point.
        // SAFETY: stream handle is valid (null = default stream).
        let sync_ret = unsafe { cudaStreamSynchronize(self.stream.raw()) };
        if sync_ret != CUDA_SUCCESS {
            return Err(FfmpegError::Transfer(sync_ret));
        }
        Ok(())
    }
}

impl Drop for CudaNppContext {
    fn drop(&mut self) {
        // Drain any in-flight async ops on this encoder's stream before
        // freeing the device buffers they may still reference. Best-effort —
        // ignore the error if the runtime is unloaded. Per plan §3 W2.
        // SAFETY: stream handle is valid for the lifetime of self.
        let _ = unsafe { cudaStreamSynchronize(self.stream.raw()) };
        // Fields drop in declaration order via RAII: dev_bgra → dev_y →
        // dev_u → dev_v → stream (no-op for default stream).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructor failure path on a CUDA-less host (dev container).
    /// `cudaDriverGetVersion` returns 0 (no driver) on hosts without
    /// `libcuda.so.1`; surfaces as `FfmpegError::HwDevice`. Mirrors
    /// `cuda_hwdevice::tests::open_fails_cleanly_without_cuda`.
    #[test]
    #[ignore = "exercises driver-version probe behavior; depends on the host \
                having either a real NVIDIA driver (>=12.0) or no driver at all"]
    fn new_fails_cleanly_without_cuda() {
        let result = CudaNppContext::new(320, 240);
        assert!(
            matches!(&result, Err(FfmpegError::HwDevice(_))),
            "expected HwDevice error, got Ok or other Err variant"
        );
    }
}
