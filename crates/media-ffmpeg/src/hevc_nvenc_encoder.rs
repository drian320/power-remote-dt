//! `hevc_nvenc` FFmpeg encoder backend. Mirrors `HevcVaapiFfmpegEncoder` but
//! drives NVIDIA NVENC via libavcodec's `hevc_nvenc`. Key asymmetries with
//! the VAAPI sibling:
//!   - Uses `CudaHwDevice` / `CudaHwFrames` (AV_HWDEVICE_TYPE_CUDA;
//!     AV_PIX_FMT_CUDA with NV12 sw_format).
//!   - No BSF chain — `hevc_nvenc` emits Annex-B natively, so there is no
//!     `hevc_mp4toannexb` bitstream filter (the VAAPI side needs it because
//!     the H.265-in-MP4 length-prefixed shape is what libavcodec hands back).
//!   - Encoder name `"hevc_nvenc"`; private-data dict from
//!     `build_priv_data_dict_nvenc(gop_size)`.
//!
//! Exactly one CPU→GPU upload (the renamed `hw_upload` symbol) lives in this
//! file (the per-backend single-upload invariant enforced by CI's A9b grep
//! guard).

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use prdt_media_core::{EncodeError, EncodedPacket};
use prdt_media_sw::{i420_to_nv12_into, I420Frame};
use rusty_ffmpeg::ffi::{
    av_buffer_ref, av_frame_free, av_frame_get_buffer, av_hwframe_get_buffer,
    av_hwframe_transfer_data as hw_upload, av_opt_set_int, av_packet_alloc, av_packet_free,
    av_packet_unref, avcodec_free_context, avcodec_receive_packet, avcodec_send_frame,
    AVCodecContext, AVFrame, AV_OPT_SEARCH_CHILDREN, AV_PICTURE_TYPE_I, AV_PKT_FLAG_KEY,
};

use crate::cuda_hwdevice::CudaHwDevice;
use crate::cuda_hwframes::CudaHwFrames;
use crate::error::FfmpegError;
use crate::nvenc_common::{emit_first_frame_log, open_nvenc_codec_context, AVERROR_EAGAIN};
use crate::options::EncoderTunables;

pub struct HevcNvencFfmpegEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,
    /// CUDA device index. Reserved for future multi-GPU selection; currently
    /// unused (device is picked by `CUDA_VISIBLE_DEVICES` env / default 0).
    /// Tracked as ADR follow-up F3.
    pub cuda_device_index: Option<u32>,
}

