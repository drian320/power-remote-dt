use std::path::PathBuf;
use std::ptr;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use prdt_media_core::{EncodeError, EncodedPacket};
use prdt_media_sw::{i420_to_nv12_into, I420Frame};
use rusty_ffmpeg::ffi::{
    av_bsf_alloc, av_bsf_free, av_bsf_get_by_name, av_bsf_init, av_bsf_receive_packet,
    av_bsf_send_packet, av_buffer_ref, av_frame_free, av_frame_get_buffer, av_hwframe_get_buffer,
    av_hwframe_transfer_data as hw_upload, av_opt_set_int, av_packet_alloc, av_packet_free,
    av_packet_unref, avcodec_alloc_context3, avcodec_find_encoder_by_name, avcodec_free_context,
    avcodec_open2, avcodec_parameters_from_context, avcodec_receive_packet, avcodec_send_frame,
    AVBSFContext, AVCodecContext, AVFrame, AVPictureType_AV_PICTURE_TYPE_I, AV_OPT_SEARCH_CHILDREN,
    AV_PKT_FLAG_KEY,
};

use crate::error::FfmpegError;
use crate::hwdevice::VaapiHwDevice;
use crate::hwframes::VaapiHwFrames;
use crate::options::{apply_low_latency_hevc, build_priv_data_dict, EncoderTunables};

// AVERROR(EAGAIN) = -(EAGAIN) = -11 on Linux.
const AVERROR_EAGAIN: i32 = -11;

pub struct HevcVaapiFfmpegEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,
    pub render_node: Option<PathBuf>,
}

impl Default for HevcVaapiFfmpegEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 8_000_000,
            gop_size: 60,
            render_node: None,
        }
    }
}

pub struct HevcVaapiFfmpegEncoder {
    device: VaapiHwDevice,
    frames: VaapiHwFrames,
    codec_ctx: NonNull<AVCodecContext>,
    cpu_frame: NonNull<AVFrame>,
    hw_frame: NonNull<AVFrame>,
    bsf_ctx: NonNull<AVBSFContext>,
    tunables: EncoderTunables,
    seq: u64,
    closed: bool,
    first_frame_logged: AtomicBool,
    last_bitrate_warn_secs: AtomicU64,
}

impl HevcVaapiFfmpegEncoder {
    pub fn new(cfg: HevcVaapiFfmpegEncoderConfig) -> Result<Self, FfmpegError> {
        // 1. Runtime-probe encoder.
        // SAFETY: string literal is a valid nul-terminated C string.
        let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_vaapi".as_ptr()) };
        if codec.is_null() {
            return Err(FfmpegError::EncoderNotFound("hevc_vaapi"));
        }

        // 2. Open HW device + frames.
        let device = VaapiHwDevice::open(cfg.render_node.as_deref())?;
        let frames = VaapiHwFrames::new(&device, cfg.width, cfg.height)?;

        let tunables = EncoderTunables {
            bitrate_bps: cfg.initial_bitrate_bps,
            fps: cfg.fps,
            width: cfg.width,
            height: cfg.height,
            gop_size: cfg.gop_size,
        };

        // 3. Allocate codec context + apply tunables.
        // SAFETY: codec is a valid non-null AVCodec pointer from avcodec_find_encoder_by_name.
        let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
        if codec_ctx_ptr.is_null() {
            return Err(FfmpegError::OpenCodec(-1));
        }
        // SAFETY: codec_ctx_ptr is a freshly allocated, unopened AVCodecContext.
        unsafe { apply_low_latency_hevc(codec_ctx_ptr, &tunables) };

        // 4. Set hw_frames_ctx — avcodec_open2 will take ownership of this ref.
        // SAFETY: frames.raw() is a valid AVBufferRef owned by frames.
        let frames_ref = unsafe { av_buffer_ref(frames.raw()) };
        if frames_ref.is_null() {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; freeing on error path.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::HwFrames("av_buffer_ref returned null".into()));
        }
        // SAFETY: codec_ctx_ptr is valid and not yet opened; hw_frames_ctx takes ownership.
        unsafe { (*codec_ctx_ptr).hw_frames_ctx = frames_ref };

