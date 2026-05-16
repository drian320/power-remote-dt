//! `hevc_nvenc` FFmpeg encoder backend with on-GPU BGRA→NV12 via CUDA NPP
//! (P2.5). Mirrors `HevcNvencFfmpegEncoder` but bypasses the CPU
//! `bgra_to_i420` + `i420_to_nv12_into` + `hw_upload` chain — the new path
//! accepts BGRA from capture directly, uploads once via `cudaMemcpy2D`,
//! runs the NPP color conversion on the GPU, and writes the NV12 planes
//! directly into the encoder's CUDA hwframe surface.
//!
//! Key asymmetries with the sibling `hevc_nvenc_encoder`:
//!   - `encode()` takes `BgraFrame` (4-bpp host buffer) instead of
//!     `I420Frame`.
//!   - No `cpu_frame` — NPP writes straight into `hw_frame.data[0/1]`.
//!   - No `av_hwframe_transfer_data` (renamed `hw_upload`) call site —
//!     the per-file A10 grep guard sees ZERO matches in this file. The
//!     `cudaMemcpy2D` (and the unused-in-P2.5 `cudaMemcpy2DAsync`) NPP
//!     uploads are the deliberate by-design replacement; see the
//!     `// ci-allow: cuda-direct` carve-out at `cuda_npp.rs`.
//!
//! All bookkeeping (codec context open, drain on EAGAIN, PTS rescale,
//! first-frame log) is delegated to `nvenc_common` so this file and
//! `hevc_nvenc_encoder` cannot drift on libavcodec sequencing.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use prdt_media_core::{BgraFrame, EncodeError, EncodedPacket};
use rusty_ffmpeg::ffi::{
    av_buffer_ref, av_frame_free, av_hwframe_get_buffer, av_opt_set_int, av_packet_alloc,
    av_packet_free, av_packet_unref, avcodec_free_context, avcodec_receive_packet,
    avcodec_send_frame, AVCodecContext, AVFrame, AV_OPT_SEARCH_CHILDREN, AV_PICTURE_TYPE_I,
    AV_PKT_FLAG_KEY,
};

use crate::cuda_hwdevice::CudaHwDevice;
use crate::cuda_hwframes::CudaHwFrames;
use crate::cuda_npp::CudaNppContext;
use crate::error::FfmpegError;
use crate::nvenc_common::{emit_first_frame_log, open_nvenc_codec_context, AVERROR_EAGAIN};
use crate::options::EncoderTunables;