impl Default for HevcNvencFfmpegEncoderConfig {
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

pub struct HevcNvencFfmpegEncoder {
    #[allow(dead_code)]
    device: CudaHwDevice,
    #[allow(dead_code)]
    frames: CudaHwFrames,
    codec_ctx: NonNull<AVCodecContext>,
    cpu_frame: NonNull<AVFrame>,
    hw_frame: NonNull<AVFrame>,
    tunables: EncoderTunables,
    seq: u64,
    closed: bool,
    first_frame_logged: AtomicBool,
    last_bitrate_warn_secs: AtomicU64,
}

impl HevcNvencFfmpegEncoder {
    pub fn new(cfg: HevcNvencFfmpegEncoderConfig) -> Result<Self, FfmpegError> {
        // 1. Open HW device + frames.
        let device = CudaHwDevice::open()?;
        let frames = CudaHwFrames::new(&device, cfg.width, cfg.height)?;

        let tunables = EncoderTunables {
            bitrate_bps: cfg.initial_bitrate_bps,
            fps: cfg.fps,
            width: cfg.width,
            height: cfg.height,
            gop_size: cfg.gop_size,
        };

        // 2. Open codec context (probe encoder, alloc, apply tunables, attach
        //    hw_frames_ctx, open with priv-data dict). Shared with the NPP
        //    encoder via nvenc_common to keep both encoders byte-stable on
        //    libavcodec init order (A13).
        // SAFETY: frames.raw() is a valid AVBufferRef owned by frames; we
        // bump its refcount so the helper can consume the new ref.
        let frames_ref_ptr = unsafe { av_buffer_ref(frames.raw()) };
        let frames_ref = NonNull::new(frames_ref_ptr)
            .ok_or_else(|| FfmpegError::HwFrames("av_buffer_ref returned null".into()))?;
        let codec_ctx = open_nvenc_codec_context(&tunables, frames_ref)?;

        // 3. (No BSF chain — hevc_nvenc emits Annex-B natively.)

        // 4a. Allocate cpu_frame (NV12, software side).
        // SAFETY: av_frame_alloc allocates a zeroed AVFrame; always returns non-null or null on OOM.
        let cpu_ptr = unsafe {
            use rusty_ffmpeg::ffi::{av_frame_alloc, AV_PIX_FMT_NV12};
            let f = av_frame_alloc();
            if f.is_null() {
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(-1));
            }
            (*f).format = AV_PIX_FMT_NV12;
            (*f).width = cfg.width as i32;
            (*f).height = cfg.height as i32;
            // SAFETY: frame fields are set; 32-byte alignment is safe for NV12.
            let ret = av_frame_get_buffer(f, 32);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(ret));
            }
            f
        };
        // SAFETY: cpu_ptr is non-null after successful av_frame_get_buffer.
        let cpu_frame = unsafe { NonNull::new_unchecked(cpu_ptr) };

        // 4b. Allocate hw_frame (CUDA surface from pool).
        // SAFETY: frames.raw() is the valid frames buffer; hw_ptr is the out-param address.
        let hw_ptr = unsafe {
            use rusty_ffmpeg::ffi::av_frame_alloc;
            let f = av_frame_alloc();
            if f.is_null() {
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(-1));
            }
            let ret = av_hwframe_get_buffer(frames.raw(), f, 0);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::HwFrames(format!(
                    "av_hwframe_get_buffer returned {ret}"
                )));
            }
            f
        };
        // SAFETY: hw_ptr is non-null after successful av_hwframe_get_buffer.
        let hw_frame = unsafe { NonNull::new_unchecked(hw_ptr) };

        // 5. Emit encoder_ready event.
        tracing::info!(
            target: "video.pipeline",
            event = "encoder_ready",
            backend = "ffmpeg-nvenc-hevc",
            codec = "h265",
            profile = "main",
            bitdepth = 8,
            gop = cfg.gop_size,
        );

        Ok(Self {
            device,
            frames,
            codec_ctx,
            cpu_frame,
            hw_frame,
            tunables,
            seq: 0,
            closed: false,
            first_frame_logged: AtomicBool::new(false),
            last_bitrate_warn_secs: AtomicU64::new(0),
        })
    }

    pub fn encode(
        &mut self,
        frame: &I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        if self.closed {
            return Err(EncodeError::Backend("encoder closed".into()));
        }

        let cpu = self.cpu_frame.as_ptr();
        let hw = self.hw_frame.as_ptr();

        // 1. I420 → NV12 into cpu_frame planes.
        // SAFETY: cpu is a valid AVFrame with allocated buffers (data[0]=Y, data[1]=UV).
        let (y_dst, uv_dst, y_stride, uv_stride) = unsafe {
            let y_ptr = (*cpu).data[0];
            let uv_ptr = (*cpu).data[1];
            let y_ls = (*cpu).linesize[0] as usize;
            let uv_ls = (*cpu).linesize[1] as usize;
            let h = frame.height as usize;
            let y_slice = std::slice::from_raw_parts_mut(y_ptr, y_ls * h);
            let uv_slice = std::slice::from_raw_parts_mut(uv_ptr, uv_ls * (h / 2));
            (y_slice, uv_slice, y_ls, uv_ls)
        };
        i420_to_nv12_into(frame, y_dst, y_stride, uv_dst, uv_stride);

        // 2. Upload: CPU → GPU. This is the sole CPU→GPU transfer site in this file
        // (per-backend A9b invariant enforced by CI grep guard).
        // SAFETY: hw and cpu are valid non-null AVFrames; 0 flags is required by the API.
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
            // Drain one packet then retry.
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

        // 8. First-frame log (shared with NPP encoder via nvenc_common).
        emit_first_frame_log(
            self.seq,
            "hevc_nvenc",
            "cuda",
            "sw",
            self.tunables.width,
            self.tunables.height,
            &self.first_frame_logged,
        );

        // 9. Advance seq counter and return.
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
        "ffmpeg-nvenc-hevc"
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

impl Drop for HevcNvencFfmpegEncoder {
    fn drop(&mut self) {
        // Reverse-creation order: hw_frame → cpu_frame → codec_ctx → frames → device.
        let mut hw = self.hw_frame.as_ptr();
        // SAFETY: hw_frame is the unique owner of the CUDA surface ref.
        unsafe { av_frame_free(&mut hw) };

        let mut cpu = self.cpu_frame.as_ptr();
        // SAFETY: cpu_frame is the unique owner of the NV12 CPU frame.
        unsafe { av_frame_free(&mut cpu) };

        let mut ctx = self.codec_ctx.as_ptr();
        // SAFETY: codec_ctx is the unique owner (hw_frames_ctx was consumed by avcodec_open2).
        unsafe { avcodec_free_context(&mut ctx) };

        // frames and device drop via their own Drop impls (av_buffer_unref).
        // Rust drops struct fields in declaration order, so frames drops before device,
        // matching creation order (device created first → device destroyed last).
    }
}

// Integration tests — require real NVIDIA HW with hevc_nvenc encode support.
#[cfg(test)]
mod tests {
    use super::*;

    fn default_config(w: u32, h: u32, gop: u32) -> HevcNvencFfmpegEncoderConfig {
        HevcNvencFfmpegEncoderConfig {
            width: w,
            height: h,
            fps: 30,
            initial_bitrate_bps: 4_000_000,
            gop_size: gop,
            cuda_device_index: None,
        }
    }

    #[test]
    #[ignore = "requires NVIDIA hevc_nvenc encode"]
    fn small_frame_emits_idr() {
        let cfg = default_config(320, 240, 30);
        let mut enc = HevcNvencFfmpegEncoder::new(cfg).expect("encoder created");
        let frame = I420Frame::new_packed(320, 240).expect("frame");
        let pkt = enc.encode(&frame, true, 0).expect("encoded");
        assert!(pkt.is_keyframe);
        // hevc_nvenc emits Annex-B directly — first 4 bytes are a 0x00000001 start code.
        assert!(pkt.nal_bytes.starts_with(&[0, 0, 0, 1]));
    }

    #[test]
    fn allocates_config_with_defaults() {
        let cfg = HevcNvencFfmpegEncoderConfig::default();
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.fps, 60);
        assert!(cfg.cuda_device_index.is_none());
    }
}
