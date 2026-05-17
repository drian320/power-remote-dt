//! `hevc_nvenc` FFmpeg Main10 encoder backend. Sibling of `hevc_nvenc_encoder.rs`
//! (8-bit). Key differences from the 8-bit sibling:
//!   - HW frames pool uses `sw_format = AV_PIX_FMT_P010LE` (10-bit 4:2:0).
//!   - CPU-side conversion: BGRA8 → P010LE via `sws_scale` (Choice C-2).
//!   - Profile: `AV_PROFILE_HEVC_MAIN_10` (2); HDR10 VUI/SEI color metadata
//!     set on the codec context by `apply_low_latency_hevc_nvenc_main10`.
//!   - No BSF chain — `hevc_nvenc` emits Annex-B natively (same as 8-bit NVENC).
//!   - NPP Main10 deferred to F3 follow-up — no `hevc_nvenc_npp_main10_encoder.rs`.

use std::ptr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use prdt_media_core::{EncodeError, EncodedPacket};
use rusty_ffmpeg::ffi::{
    av_buffer_ref, av_frame_alloc, av_frame_free, av_frame_get_buffer, av_hwframe_get_buffer,
    av_hwframe_transfer_data as hw_upload, av_opt_set_int, av_packet_alloc, av_packet_free,
    av_packet_unref, avcodec_free_context, avcodec_receive_packet, avcodec_send_frame,
    sws_freeContext, sws_getContext, sws_scale, AVCodecContext, AVFrame, AV_OPT_SEARCH_CHILDREN,
    AV_PICTURE_TYPE_I, AV_PIX_FMT_BGRA, AV_PIX_FMT_CUDA, AV_PIX_FMT_P010LE, AV_PKT_FLAG_KEY,
    SWS_BILINEAR,
};
use rusty_ffmpeg::ffi::{av_hwframe_ctx_alloc, av_hwframe_ctx_init, AVHWFramesContext};

use crate::cuda_hwdevice::CudaHwDevice;
use crate::error::FfmpegError;
use crate::nvenc_common::{emit_first_frame_log, open_nvenc_codec_context_main10, AVERROR_EAGAIN};
use crate::options::EncoderTunables;

pub struct HevcNvencMain10FfmpegEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,
    /// CUDA device index. Reserved for future multi-GPU selection; currently
    /// unused. Tracked as ADR follow-up F3.
    pub cuda_device_index: Option<u32>,
}

impl Default for HevcNvencMain10FfmpegEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            gop_size: 60,
            cuda_device_index: None,
        }
    }
}

pub struct HevcNvencMain10FfmpegEncoder {
    #[allow(dead_code)]
    device: CudaHwDevice,
    // hw frames pool buffer ref (P010LE sw_format); kept alive for hw_frame lifetime.
    frames_buf: NonNull<rusty_ffmpeg::ffi::AVBufferRef>,
    codec_ctx: NonNull<AVCodecContext>,
    // CPU-side P010LE frame written by sws_scale, then uploaded via hw_upload.
    cpu_frame: NonNull<AVFrame>,
    hw_frame: NonNull<AVFrame>,
    // sws_scale context for BGRA8 → P010LE conversion.
    sws_ctx: *mut rusty_ffmpeg::ffi::SwsContext,
    tunables: EncoderTunables,
    seq: u64,
    closed: bool,
    first_frame_logged: AtomicBool,
    last_bitrate_warn_secs: AtomicU64,
}