        // 5. Open codec with priv_data_dict (avcodec_open2 consumes the dict).
        let dict = build_priv_data_dict(cfg.gop_size)?;
        // SAFETY: codec_ctx_ptr, codec, and dict are all valid; avcodec_open2 frees dict on success.
        let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, &mut dict.as_ptr()) };
        if ret < 0 {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner at this point.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(ret));
        }

        // SAFETY: avcodec_alloc_context3 succeeded; pointer is non-null.
        let codec_ctx = unsafe { NonNull::new_unchecked(codec_ctx_ptr) };

        // 6. BSF: hevc_mp4toannexb.
        // SAFETY: string literal is a valid nul-terminated C string.
        let bsf_filter = unsafe { av_bsf_get_by_name(c"hevc_mp4toannexb".as_ptr()) };
        if bsf_filter.is_null() {
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::Bsf(-1));
        }
        let mut bsf_ptr: *mut AVBSFContext = ptr::null_mut();
        // SAFETY: bsf_filter is non-null; bsf_ptr is the out-param address.
        let ret = unsafe { av_bsf_alloc(bsf_filter, &mut bsf_ptr) };
        if ret < 0 || bsf_ptr.is_null() {
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::Bsf(ret));
        }
        // Copy codec params into BSF.
        // SAFETY: bsf_ptr and codec_ctx_ptr are both valid; par_in is allocated by av_bsf_alloc.
        let ret = unsafe { avcodec_parameters_from_context((*bsf_ptr).par_in, codec_ctx.as_ptr()) };
        if ret < 0 {
            let mut b = bsf_ptr;
            // SAFETY: bsf_ptr is the unique owner.
            unsafe { av_bsf_free(&mut b) };
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::Bsf(ret));
        }
        // SAFETY: bsf_ptr is valid and params are set; init finalises the BSF.
        let ret = unsafe { av_bsf_init(bsf_ptr) };
        if ret < 0 {
            let mut b = bsf_ptr;
            // SAFETY: bsf_ptr is the unique owner.
            unsafe { av_bsf_free(&mut b) };
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::Bsf(ret));
        }
        // SAFETY: bsf_ptr is non-null after successful av_bsf_init.
        let bsf_ctx = unsafe { NonNull::new_unchecked(bsf_ptr) };

        // 7a. Allocate cpu_frame (NV12, software side).
        // SAFETY: av_frame_alloc allocates a zeroed AVFrame; always returns non-null or null on OOM.
        let cpu_ptr = unsafe {
            use rusty_ffmpeg::ffi::{av_frame_alloc, AVPixelFormat_AV_PIX_FMT_NV12};
            let f = av_frame_alloc();
            if f.is_null() {
                let mut b = bsf_ctx.as_ptr();
                av_bsf_free(&mut b);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(-1));
            }
            (*f).format = AVPixelFormat_AV_PIX_FMT_NV12;
            (*f).width = cfg.width as i32;
            (*f).height = cfg.height as i32;
            // SAFETY: frame fields are set; 32-byte alignment is safe for NV12.
            let ret = av_frame_get_buffer(f, 32);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut b = bsf_ctx.as_ptr();
                av_bsf_free(&mut b);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(ret));
            }
            f
        };
        // SAFETY: cpu_ptr is non-null after successful av_frame_get_buffer.
        let cpu_frame = unsafe { NonNull::new_unchecked(cpu_ptr) };

        // 7b. Allocate hw_frame (VAAPI surface from pool).
        // SAFETY: frames.raw() is the valid frames buffer; hw_ptr is the out-param address.
        let hw_ptr = unsafe {
            use rusty_ffmpeg::ffi::av_frame_alloc;
            let f = av_frame_alloc();
            if f.is_null() {
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut b = bsf_ctx.as_ptr();
                av_bsf_free(&mut b);
                let mut p = codec_ctx.as_ptr();
                avcodec_free_context(&mut p);
                return Err(FfmpegError::OpenCodec(-1));
            }
            let ret = av_hwframe_get_buffer(frames.raw(), f, 0);
            if ret < 0 {
                av_frame_free(&mut { f });
                let mut c = cpu_frame.as_ptr();
                av_frame_free(&mut c);
                let mut b = bsf_ctx.as_ptr();
                av_bsf_free(&mut b);
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

        // 8. Emit encoder_ready event.
        tracing::info!(
            target: "video.pipeline",
            event = "encoder_ready",
            backend = "ffmpeg-vaapi-hevc",
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
            bsf_ctx,
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

        // 2. Upload: CPU → GPU. This is the sole CPU→GPU transfer site in this crate.
        // SAFETY: hw and cpu are valid non-null AVFrames; 0 flags is required by the API.
        let ret = unsafe { hw_upload(hw, cpu, 0) };
        if ret < 0 {
            return Err(FfmpegError::Transfer(ret).into());
        }

        // 3. Set picture type for IDR forcing.
        // SAFETY: hw is a valid AVFrame owned by self.
        unsafe {
            if force_idr {
                (*hw).pict_type = AVPictureType_AV_PICTURE_TYPE_I;
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

        // 6. Receive encoded packet.
        // SAFETY: av_packet_alloc returns zeroed packet or null.
        let pkt_in = unsafe { av_packet_alloc() };
        if pkt_in.is_null() {
            return Err(EncodeError::Backend("av_packet_alloc failed".into()));
        }
        // SAFETY: ctx is open; pkt_in is freshly allocated.
        let recv_ret = unsafe { avcodec_receive_packet(ctx, pkt_in) };
        if recv_ret < 0 {
            // SAFETY: pkt_in is still the unique owner.
            unsafe {
                av_packet_unref(pkt_in);
                av_packet_free(&mut { pkt_in });
            }
            return Err(FfmpegError::Receive(recv_ret).into());
        }

        // 7. BSF: hevc_mp4toannexb. EAGAIN after send is a logic error.
        let bsf = self.bsf_ctx.as_ptr();
        // SAFETY: bsf is valid and open; pkt_in ownership transfers to the BSF.
        let bsf_send_ret = unsafe { av_bsf_send_packet(bsf, pkt_in) };
        if bsf_send_ret == AVERROR_EAGAIN {
            // SAFETY: pkt_in ownership is with BSF now; just free our handle.
            unsafe {
                av_packet_unref(pkt_in);
                av_packet_free(&mut { pkt_in });
            }
            return Err(FfmpegError::Bsf(bsf_send_ret).into());
        }
        if bsf_send_ret < 0 {
            // SAFETY: pkt_in ownership is with BSF now; just free our handle.
            unsafe {
                av_packet_unref(pkt_in);
                av_packet_free(&mut { pkt_in });
            }
            return Err(FfmpegError::Bsf(bsf_send_ret).into());
        }
        // Free our pkt_in handle (BSF owns the data now).
        // SAFETY: pkt_in data has been transferred; we only free the shell.
        unsafe {
            av_packet_unref(pkt_in);
            av_packet_free(&mut { pkt_in });
        }

        // Collect all BSF output packets (theoretically >1 on IDR param-set re-injection).
        let mut nal_bytes: Vec<u8> = Vec::new();
        let mut is_keyframe = false;
        loop {
            // SAFETY: av_packet_alloc returns zeroed packet or null.
            let pkt_out = unsafe { av_packet_alloc() };
            if pkt_out.is_null() {
                return Err(EncodeError::Backend(
                    "av_packet_alloc failed (bsf output)".into(),
                ));
            }
            // SAFETY: bsf is valid; pkt_out is freshly allocated.
            let bsf_recv_ret = unsafe { av_bsf_receive_packet(bsf, pkt_out) };
            if bsf_recv_ret == AVERROR_EAGAIN {
                // SAFETY: pkt_out is the unique owner.
                unsafe {
                    av_packet_unref(pkt_out);
                    av_packet_free(&mut { pkt_out });
                }
                break;
            }
            if bsf_recv_ret < 0 {
                // SAFETY: pkt_out is the unique owner.
                unsafe {
                    av_packet_unref(pkt_out);
                    av_packet_free(&mut { pkt_out });
                }
                return Err(FfmpegError::Bsf(bsf_recv_ret).into());
            }

            // 8+9. Copy bytes and detect keyframe.
            // SAFETY: pkt_out.data/size are valid after successful av_bsf_receive_packet.
            unsafe {
                let slice = std::slice::from_raw_parts((*pkt_out).data, (*pkt_out).size as usize);
                nal_bytes.extend_from_slice(slice);
                if ((*pkt_out).flags & AV_PKT_FLAG_KEY as i32) != 0 {
                    is_keyframe = true;
                }
            }

            // 10. Cleanup output packet.
            // SAFETY: pkt_out is the unique owner; unref before free.
            unsafe {
                av_packet_unref(pkt_out);
                av_packet_free(&mut { pkt_out });
            }
        }

        // 11. First-frame log.
        if !self.first_frame_logged.swap(true, Ordering::SeqCst) {
            tracing::info!(
                target: "video.pipeline",
                event = "first_frame_emitted",
                backend = "ffmpeg-vaapi-hevc",
                codec = "h265",
                zero_copy = true,
                profile = "main",
                bitdepth = 8,
                gop = self.tunables.gop_size,
                "first encoded frame delivered"
            );
        }

        // 12. Advance seq counter and return.
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
        "ffmpeg-vaapi-hevc"
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

impl Drop for HevcVaapiFfmpegEncoder {
    fn drop(&mut self) {
        // Reverse-creation order: bsf → hw_frame → cpu_frame → codec_ctx → frames → device.
        let mut bsf = self.bsf_ctx.as_ptr();
        // SAFETY: bsf_ctx is the unique owner of the BSF context.
        unsafe { av_bsf_free(&mut bsf) };

        let mut hw = self.hw_frame.as_ptr();
        // SAFETY: hw_frame is the unique owner of the VAAPI surface ref.
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

// Integration tests — require real Intel iGPU with VAAPI HEVC encode support.
#[cfg(test)]
mod tests {
    use super::*;

    fn default_config(w: u32, h: u32, gop: u32) -> HevcVaapiFfmpegEncoderConfig {
        HevcVaapiFfmpegEncoderConfig {
            width: w,
            height: h,
            fps: 30,
            initial_bitrate_bps: 4_000_000,
            gop_size: gop,
            render_node: None,
        }
    }

    #[test]
    #[ignore = "requires Intel iGPU VAAPI HEVC encode"]
    fn small_frame_emits_idr() {
        let cfg = default_config(320, 240, 30);
        let mut enc = HevcVaapiFfmpegEncoder::new(cfg).expect("encoder created");
        let frame = I420Frame::new_packed(320, 240).expect("frame");
        let pkt = enc.encode(&frame, true, 0).expect("encoded");
        assert!(pkt.is_keyframe);
        assert!(pkt.nal_bytes.starts_with(&[0, 0, 0, 1]));
        // Parse first 3 NAL types: VPS=32, SPS=33, PPS=34.
        let mut pos = 0;
        let mut nal_types = Vec::new();
        let b = &pkt.nal_bytes;
        while pos + 4 < b.len() && nal_types.len() < 4 {
            if b[pos] == 0 && b[pos + 1] == 0 && b[pos + 2] == 0 && b[pos + 3] == 1 {
                if pos + 4 < b.len() {
                    let nal_type = (b[pos + 4] >> 1) & 0x3F;
                    nal_types.push(nal_type);
                }
                pos += 4;
            } else {
                pos += 1;
            }
        }
        assert_eq!(nal_types.get(0), Some(&32u8), "expected VPS");
        assert_eq!(nal_types.get(1), Some(&33u8), "expected SPS");
        assert_eq!(nal_types.get(2), Some(&34u8), "expected PPS");
        assert!(
            nal_types
                .get(3)
                .map(|&t| t == 19 || t == 20)
                .unwrap_or(false),
            "expected IDR slice"
        );
    }

    #[test]
    #[ignore = "requires Intel iGPU VAAPI HEVC encode"]
    fn idr_cadence_respects_gop() {
        let cfg = default_config(320, 240, 30);
        let mut enc = HevcVaapiFfmpegEncoder::new(cfg).expect("encoder created");
        let frame = I420Frame::new_packed(320, 240).expect("frame");
        let mut key_count = 0u32;
        for i in 0u64..120 {
            let pkt = enc.encode(&frame, false, i * 33_333).expect("encoded");
            if pkt.is_keyframe {
                key_count += 1;
            }
        }
        assert!(
            (3..=5).contains(&key_count),
            "expected 4±1 IDR frames, got {key_count}"
        );
    }

    #[test]
    #[ignore = "requires Intel iGPU VAAPI HEVC encode"]
    fn set_target_bitrate_takes_effect() {
        let cfg = default_config(320, 240, 30);
        let mut enc = HevcVaapiFfmpegEncoder::new(cfg).expect("encoder created");
        let frame = I420Frame::new_packed(320, 240).expect("frame");

        let mut low_bytes = 0usize;
        for i in 0u64..60 {
            let pkt = enc.encode(&frame, i == 0, i * 33_333).expect("encoded");
            low_bytes += pkt.nal_bytes.len();
        }
        enc.set_target_bitrate(12_000_000).expect("bitrate set");
        let mut high_bytes = 0usize;
        for i in 60u64..120 {
            let pkt = enc.encode(&frame, false, i * 33_333).expect("encoded");
            high_bytes += pkt.nal_bytes.len();
        }
        assert!(
            high_bytes >= low_bytes * 2,
            "expected high-bitrate bytes ({high_bytes}) >= 2× low ({low_bytes})"
        );
    }
}
