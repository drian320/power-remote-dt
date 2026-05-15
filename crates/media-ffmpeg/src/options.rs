use std::ffi::CString;
use std::ptr;
use std::ptr::NonNull;

use rusty_ffmpeg::ffi::{
    av_dict_set, AVCodecContext, AVDictionary, AVRational, AV_CODEC_FLAG_GLOBAL_HEADER,
    AV_PIX_FMT_VAAPI,
};

// HEVC Main profile (`profile_idc`) is defined by the HEVC standard as the
// value 1. FFmpeg exposes it as `FF_PROFILE_HEVC_MAIN` (≤ FFmpeg 5.x) and as
// `AV_PROFILE_HEVC_MAIN` (FFmpeg 6+), with the same numeric value in both.
// Use the literal so the source compiles cleanly against rusty_ffmpeg's
// ffmpeg5 / ffmpeg6 / ffmpeg7 bindings without per-feature import noise.
const AV_PROFILE_HEVC_MAIN: i32 = 1;

use crate::error::FfmpegError;

pub(crate) struct EncoderTunables {
    pub bitrate_bps: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub gop_size: u32,
}

/// Apply low-latency HEVC encoder settings to a freshly allocated AVCodecContext.
///
/// # Safety
/// `ctx` must point to a valid, freshly allocated (via `avcodec_alloc_context3`)
/// AVCodecContext that has not yet been opened with `avcodec_open2`.
pub(crate) unsafe fn apply_low_latency_hevc(ctx: *mut AVCodecContext, t: &EncoderTunables) {
    // SAFETY: caller guarantees ctx is a valid uninitialized AVCodecContext.
    unsafe {
        (*ctx).bit_rate = t.bitrate_bps as i64;
        (*ctx).rc_max_rate = t.bitrate_bps as i64;
        // One-second VBV buffer halved for low-delay: bitrate/fps * 2 / 2 = bitrate/fps
        (*ctx).rc_buffer_size = (t.bitrate_bps / t.fps.max(1)) as i32;
        (*ctx).gop_size = t.gop_size as i32;
        (*ctx).max_b_frames = 0;
        (*ctx).time_base = AVRational {
            num: 1,
            den: t.fps as i32,
        };
        (*ctx).framerate = AVRational {
            num: t.fps as i32,
            den: 1,
        };
        (*ctx).pix_fmt = AV_PIX_FMT_VAAPI;
        (*ctx).profile = AV_PROFILE_HEVC_MAIN as i32;
        // In-band parameter sets required: re-emitted on every IDR.
        // low_power=0 is the higher-quality path on Intel iGPU at our bitrates.
        (*ctx).flags &= !(AV_CODEC_FLAG_GLOBAL_HEADER as i32);
    }
}

fn dict_set(dict: &mut *mut AVDictionary, key: &str, value: &str) -> Result<(), FfmpegError> {
    let k = CString::new(key).expect("key has no interior nul");
    let v = CString::new(value).expect("value has no interior nul");
    // SAFETY: dict is a valid *mut *mut AVDictionary; k/v lifetimes cover the call.
    let ret = unsafe { av_dict_set(dict, k.as_ptr(), v.as_ptr(), 0) };
    if ret < 0 {
        Err(FfmpegError::HwDevice(format!(
            "av_dict_set({key}={value}) returned {ret}"
        )))
    } else {
        Ok(())
    }
}

/// Build the private-data dictionary for `hevc_vaapi` low-latency CBR encode.
/// Returns ownership of the dict pointer; caller passes it to `avcodec_open2`
/// which consumes and frees it.
pub(crate) fn build_priv_data_dict(gop_size: u32) -> Result<NonNull<AVDictionary>, FfmpegError> {
    let mut dict: *mut AVDictionary = ptr::null_mut();
    dict_set(&mut dict, "async_depth", "1")?;
    dict_set(&mut dict, "rc_mode", "CBR")?;
    dict_set(&mut dict, "forced_idr", "1")?;
    // low_power=0: higher-quality encode path on Intel iGPU at ≤30 Mbps.
    dict_set(&mut dict, "low_power", "0")?;
    dict_set(&mut dict, "idr_interval", &gop_size.to_string())?;

    NonNull::new(dict).ok_or_else(|| FfmpegError::HwDevice("av_dict_set produced null dict".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_ffmpeg::ffi::{av_dict_free, av_dict_get, AV_DICT_IGNORE_SUFFIX};

    fn get_dict_value<'a>(dict: *mut AVDictionary, key: &str) -> Option<String> {
        let k = CString::new(key).unwrap();
        // SAFETY: dict is a valid AVDictionary; key lifetime covers the call; entry is valid.
        let entry =
            unsafe { av_dict_get(dict, k.as_ptr(), ptr::null(), AV_DICT_IGNORE_SUFFIX as i32) };
        if entry.is_null() {
            return None;
        }
        // SAFETY: entry->value is a valid nul-terminated C string owned by the dict.
        let val = unsafe { std::ffi::CStr::from_ptr((*entry).value) };
        Some(val.to_string_lossy().into_owned())
    }

    #[test]
    fn low_latency_dict_contains_required_keys() {
        let dict = build_priv_data_dict(60).expect("dict built");
        let d = dict.as_ptr();

        assert_eq!(get_dict_value(d, "async_depth").as_deref(), Some("1"));
        assert_eq!(get_dict_value(d, "rc_mode").as_deref(), Some("CBR"));
        assert_eq!(get_dict_value(d, "forced_idr").as_deref(), Some("1"));
        assert_eq!(get_dict_value(d, "idr_interval").as_deref(), Some("60"));

        let mut d_free = dict.as_ptr();
        // SAFETY: dict is the sole owner; test cleanup.
        unsafe { av_dict_free(&mut d_free) };
    }

    #[test]
    fn codec_ctx_fields_set() {
        use rusty_ffmpeg::ffi::avcodec_alloc_context3;

        // SAFETY: null codec arg is valid — returns a default-zeroed context.
        let ctx = unsafe { avcodec_alloc_context3(ptr::null()) };
        assert!(!ctx.is_null(), "avcodec_alloc_context3 returned null");

        let t = EncoderTunables {
            bitrate_bps: 8_000_000,
            fps: 60,
            width: 1920,
            height: 1080,
            gop_size: 60,
        };
        // SAFETY: ctx is a freshly allocated, unopened AVCodecContext.
        unsafe { apply_low_latency_hevc(ctx, &t) };

        // SAFETY: ctx is still valid; we read fields then free.
        unsafe {
            assert_eq!((*ctx).max_b_frames, 0);
            assert_eq!((*ctx).gop_size, 60);
            assert_eq!((*ctx).bit_rate, 8_000_000);
            assert_eq!((*ctx).pix_fmt, AV_PIX_FMT_VAAPI);
            assert_eq!((*ctx).profile, AV_PROFILE_HEVC_MAIN as i32);
            assert_eq!((*ctx).flags & AV_CODEC_FLAG_GLOBAL_HEADER as i32, 0);
        }

        use rusty_ffmpeg::ffi::avcodec_free_context;
        // SAFETY: ctx is the sole owner; freeing after test.
        unsafe { avcodec_free_context(&mut { ctx }) };
    }
}
