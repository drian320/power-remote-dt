use std::ptr;
use std::ptr::NonNull;

use rusty_ffmpeg::ffi::{
    av_buffer_unref, av_hwdevice_ctx_create, avcodec_find_encoder_by_name, AVBufferRef,
    AV_HWDEVICE_TYPE_CUDA,
};

use crate::error::FfmpegError;

/// CUDA HW device for `hevc_nvenc`. CUDA device index defaults to 0; override
/// via `CUDA_VISIBLE_DEVICES` env (multi-GPU selection is intentionally not a
/// CLI flag in P1.5 — tracked as ADR follow-up F3).
#[derive(Debug)]
pub(crate) struct CudaHwDevice {
    raw: NonNull<AVBufferRef>,
}

impl CudaHwDevice {
    pub(crate) fn open() -> Result<Self, FfmpegError> {
        let mut raw_ptr: *mut AVBufferRef = ptr::null_mut();
        // SAFETY: raw_ptr is a local out-param; device path is null (CUDA default
        // device 0, or whatever CUDA_VISIBLE_DEVICES selects); opts is null; flags 0.
        let ret = unsafe {
            av_hwdevice_ctx_create(
                &mut raw_ptr,
                AV_HWDEVICE_TYPE_CUDA,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            return Err(FfmpegError::HwDevice(format!(
                "av_hwdevice_ctx_create(CUDA) returned {ret}"
            )));
        }

        // SAFETY: av_hwdevice_ctx_create succeeded so raw_ptr is non-null.
        let raw = unsafe { NonNull::new_unchecked(raw_ptr) };

        // Fail-fast: probe hevc_nvenc availability before allocating frames.
        // SAFETY: string literal is a valid nul-terminated C string.
        let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_nvenc".as_ptr()) };
        if codec.is_null() {
            let mut p = raw.as_ptr();
            // SAFETY: raw is the unique owner; no other references exist yet.
            unsafe { av_buffer_unref(&mut p) };
            return Err(FfmpegError::EncoderNotFound("hevc_nvenc"));
        }

        Ok(Self { raw })
    }

    pub(crate) fn raw(&self) -> *mut AVBufferRef {
        self.raw.as_ptr()
    }
}

impl Drop for CudaHwDevice {
    fn drop(&mut self) {
        let mut p = self.raw.as_ptr();
        // SAFETY: self.raw is the unique owner (non-Send struct, single-thread use).
        unsafe { av_buffer_unref(&mut p) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_fails_cleanly_without_cuda() {
        // Dev container has no /dev/nvidia* — expect HwDevice or EncoderNotFound,
        // never panic. Mirrors VaapiHwDevice::open_fails_cleanly_without_vaapi.
        let result = CudaHwDevice::open();
        assert!(
            matches!(
                result,
                Err(FfmpegError::HwDevice(_)) | Err(FfmpegError::EncoderNotFound(_))
            ),
            "expected HwDevice or EncoderNotFound, got: {result:?}"
        );
    }
}