impl HevcNvencMain10FfmpegEncoder {
    pub fn new(cfg: HevcNvencMain10FfmpegEncoderConfig) -> Result<Self, FfmpegError> {
        // 1. Open HW device.
        let device = CudaHwDevice::open()?;

        // 2. Allocate HW frames pool with P010LE sw_format.
        // SAFETY: device.raw() is a valid AVHWDeviceContext buffer ref owned by device.
        let mut frames_raw_ptr = unsafe { av_hwframe_ctx_alloc(device.raw()) };
        if frames_raw_ptr.is_null() {
            return Err(FfmpegError::HwFrames(
                "av_hwframe_ctx_alloc returned null".into(),
            ));
        }
        // SAFETY: frames_raw_ptr is non-null; data points to the embedded AVHWFramesContext.
        unsafe {
            let ctx = (*frames_raw_ptr).data as *mut AVHWFramesContext;
            (*ctx).format = AV_PIX_FMT_CUDA;
            (*ctx).sw_format = AV_PIX_FMT_P010LE;
            (*ctx).width = cfg.width as i32;
            (*ctx).height = cfg.height as i32;
            (*ctx).initial_pool_size = 4;
        }
        // SAFETY: frames_raw_ptr is a valid uninitialised AVHWFramesContext buffer ref.
        let ret = unsafe { av_hwframe_ctx_init(frames_raw_ptr) };
        if ret < 0 {
            // SAFETY: frames_raw_ptr is the unique owner.
            unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut frames_raw_ptr) };
            return Err(FfmpegError::HwFrames(format!(
                "av_hwframe_ctx_init returned {ret}"
            )));
        }
        // SAFETY: init succeeded; frames_raw_ptr is non-null.
        let frames_buf = unsafe { NonNull::new_unchecked(frames_raw_ptr) };

        let tunables = EncoderTunables {
            bitrate_bps: cfg.initial_bitrate_bps,
            fps: cfg.fps,
            width: cfg.width,
            height: cfg.height,
            gop_size: cfg.gop_size,
        };

        // 3. Open codec context (probe encoder, alloc, apply Main10 tunables,
        //    attach hw_frames_ctx, open with priv-data dict).
        // SAFETY: frames_buf.as_ptr() is a valid AVBufferRef; we bump refcount
        // so open_nvenc_codec_context_main10 can consume the new ref.
        let frames_ref_ptr = unsafe { av_buffer_ref(frames_buf.as_ptr()) };
        let frames_ref = match NonNull::new(frames_ref_ptr) {
            Some(r) => r,
            None => {
                let mut f = frames_buf.as_ptr();
                // SAFETY: frames_buf is the unique owner.
                unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut f) };
                return Err(FfmpegError::HwFrames("av_buffer_ref returned null".into()));
            }
        };
        let codec_ctx = match open_nvenc_codec_context_main10(&tunables, frames_ref) {
            Ok(c) => c,
            Err(e) => {
                let mut f = frames_buf.as_ptr();
                // SAFETY: frames_buf is the unique owner; helper consumed the ref it was given.
                unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut f) };
                return Err(e);
            }
        };

        // 4. (No BSF chain — hevc_nvenc emits Annex-B natively.)

        // 5a. Allocate cpu_frame (P010LE, software side for sws_scale output).
        // SAFETY: av_frame_alloc allocates a zeroed AVFrame or null on OOM.
        let cpu_ptr = unsafe {
            let f = av_frame_alloc();
            if f.is_null() {
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                let mut fr = frames_buf.as_ptr();
                rusty_ffmpeg::ffi::av_buffer_unref(&mut fr);
                return Err(FfmpegError::OpenCodec(-1));
            }
            (*f).format = AV_PIX_FMT_P010LE;
            (*f).width = cfg.width as i32;
            (*f).height = cfg.height as i32;
            // SAFETY: frame fields are set; 32-byte alignment is safe for P010LE.
            let ret = av_frame_get_buffer(f, 32);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                let mut fr = frames_buf.as_ptr();
                rusty_ffmpeg::ffi::av_buffer_unref(&mut fr);
                return Err(FfmpegError::OpenCodec(ret));
            }
            f
        };
        // SAFETY: cpu_ptr is non-null after successful av_frame_get_buffer.
        let cpu_frame = unsafe { NonNull::new_unchecked(cpu_ptr) };

        // 5b. Allocate hw_frame (CUDA surface from pool).
        // SAFETY: frames_buf.as_ptr() is the valid frames buffer.
        let hw_ptr = unsafe {
            let f = av_frame_alloc();
            if f.is_null() {
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                let mut fr = frames_buf.as_ptr();
                rusty_ffmpeg::ffi::av_buffer_unref(&mut fr);
                return Err(FfmpegError::OpenCodec(-1));
            }
            let ret = av_hwframe_get_buffer(frames_buf.as_ptr(), f, 0);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                let mut fr = frames_buf.as_ptr();
                rusty_ffmpeg::ffi::av_buffer_unref(&mut fr);
                return Err(FfmpegError::HwFrames(format!(
                    "av_hwframe_get_buffer returned {ret}"
                )));
            }
            f
        };
        // SAFETY: hw_ptr is non-null after successful av_hwframe_get_buffer.
        let hw_frame = unsafe { NonNull::new_unchecked(hw_ptr) };

        // 6. Allocate sws_scale context: BGRA8 → P010LE, SWS_BILINEAR.
        // SAFETY: all integer args are positive; null filter/param selects defaults.
        let sws_ctx = unsafe {
            sws_getContext(
                cfg.width as i32,
                cfg.height as i32,
                AV_PIX_FMT_BGRA,
                cfg.width as i32,
                cfg.height as i32,
                AV_PIX_FMT_P010LE,
                SWS_BILINEAR as i32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if sws_ctx.is_null() {
            let mut h = hw_frame.as_ptr();
            // SAFETY: hw_frame is the unique owner.
            unsafe { av_frame_free(&mut h) };
            let mut c = cpu_frame.as_ptr();
            // SAFETY: cpu_frame is the unique owner.
            unsafe { av_frame_free(&mut c) };
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            let mut fr = frames_buf.as_ptr();
            // SAFETY: frames_buf is the unique owner.
            unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut fr) };
            return Err(FfmpegError::HwDevice("sws_getContext returned null".into()));
        }

        // 7. Emit encoder_ready event.
        tracing::info!(
            target: "video.pipeline",
            event = "encoder_ready",
            backend = "ffmpeg-nvenc-hevc-main10",
            codec = "h265",
            profile = "main10",
            bitdepth = 10,
            gop = cfg.gop_size,
        );

        Ok(Self {
            device,
            frames_buf,
            codec_ctx,
            cpu_frame,
            hw_frame,
            sws_ctx,
            tunables,
            seq: 0,
            closed: false,
            first_frame_logged: AtomicBool::new(false),
            last_bitrate_warn_secs: AtomicU64::new(0),
        })
    }

    /// Encode one BGRA8 frame to HEVC Main10 Annex-B (emitted natively by hevc_nvenc).
    pub fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        if self.closed {
            return Err(EncodeError::Backend("encoder closed".into()));
        }

        let cpu = self.cpu_frame.as_ptr();
        let hw = self.hw_frame.as_ptr();

        // 1. BGRA8 → P010LE via sws_scale (CPU; sole CPU→format-convert site in this file).
        // SAFETY: bgra slice has width*height*4 bytes; cpu frame buffers are allocated.
        unsafe {
            let src_ptr = bgra.as_ptr();
            let src_stride = (width as i32) * 4;
            let src_planes: [*const u8; 4] = [src_ptr, ptr::null(), ptr::null(), ptr::null()];
            let src_strides: [i32; 4] = [src_stride, 0, 0, 0];
            let dst_planes: [*mut u8; 4] = [
                (*cpu).data[0],
                (*cpu).data[1],
                ptr::null_mut(),
                ptr::null_mut(),
            ];
            let dst_strides: [i32; 4] = [(*cpu).linesize[0], (*cpu).linesize[1], 0, 0];
            sws_scale(
                self.sws_ctx,
                src_planes.as_ptr(),
                src_strides.as_ptr(),
                0,
                height as i32,
                dst_planes.as_ptr(),
                dst_strides.as_ptr(),
            );
        }

        // 2. Upload: CPU P010LE → GPU CUDA surface. Sole CPU→GPU transfer in this file
        // (per-backend A9b invariant enforced by CI grep guard).
        // SAFETY: hw and cpu are valid non-null AVFrames; 0 flags required by the API.
        let ret = unsafe { hw_upload(hw, cpu, 0) };
        if ret < 0 {
            return Err(FfmpegError::Transfer(ret).into());
        }

        // 3. Set picture type for IDR forcing.
        // SAFETY: hw is a valid AVFrame owned by self.
        unsafe {
            if force_idr {
                (*hw).pict_type = AV_PICTURE_TYPE_I;
                (*hw).key_frame = 1;
            } else {
                (*hw).pict_type = 0;
                (*hw).key_frame = 0;
            }
        }

        // 4. PTS rescale: ts_us (microseconds) → 1/fps time_base units.
        // SAFETY: hw is a valid AVFrame.
        unsafe {
            (*hw).pts = (ts_us as i64 * self.tunables.fps as i64) / 1_000_000;
        }

        // 5. Send frame; drain on EAGAIN.
        let ctx = self.codec_ctx.as_ptr();
        // SAFETY: ctx is a valid open AVCodecContext; hw is a valid AVFrame.
        let mut send_ret = unsafe { avcodec_send_frame(ctx, hw) };
        if send_ret == AVERROR_EAGAIN {
            let drain_pkt = {
                // SAFETY: av_packet_alloc always returns a zeroed packet or null.
                let p = unsafe { av_packet_alloc() };
                if p.is_null() {
                    return Err(EncodeError::Backend(
                        "av_packet_alloc failed (drain)".into(),
                    ));
                }
                p
            };
            // SAFETY: ctx is open; drain_pkt is freshly allocated.
            unsafe { avcodec_receive_packet(ctx, drain_pkt) };
            // SAFETY: drain_pkt is the unique owner.
            unsafe {
                av_packet_unref(drain_pkt);
                av_packet_free(&mut { drain_pkt });
            }
            // SAFETY: retry send after drain.
            send_ret = unsafe { avcodec_send_frame(ctx, hw) };
        }
        if send_ret < 0 {
            return Err(FfmpegError::Send(send_ret).into());
        }

        // 6. Receive encoded packet (Annex-B directly — no BSF).
        // SAFETY: av_packet_alloc returns zeroed packet or null.
        let pkt = unsafe { av_packet_alloc() };
        if pkt.is_null() {
            return Err(EncodeError::Backend("av_packet_alloc failed".into()));
        }
        // SAFETY: ctx is open; pkt is freshly allocated.
        let recv_ret = unsafe { avcodec_receive_packet(ctx, pkt) };
        if recv_ret < 0 {
            // SAFETY: pkt is still the unique owner.
            unsafe {
                av_packet_unref(pkt);
                av_packet_free(&mut { pkt });
            }
            return Err(FfmpegError::Receive(recv_ret).into());
        }

        // 7. Copy bytes and detect keyframe.
        let (nal_bytes, is_keyframe) = {
            // SAFETY: pkt.data/size are valid after successful avcodec_receive_packet.
            let (data_ptr, size, flags) =
                unsafe { ((*pkt).data, (*pkt).size as usize, (*pkt).flags) };
            // SAFETY: data_ptr is valid for `size` bytes for the duration of pkt's lifetime.
            let slice = unsafe { std::slice::from_raw_parts(data_ptr, size) };
            let bytes = slice.to_vec();
            let key = (flags & AV_PKT_FLAG_KEY as i32) != 0;
            // SAFETY: pkt is the unique owner; unref before free.
            unsafe {
                av_packet_unref(pkt);
                av_packet_free(&mut { pkt });
            }
            (bytes, key)
        };

        // 8. First-frame log (shared helper from nvenc_common).
        emit_first_frame_log(
            self.seq,
            "hevc_nvenc",
            "cuda",
            "sws_scale",
            self.tunables.width,
            self.tunables.height,
            &self.first_frame_logged,
        );

        self.seq += 1;
        Ok(EncodedPacket {
            nal_bytes,
            is_keyframe,
            timestamp_us: ts_us,
        })
    }

    pub fn set_target_bitrate(&mut self, bps: u32) -> Result<(), EncodeError> {
        let ctx = self.codec_ctx.as_ptr();
        // SAFETY: ctx is a valid open AVCodecContext; "b" is the standard bitrate option.
        let ret = unsafe {
            av_opt_set_int(
                ctx.cast(),
                c"b".as_ptr(),
                bps as i64,
                AV_OPT_SEARCH_CHILDREN as i32,
            )
        };
        if ret < 0 {
            return Err(EncodeError::Backend(format!(
                "av_opt_set_int(b={bps}) returned {ret}"
            )));
        }
        self.tunables.bitrate_bps = bps;
        Ok(())
    }

    pub fn backend_name(&self) -> &'static str {
        "ffmpeg-nvenc-hevc-main10"
    }

    /// Rate-limited bitrate failure warning (at most once per 60 seconds).
    pub(crate) fn maybe_warn_bitrate_failure(&self, e: &EncodeError, bps: u32) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = self.last_bitrate_warn_secs.load(Ordering::Relaxed);
        if now_secs.saturating_sub(last) >= 60 {
            self.last_bitrate_warn_secs
                .store(now_secs, Ordering::Relaxed);
            tracing::warn!(
                target: "video.pipeline",
                error = %e,
                bps,
                "set_target_bitrate failed (rate-limited warn)"
            );
        }
    }
}

