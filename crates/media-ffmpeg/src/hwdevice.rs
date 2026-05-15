use std::ffi::CString;
use std::path::Path;
use std::ptr;
use std::ptr::NonNull;

use rusty_ffmpeg::ffi::{
    av_buffer_unref, av_hwdevice_ctx_create, avcodec_find_encoder_by_name, AVBufferRef,
    AVHWDeviceType_AV_HWDEVICE_TYPE_VAAPI,
};

use crate::error::FfmpegError;

#[derive(Debug)]
pub(crate) struct VaapiHwDevice {
    raw: NonNull<AVBufferRef>,
}

impl VaapiHwDevice {
    pub(crate) fn open(render_node: Option<&Path>) -> Result<Self, FfmpegError> {
        let node_cstr = render_node.map(|p| {
            CString::new(p.to_string_lossy().as_bytes())
                .expect("render node path has no interior nul")
        });
        let node_ptr = node_cstr
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null());

        let mut raw_ptr: *mut AVBufferRef = ptr::null_mut();
        // SAFETY: raw_ptr is a local out-param; node_ptr lifetime covers the call;
        // opts is null (no extra options); flags is 0 (reserved, must be 0).
        let ret = unsafe {
            av_hwdevice_ctx_create(
                &mut raw_ptr,
                AVHWDeviceType_AV_HWDEVICE_TYPE_VAAPI,
                node_ptr,
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 {
            return Err(FfmpegError::HwDevice(format!(
                "av_hwdevice_ctx_create returned {ret}"
            )));
        }

        // SAFETY: av_hwdevice_ctx_create succeeded so raw_ptr is non-null.
        let raw = unsafe { NonNull::new_unchecked(raw_ptr) };

        // Probe encoder availability early so we fail fast before allocating frames.
        // SAFETY: string literal is valid nul-terminated C string.
        let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_vaapi".as_ptr()) };
        if codec.is_null() {
            let mut p = raw.as_ptr();
            // SAFETY: raw is the unique owner; no other references exist yet.
            unsafe { av_buffer_unref(&mut p) };
            return Err(FfmpegError::EncoderNotFound("hevc_vaapi"));
        }

        Ok(Self { raw })
    }

    pub(crate) fn raw(&self) -> *mut AVBufferRef {
        self.raw.as_ptr()
    }
}

impl Drop for VaapiHwDevice {
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
    fn open_fails_cleanly_without_vaapi() {
        // Dev container has no /dev/dri/renderD* — expect HwDevice error, not panic.
        let result = VaapiHwDevice::open(None);
        assert!(
            matches!(
                result,
                Err(FfmpegError::HwDevice(_)) | Err(FfmpegError::EncoderNotFound(_))
            ),
            "expected HwDevice or EncoderNotFound, got: {result:?}"
        );
    }
}
