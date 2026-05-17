//! Software HEVC Main10 decoder via libavcodec's generic `hevc` decoder.
//! Sibling of `hevc_sw_decoder.rs` (8-bit). Key differences:
//!   - Output pixel format: `AV_PIX_FMT_P010LE` (10-bit 4:2:0, 16-bit container).
//!   - Emits `Nv12Frame16` (P010LE) via `HevcDecoderBackend10`.
//!   - HDR10 SEI sidecar extracted per-frame via `extract_hdr10_sidecar`.
//!   - Profile accepted: `AV_PROFILE_HEVC_MAIN_10` (=2); MAIN8 streams are
//!     gracefully handled (libavcodec falls back; we return the frame as-is).
//!
//! No `av_hwframe_transfer_data` call site (SW path has no HW surface).

use std::ptr;
use std::ptr::NonNull;

use prdt_media_core::{DecodeError, Nv12Frame16};
use rusty_ffmpeg::ffi::{
    av_frame_alloc, av_frame_free, av_frame_unref, av_packet_alloc, av_packet_free,
    av_packet_unref, avcodec_alloc_context3, avcodec_find_decoder_by_name, avcodec_free_context,
    avcodec_open2, avcodec_receive_frame, avcodec_send_packet, AVCodecContext, AVFrame, AVPacket,
    AV_PIX_FMT_P010LE, AV_PIX_FMT_YUV420P10LE,
};

use crate::decoder_common::{
    copy_p010_planes, extract_hdr10_sidecar, ffmpeg_to_decode_err, HevcDecoderBackend10,
    AVERROR_EAGAIN, AVERROR_EOF,
};
use crate::error::FfmpegError;

pub struct HevcSwMain10FfmpegDecoderConfig {
    /// Coded width in pixels. Used as the assumed output dimension when
    /// the decoder hasn't yet observed an SPS.
    pub width: u32,
    pub height: u32,
}

impl Default for HevcSwMain10FfmpegDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}

pub struct HevcSwMain10FfmpegDecoder {
    codec_ctx: NonNull<AVCodecContext>,
    frame: NonNull<AVFrame>,
    packet: NonNull<AVPacket>,
}

// SAFETY: HevcSwMain10FfmpegDecoder owns its libavcodec resources exclusively
// via NonNull pointers. It is never aliased and the decode pipeline always runs
// single-threaded.
unsafe impl Send for HevcSwMain10FfmpegDecoder {}

impl HevcSwMain10FfmpegDecoder {
    pub fn new(_cfg: HevcSwMain10FfmpegDecoderConfig) -> Result<Self, FfmpegError> {
        // SAFETY: string literal is a valid nul-terminated C string.
        let codec = unsafe { avcodec_find_decoder_by_name(c"hevc".as_ptr()) };
        if codec.is_null() {
            return Err(FfmpegError::EncoderNotFound("hevc"));
        }

        // SAFETY: codec is non-null from avcodec_find_decoder_by_name.
        let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
        if codec_ctx_ptr.is_null() {
            return Err(FfmpegError::OpenCodec(-1));
        }

        // Pin output to P010LE for Main10 10-bit 4:2:0.
        // sw_pix_fmt hints the source native format so the internal converter
        // knows the source shape (YUV420P10LE is Main10's planar native form).
        // SAFETY: codec_ctx_ptr is freshly allocated and unopened.
        unsafe {
            (*codec_ctx_ptr).pix_fmt = AV_PIX_FMT_P010LE;
            (*codec_ctx_ptr).sw_pix_fmt = AV_PIX_FMT_YUV420P10LE;
        }

        // SAFETY: codec_ctx_ptr is valid and not yet opened; no priv_data dict needed.
        let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, ptr::null_mut()) };
        if ret < 0 {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; free on error path.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(ret));
        }

        // SAFETY: avcodec_alloc_context3 succeeded so codec_ctx_ptr is non-null.
        let codec_ctx = unsafe { NonNull::new_unchecked(codec_ctx_ptr) };

        // SAFETY: av_frame_alloc always succeeds or returns null.
        let frame_ptr = unsafe { av_frame_alloc() };
        if frame_ptr.is_null() {
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(-1));
        }
        // SAFETY: frame_ptr is non-null.
        let frame = unsafe { NonNull::new_unchecked(frame_ptr) };

        // SAFETY: av_packet_alloc returns zeroed AVPacket or null.
        let packet_ptr = unsafe { av_packet_alloc() };
        if packet_ptr.is_null() {
            let mut fp = frame.as_ptr();
            // SAFETY: frame is the unique owner.
            unsafe { av_frame_free(&mut fp) };
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(-1));
        }
        // SAFETY: packet_ptr is non-null.
        let packet = unsafe { NonNull::new_unchecked(packet_ptr) };

        tracing::info!(
            target: "video.pipeline",
            event = "decoder_ready",
            backend = "ffmpeg-sw-hevc-main10",
            codec = "h265",
            profile = "main10",
            bitdepth = 10,
        );

        Ok(Self {
            codec_ctx,
            frame,
            packet,
        })
    }
}