impl Drop for HevcNvencMain10FfmpegEncoder {
    fn drop(&mut self) {
        // Reverse-creation order: sws → hw_frame → cpu_frame → codec_ctx → frames → device.
        // SAFETY: sws_ctx is the unique owner of the SwsContext.
        unsafe { sws_freeContext(self.sws_ctx) };

        let mut hw = self.hw_frame.as_ptr();
        // SAFETY: hw_frame is the unique owner of the CUDA surface ref.
        unsafe { av_frame_free(&mut hw) };

        let mut cpu = self.cpu_frame.as_ptr();
        // SAFETY: cpu_frame is the unique owner of the P010LE CPU frame.
        unsafe { av_frame_free(&mut cpu) };

        let mut ctx = self.codec_ctx.as_ptr();
        // SAFETY: codec_ctx is the unique owner (hw_frames_ctx consumed by avcodec_open2).
        unsafe { avcodec_free_context(&mut ctx) };

        let mut fr = self.frames_buf.as_ptr();
        // SAFETY: frames_buf is the unique owner of the P010LE HW frames pool.
        unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut fr) };

        // device drops via its own Drop impl.
        let _ = &self.device;
    }
}

// SAFETY: all raw FFmpeg pointers are owned exclusively by this struct and
// accessed only from the thread that calls encode() / drop(). The struct is
// never aliased across threads; spawn_blocking moves sole ownership in and out.
unsafe impl Send for HevcNvencMain10FfmpegEncoder {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_fields() {
        let cfg = HevcNvencMain10FfmpegEncoderConfig::default();
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.height, 1080);
        assert_eq!(cfg.fps, 60);
        assert_eq!(cfg.initial_bitrate_bps, 8_000_000);
        assert_eq!(cfg.gop_size, 60);
        assert!(cfg.cuda_device_index.is_none());
    }

    #[test]
    #[ignore = "requires NVIDIA hevc_nvenc Main10 encode"]
    fn small_frame_emits_idr_main10() {
        let cfg = HevcNvencMain10FfmpegEncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            initial_bitrate_bps: 4_000_000,
            gop_size: 30,
            cuda_device_index: None,
        };
        let mut enc = HevcNvencMain10FfmpegEncoder::new(cfg).expect("encoder created");
        let bgra = vec![0u8; 320 * 240 * 4];
        let pkt = enc.encode(&bgra, 320, 240, true, 0).expect("encoded");
        assert!(pkt.is_keyframe);
        assert!(pkt.nal_bytes.starts_with(&[0, 0, 0, 1]));
    }
}
