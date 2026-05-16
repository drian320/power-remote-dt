use std::ptr::NonNull;

use rusty_ffmpeg::ffi::{
    av_buffer_unref, av_hwframe_ctx_alloc, av_hwframe_ctx_init, AVBufferRef, AVHWFramesContext,
    AV_PIX_FMT_CUDA, AV_PIX_FMT_NV12,
};

use crate::cuda_hwdevice::CudaHwDevice;
use crate::error::FfmpegError;

pub(crate) struct CudaHwFrames {
    raw: NonNull<AVBufferRef>,
}

impl CudaHwFrames {
    pub(crate) fn new(device: &CudaHwDevice, width: u32, height: u32) -> Result<Self, FfmpegError> {
        // SAFETY: device.raw() is a valid AVHWDeviceContext buffer ref owned by device.
        let mut raw_ptr = unsafe { av_hwframe_ctx_alloc(device.raw()) };
        if raw_ptr.is_null() {
            return Err(FfmpegError::HwFrames(
                "av_hwframe_ctx_alloc returned null".into(),
            ));
        }

        // SAFETY: raw_ptr is non-null; data points to the embedded AVHWFramesContext.
        unsafe {
            let ctx = (*raw_ptr).data as *mut AVHWFramesContext;
            (*ctx).format = AV_PIX_FMT_CUDA;
            (*ctx).sw_format = AV_PIX_FMT_NV12;
            (*ctx).width = width as i32;
            (*ctx).height = height as i32;
            (*ctx).initial_pool_size = 4;
        }

        // SAFETY: raw_ptr is a valid uninitialised AVHWFramesContext buffer ref.
        let ret = unsafe { av_hwframe_ctx_init(raw_ptr) };
        if ret < 0 {
            // SAFETY: raw_ptr is still the unique owner; init failed, clean up.
            unsafe { av_buffer_unref(&mut raw_ptr) };
            return Err(FfmpegError::HwFrames(format!(
                "av_hwframe_ctx_init returned {ret}"
            )));
        }

        // Post-init: assert driver didn't coerce sw_format away from NV12.
        // SAFETY: init succeeded; ctx pointer is still valid.
        let actual_sw_format = unsafe {
            let ctx = (*raw_ptr).data as *mut AVHWFramesContext;
            (*ctx).sw_format
        };
        if actual_sw_format != AV_PIX_FMT_NV12 {
            // SAFETY: raw_ptr is the unique owner.
            unsafe { av_buffer_unref(&mut raw_ptr) };
            return Err(FfmpegError::HwFrames(
                "driver coerced sw_format away from NV12".into(),
            ));
        }

        // SAFETY: av_hwframe_ctx_init succeeded so raw_ptr is non-null.
        let raw = unsafe { NonNull::new_unchecked(raw_ptr) };
        Ok(Self { raw })
    }

    pub(crate) fn raw(&self) -> *mut AVBufferRef {
        self.raw.as_ptr()
    }
}

impl Drop for CudaHwFrames {
    fn drop(&mut self) {
        let mut p = self.raw.as_ptr();
        // SAFETY: self.raw is the unique owner of this AVHWFramesContext buffer ref.
        unsafe { av_buffer_unref(&mut p) };
    }
}