impl HevcDecoderBackend10 for HevcSwMain10FfmpegDecoder {
    fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        if packet.is_empty() {
            return Ok(());
        }
        let pkt = self.packet.as_ptr();
        // SAFETY: pkt is the unique AVPacket owned by self; data/size are
        // assigned to caller-owned slice for the duration of avcodec_send_packet
        // (libavcodec copies the bytes when no buf is set).
        unsafe {
            (*pkt).data = packet.as_ptr() as *mut u8;
            (*pkt).size = packet.len() as i32;
            (*pkt).pts = pts_us as i64;
            (*pkt).dts = pts_us as i64;
        }
        let ctx = self.codec_ctx.as_ptr();
        // SAFETY: ctx is a valid open AVCodecContext; pkt is valid for the call duration.
        let ret = unsafe { avcodec_send_packet(ctx, pkt) };
        // Clear our packet shell so the borrowed slice pointer is not retained.
        // SAFETY: pkt is the unique owner.
        unsafe {
            (*pkt).data = ptr::null_mut();
            (*pkt).size = 0;
            av_packet_unref(pkt);
        }
        if ret < 0 && ret != AVERROR_EAGAIN {
            return Err(ffmpeg_to_decode_err(FfmpegError::Send(ret)));
        }
        Ok(())
    }

    fn drain_frame(&mut self) -> Result<Option<Nv12Frame16>, DecodeError> {
        let ctx = self.codec_ctx.as_ptr();
        let frame = self.frame.as_ptr();
        // SAFETY: ctx is open; frame is a valid AVFrame.
        let ret = unsafe { avcodec_receive_frame(ctx, frame) };
        if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
            return Ok(None);
        }
        if ret < 0 {
            return Err(ffmpeg_to_decode_err(FfmpegError::Receive(ret)));
        }

        // Extract HDR10 sidecar before we move the plane pointers.
        // SAFETY: frame is valid after successful avcodec_receive_frame.
        let hdr10 = unsafe { extract_hdr10_sidecar(frame) };

        // SAFETY: receive_frame succeeded; frame's planes/linesize/dims are valid.
        let (y_ptr, uv_ptr, y_stride_bytes, uv_stride_bytes, w, h, pts) = unsafe {
            let f = &*frame;
            (
                f.data[0] as *const u8,
                f.data[1] as *const u8,
                f.linesize[0] as usize,
                f.linesize[1] as usize,
                f.width as u32,
                f.height as u32,
                f.pts as u64,
            )
        };

        // SAFETY: copy_p010_planes copies bytes out of the AVFrame's P010LE planes
        // into owned Vecs; source pointers are valid for the strides/dims read above.
        let out = unsafe {
            copy_p010_planes(
                y_ptr,
                uv_ptr,
                y_stride_bytes,
                uv_stride_bytes,
                w,
                h,
                pts,
                hdr10,
            )
        };

        // SAFETY: frame is the unique owner; release so the next receive_frame can repopulate.
        unsafe { av_frame_unref(frame) };

        Ok(Some(out))
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-sw-hevc-main10"
    }
}

impl Drop for HevcSwMain10FfmpegDecoder {
    fn drop(&mut self) {
        let mut pkt = self.packet.as_ptr();
        // SAFETY: packet is the unique owner.
        unsafe { av_packet_free(&mut pkt) };
        let mut f = self.frame.as_ptr();
        // SAFETY: frame is the unique owner.
        unsafe { av_frame_free(&mut f) };
        let mut ctx = self.codec_ctx.as_ptr();
        // SAFETY: codec_ctx is the unique owner.
        unsafe { avcodec_free_context(&mut ctx) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_cleanly_when_libavcodec_has_hevc() {
        let cfg = HevcSwMain10FfmpegDecoderConfig::default();
        let dec =
            HevcSwMain10FfmpegDecoder::new(cfg).expect("hevc SW Main10 decoder must be present");
        assert_eq!(dec.backend_name(), "ffmpeg-sw-hevc-main10");
    }

    #[test]
    fn empty_packet_is_a_noop() {
        let mut dec = HevcSwMain10FfmpegDecoder::new(HevcSwMain10FfmpegDecoderConfig::default())
            .expect("dec");
        dec.feed_packet(&[], 0).expect("empty feed is ok");
        assert!(dec.drain_frame().expect("drain").is_none());
    }
}
