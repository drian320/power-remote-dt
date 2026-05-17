//! VAAPI HEVC Main10 decoder. Sibling of `hevc_vaapi_decoder.rs` (8-bit).
//! Key differences:
//!   - Output pixel format: `AV_PIX_FMT_P010LE` (10-bit 4:2:0).
//!   - `get_format` callback returns `AV_PIX_FMT_VAAPI` (same as 8-bit).
//!   - After `hw_download`, the CPU-side frame carries P010LE samples.
//!   - Emits `Nv12Frame16` via `HevcDecoderBackend10`.
//!   - HDR10 SEI sidecar extracted from the SW frame after download.
//!   - Profile accepted: `AV_PROFILE_HEVC_MAIN_10` (=2); MAIN8 gracefully handled.
//!
//! One `av_hwframe_transfer_data as hw_download` call per decoded frame
//! (CI grep guard; same as 8-bit VAAPI decoder).

use std::path::PathBuf;
use std::ptr;
use std::ptr::NonNull;

use prdt_media_core::{DecodeError, Nv12Frame16};
use rusty_ffmpeg::ffi::{
    av_buffer_ref, av_frame_alloc, av_frame_free, av_frame_unref,
    av_hwframe_transfer_data as hw_download, av_packet_alloc, av_packet_free, av_packet_unref,
    avcodec_alloc_context3, avcodec_find_decoder_by_name, avcodec_free_context, avcodec_open2,
    avcodec_receive_frame, avcodec_send_packet, AVCodecContext, AVFrame, AVPacket, AVPixelFormat,
    AV_PIX_FMT_NONE, AV_PIX_FMT_VAAPI,
};

use crate::decoder_common::{
    copy_p010_planes, extract_hdr10_sidecar, ffmpeg_to_decode_err, HevcDecoderBackend10,
    AVERROR_EAGAIN, AVERROR_EOF,
};
use crate::error::FfmpegError;
use crate::hwdevice::VaapiHwDevice;

pub struct HevcVaapiMain10FfmpegDecoderConfig {
    pub width: u32,
    pub height: u32,
    pub render_node: Option<PathBuf>,
}

impl Default for HevcVaapiMain10FfmpegDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            render_node: None,
        }
    }
}

pub struct HevcVaapiMain10FfmpegDecoder {
    #[allow(dead_code)]
    device: VaapiHwDevice,
    codec_ctx: NonNull<AVCodecContext>,
    hw_frame: NonNull<AVFrame>,
    sw_frame: NonNull<AVFrame>,
    packet: NonNull<AVPacket>,
}

// SAFETY: HevcVaapiMain10FfmpegDecoder owns its libavcodec + VAAPI resources
// exclusively via NonNull pointers; never aliased; decode pipeline runs
// single-threaded.
unsafe impl Send for HevcVaapiMain10FfmpegDecoder {}

/// Pixel-format negotiation callback. Returns `AV_PIX_FMT_VAAPI` so the codec
/// routes decoding through VAAPI for both Main and Main10 profiles.
///
/// # Safety
/// Called by libavcodec from the decode thread. `fmt` is a NUL-terminated
/// array of AVPixelFormat values.
unsafe extern "C" fn get_vaapi_format_main10(
    _avctx: *mut AVCodecContext,
    fmt: *const AVPixelFormat,
) -> AVPixelFormat {
    // SAFETY: libavcodec guarantees `fmt` is non-null and terminated by AV_PIX_FMT_NONE.
    let mut p = fmt;
    loop {
        // SAFETY: dereference inside the NONE-terminated array.
        let v = unsafe { *p };
        if v == AV_PIX_FMT_NONE {
            return AV_PIX_FMT_NONE;
        }
        if v == AV_PIX_FMT_VAAPI {
            return AV_PIX_FMT_VAAPI;
        }
        // SAFETY: advance to the next entry in the NONE-terminated list.
        p = unsafe { p.add(1) };
    }
}

impl HevcVaapiMain10FfmpegDecoder {
    pub fn new(cfg: HevcVaapiMain10FfmpegDecoderConfig) -> Result<Self, FfmpegError> {
        // SAFETY: string literal is a valid nul-terminated C string.
        let codec = unsafe { avcodec_find_decoder_by_name(c"hevc".as_ptr()) };
        if codec.is_null() {
            return Err(FfmpegError::EncoderNotFound("hevc"));
        }

        let device = VaapiHwDevice::open(cfg.render_node.as_deref())?;

        // SAFETY: codec is non-null from avcodec_find_decoder_by_name.
        let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
        if codec_ctx_ptr.is_null() {
            return Err(FfmpegError::OpenCodec(-1));
        }

        // Attach VAAPI device ref and install the format-negotiation callback.
        // SAFETY: device.raw() is a valid AVBufferRef owned by device.
        let dev_ref = unsafe { av_buffer_ref(device.raw()) };
        if dev_ref.is_null() {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; freeing on error path.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::HwDevice("av_buffer_ref returned null".into()));
        }
        // SAFETY: codec_ctx_ptr is freshly allocated and unopened; install ctx + callback.
        unsafe {
            (*codec_ctx_ptr).hw_device_ctx = dev_ref;
            (*codec_ctx_ptr).get_format = Some(get_vaapi_format_main10);
            (*codec_ctx_ptr).width = cfg.width as i32;
            (*codec_ctx_ptr).height = cfg.height as i32;
        }