pub struct HevcNvencNppFfmpegEncoderConfig {
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

impl Default for HevcNvencNppFfmpegEncoderConfig {
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

pub struct HevcNvencNppFfmpegEncoder {
    #[allow(dead_code)]
    device: CudaHwDevice,
    #[allow(dead_code)]
    frames: CudaHwFrames,
    codec_ctx: NonNull<AVCodecContext>,
    hw_frame: NonNull<AVFrame>,
    npp_ctx: CudaNppContext,
    tunables: EncoderTunables,
    seq: u64,
    closed: bool,
    first_frame_logged: AtomicBool,
    last_bitrate_warn_secs: AtomicU64,
}

impl HevcNvencNppFfmpegEncoder {
    pub fn new(cfg: HevcNvencNppFfmpegEncoderConfig) -> Result<Self, FfmpegError> {
        // 1. Open HW device + frames pool (same as the non-NPP NVENC encoder).
        let device = CudaHwDevice::open()?;
        let frames = CudaHwFrames::new(&device, cfg.width, cfg.height)?;

        let tunables = EncoderTunables {
            bitrate_bps: cfg.initial_bitrate_bps,
            fps: cfg.fps,
            width: cfg.width,
            height: cfg.height,
            gop_size: cfg.gop_size,
        };

        // 2. Open codec context via shared nvenc_common helper.
        // SAFETY: frames.raw() is a valid AVBufferRef owned by frames; we
        // bump its refcount so the helper can consume the new ref.
        let frames_ref_ptr = unsafe { av_buffer_ref(frames.raw()) };
        let frames_ref = NonNull::new(frames_ref_ptr)
            .ok_or_else(|| FfmpegError::HwFrames("av_buffer_ref returned null".into()))?;
        let codec_ctx = open_nvenc_codec_context(&tunables, frames_ref)?;

        // 3. Allocate hw_frame (CUDA surface from pool). NO cpu_frame —
        //    NPP writes straight into hw_frame's planes via cudaMemcpy2D.
        // SAFETY: frames.raw() is the valid frames buffer; codec_ctx is
        // valid; the AVFrame from av_frame_alloc is zeroed.
        let hw_ptr = unsafe {
            use rusty_ffmpeg::ffi::av_frame_alloc;
            let f = av_frame_alloc();
            if f.is_null() {
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(-1));
            }
            let ret = av_hwframe_get_buffer(frames.raw(), f, 0);
            if ret < 0 {
                av_frame_free(&mut { f });
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

        // 4. NPP context — allocates per-encoder device BGRA buf + planar
        //    I420 scratch buffers. Must be AFTER CudaHwDevice::open() so
        //    NPP shares the libavcodec-installed thread-current CUDA
        //    context (Risk row 2).
        let npp_ctx = match CudaNppContext::new(cfg.width, cfg.height) {
            Ok(c) => c,
            Err(e) => {
                let mut hw = hw_frame.as_ptr();
                // SAFETY: hw_frame is the unique owner of the CUDA surface ref.
                unsafe { av_frame_free(&mut hw) };
                let mut p = codec_ctx.as_ptr();
                // SAFETY: codec_ctx is the unique owner.
                unsafe { avcodec_free_context(&mut p) };
                return Err(e);
            }
        };

        // 5. Emit encoder_ready event.
        tracing::info!(
            target: "video.pipeline",
            event = "encoder_ready",
            backend = "ffmpeg-nvenc-hevc-npp",
            codec = "h265",
            profile = "main",
            bitdepth = 8,
            gop = cfg.gop_size,
        );

        Ok(Self {
            device,
            frames,
            codec_ctx,
            hw_frame,
            npp_ctx,
            tunables,
            seq: 0,
            closed: false,
            first_frame_logged: AtomicBool::new(false),
            last_bitrate_warn_secs: AtomicU64::new(0),
        })
    }

    pub fn encode(
        &mut self,
        frame: &BgraFrame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        if self.closed {
            return Err(EncodeError::Backend("encoder closed".into()));
        }
        if frame.width != self.tunables.width || frame.height != self.tunables.height {
            return Err(EncodeError::Backend(format!(
                "BGRA frame size ({}x{}) does not match encoder ({}x{})",
                frame.width, frame.height, self.tunables.width, self.tunables.height
            )));
        }

        let hw = self.hw_frame.as_ptr();

        // 1. GPU BGRA → NV12 directly into hw_frame's CUDA planes.
        //    Replaces the i420_to_nv12_into + hw_upload pair of the non-NPP
        //    NVENC path. Zero CPU pixel passes; one PCIe HtoD upload of
        //    BGRA (vs the existing NV12 upload — net PCIe Tx grows ~2.7×
        //    per plan §6 driver 2).
        self.npp_ctx
            .convert_bgra_to_nv12_into_av_frame(&frame.bgra, hw)?;

        // 2. Set picture type for IDR forcing.
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

        // 3. PTS rescale: ts_us (microseconds) → 1/fps time_base units.
        // SAFETY: hw is a valid AVFrame.
        unsafe {
            (*hw).pts = (ts_us as i64 * self.tunables.fps as i64) / 1_000_000;
        }

        // 4. Send frame; drain on EAGAIN (identical logic to the non-NPP
        //    NVENC encoder so the two paths can't drift on EAGAIN
        //    handling).
        let ctx = self.codec_ctx.as_ptr();
        // SAFETY: ctx is a valid open AVCodecContext; hw is a valid AVFrame.
        let mut send_ret = unsafe { avcodec_send_frame(ctx, hw) };
        if send_ret == AVERROR_EAGAIN {
            let drain_pkt = {
                // SAFETY: av_packet_alloc returns zeroed packet or null.
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

        // 5. Receive encoded packet (Annex-B directly — no BSF).
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

        // 6. Copy bytes and detect keyframe.
        let (nal_bytes, is_keyframe) = {
            // SAFETY: pkt.data/size valid after successful avcodec_receive_packet.
            let (data_ptr, size, flags) =
                unsafe { ((*pkt).data, (*pkt).size as usize, (*pkt).flags) };
            // SAFETY: data_ptr is valid for `size` bytes for the lifetime of pkt.
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

        // 7. First-frame log (shared helper, convert_path="npp").
        emit_first_frame_log(
            self.seq,
            "hevc_nvenc",
            "cuda",
            "npp",
            self.tunables.width,
            self.tunables.height,
            &self.first_frame_logged,
        );

        // 8. Advance seq counter and return.
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
        "ffmpeg-nvenc-hevc-npp"
    }

    /// Rate-limited bitrate failure warning (at most once per 60 seconds).
    /// Mirrors the non-NPP NVENC encoder's helper exactly.
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

impl Drop for HevcNvencNppFfmpegEncoder {
    fn drop(&mut self) {
        // Reverse-creation order: hw_frame → codec_ctx → npp_ctx → frames → device.
        // (npp_ctx drains its CUDA stream in its own Drop; frames + device
        // drop via their own Drop impls.)
        let mut hw = self.hw_frame.as_ptr();
        // SAFETY: hw_frame is the unique owner of the CUDA surface ref.
        unsafe { av_frame_free(&mut hw) };

        let mut ctx = self.codec_ctx.as_ptr();
        // SAFETY: codec_ctx is the unique owner (hw_frames_ctx was consumed
        // by avcodec_open2).
        unsafe { avcodec_free_context(&mut ctx) };

        // npp_ctx, frames, device drop via their Drop impls in declaration
        // order: codec_ctx (just freed) → hw_frame (just freed) → npp_ctx
        // → frames → device. NPP context drain happens INSIDE CudaNppContext::drop
        // (cudaStreamSynchronize before device buffer frees) — see cuda_npp.rs.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config(w: u32, h: u32, gop: u32) -> HevcNvencNppFfmpegEncoderConfig {
        HevcNvencNppFfmpegEncoderConfig {
            width: w,
            height: h,
            fps: 30,
            initial_bitrate_bps: 4_000_000,
            gop_size: gop,
            cuda_device_index: None,
        }
    }

    #[test]
    fn allocates_config_with_defaults() {
        let cfg = HevcNvencNppFfmpegEncoderConfig::default();
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.fps, 60);
        assert!(cfg.cuda_device_index.is_none());
    }

    /// Constructor failure path on a CUDA-less host (dev container).
    /// `CudaHwDevice::open()` fails before NPP context allocation;
    /// surfaces as `FfmpegError::HwDevice` or `EncoderNotFound`. Mirrors
    /// `hevc_nvenc_encoder` and `cuda_hwdevice::tests::open_fails_cleanly_without_cuda`.
    #[test]
    fn new_fails_cleanly_without_cuda() {
        let cfg = default_config(320, 240, 30);
        let result = HevcNvencNppFfmpegEncoder::new(cfg);
        assert!(
            matches!(
                result,
                Err(FfmpegError::HwDevice(_)) | Err(FfmpegError::EncoderNotFound(_))
            ),
            "expected HwDevice or EncoderNotFound on a CUDA-less host"
        );
    }

    #[test]
    #[ignore = "requires NVIDIA hevc_nvenc encode + libnppicc; gated to smoke runner"]
    fn small_bgra_frame_emits_idr() {
        let cfg = default_config(320, 240, 30);
        let mut enc = HevcNvencNppFfmpegEncoder::new(cfg).expect("encoder created");
        let bgra = vec![0u8; (320 * 4 * 240) as usize];
        let frame = BgraFrame {
            width: 320,
            height: 240,
            bgra,
            stride: 320 * 4,
        };
        let pkt = enc.encode(&frame, true, 0).expect("encoded");
        assert!(pkt.is_keyframe);
        assert!(pkt.nal_bytes.starts_with(&[0, 0, 0, 1]));
    }
}