        // SAFETY: codec_ctx_ptr is valid; no priv_data dict needed for VAAPI decode.
        let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, ptr::null_mut()) };
        if ret < 0 {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(ret));
        }

        // SAFETY: avcodec_alloc_context3 succeeded.
        let codec_ctx = unsafe { NonNull::new_unchecked(codec_ctx_ptr) };

        // SAFETY: av_frame_alloc returns non-null or null on OOM.
        let hw_frame_ptr = unsafe { av_frame_alloc() };
        if hw_frame_ptr.is_null() {
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(-1));
        }
        // SAFETY: hw_frame_ptr is non-null.
        let hw_frame = unsafe { NonNull::new_unchecked(hw_frame_ptr) };

        // SAFETY: av_frame_alloc returns non-null or null on OOM.
        let sw_frame_ptr = unsafe { av_frame_alloc() };
        if sw_frame_ptr.is_null() {
            let mut hp = hw_frame.as_ptr();
            // SAFETY: hw_frame is the unique owner.
            unsafe { av_frame_free(&mut hp) };
            let mut p = codec_ctx.as_ptr();
            // SAFETY: codec_ctx is the unique owner.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(-1));
        }
        // SAFETY: sw_frame_ptr is non-null.
        let sw_frame = unsafe { NonNull::new_unchecked(sw_frame_ptr) };

        // SAFETY: av_packet_alloc returns zeroed AVPacket or null.
        let packet_ptr = unsafe { av_packet_alloc() };
        if packet_ptr.is_null() {
            let mut sp = sw_frame.as_ptr();
            // SAFETY: sw_frame is the unique owner.
            unsafe { av_frame_free(&mut sp) };
            let mut hp = hw_frame.as_ptr();
            // SAFETY: hw_frame is the unique owner.
            unsafe { av_frame_free(&mut hp) };
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
            backend = "ffmpeg-vaapi-hevc-main10",
            codec = "h265",
            profile = "main10",
            bitdepth = 10,
        );

        Ok(Self {
            device,
            codec_ctx,
            hw_frame,
            sw_frame,
            packet,
        })
    }
}

impl HevcDecoderBackend10 for HevcVaapiMain10FfmpegDecoder {
    fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        if packet.is_empty() {
            return Ok(());
        }
        let pkt = self.packet.as_ptr();
        // SAFETY: pkt is the unique AVPacket owned by self; data/size point at the
        // caller-owned slice for the duration of avcodec_send_packet (libavcodec
        // copies bytes when no buf is set on the packet).
        unsafe {
            (*pkt).data = packet.as_ptr() as *mut u8;
            (*pkt).size = packet.len() as i32;
            (*pkt).pts = pts_us as i64;
            (*pkt).dts = pts_us as i64;
        }
        let ctx = self.codec_ctx.as_ptr();
        // SAFETY: ctx is a valid open AVCodecContext; pkt is valid.
        let ret = unsafe { avcodec_send_packet(ctx, pkt) };
        // SAFETY: pkt is the unique owner; clear out our borrowed-slice pointer.
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
        let hw = self.hw_frame.as_ptr();
        let sw = self.sw_frame.as_ptr();

        // SAFETY: ctx is open; hw is a valid AVFrame.
        let ret = unsafe { avcodec_receive_frame(ctx, hw) };
        if ret == AVERROR_EAGAIN || ret == AVERROR_EOF {
            return Ok(None);
        }
        if ret < 0 {
            return Err(ffmpeg_to_decode_err(FfmpegError::Receive(ret)));
        }

        // HW → CPU readback. This is the per-file single hw_download call site.
        // SAFETY: hw is a valid VAAPI surface; sw is an empty AVFrame; 0 flags is the API contract.
        let xfer = unsafe { hw_download(sw, hw, 0) };
        if xfer < 0 {
            // SAFETY: hw is the unique owner; release the frame ref.
            unsafe { av_frame_unref(hw) };
            return Err(ffmpeg_to_decode_err(FfmpegError::Transfer(xfer)));
        }

        // Extract HDR10 sidecar from the SW frame (side-data is propagated by hw_download).
        // SAFETY: sw is a valid AVFrame after successful hw_download.
        let hdr10 = unsafe { extract_hdr10_sidecar(sw) };

        // SAFETY: hw_download populated sw with CPU-side P010LE planes.
        let (y_ptr, uv_ptr, y_stride_bytes, uv_stride_bytes, w, h, pts) = unsafe {
            let f = &*sw;
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

        // SAFETY: copy_p010_planes copies bytes out of sw's P010LE planes into owned Vecs.
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

        // SAFETY: hw and sw are the unique owners.
        unsafe {
            av_frame_unref(sw);
            av_frame_unref(hw);
        }

        Ok(Some(out))
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-vaapi-hevc-main10"
    }
}

impl Drop for HevcVaapiMain10FfmpegDecoder {
    fn drop(&mut self) {
        let mut pkt = self.packet.as_ptr();
        // SAFETY: packet is the unique owner.
        unsafe { av_packet_free(&mut pkt) };
        let mut sp = self.sw_frame.as_ptr();
        // SAFETY: sw_frame is the unique owner.
        unsafe { av_frame_free(&mut sp) };
        let mut hp = self.hw_frame.as_ptr();
        // SAFETY: hw_frame is the unique owner.
        unsafe { av_frame_free(&mut hp) };
        let mut ctx = self.codec_ctx.as_ptr();
        // SAFETY: codec_ctx is the unique owner; hw_device_ctx freed on avcodec_free_context.
        unsafe { avcodec_free_context(&mut ctx) };
        // device drops via its own Drop impl (av_buffer_unref).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Intel iGPU / AMD APU with VAAPI HEVC Main10 decode"]
    fn constructs_with_real_vaapi_main10() {
        let cfg = HevcVaapiMain10FfmpegDecoderConfig::default();
        let dec = HevcVaapiMain10FfmpegDecoder::new(cfg).expect("decoder created");
        assert_eq!(dec.backend_name(), "ffmpeg-vaapi-hevc-main10");
    }
}
