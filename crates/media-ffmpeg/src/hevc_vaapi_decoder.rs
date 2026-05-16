//! VAAPI HEVC decoder via libavcodec's generic `hevc` decoder driven through
//! `AV_HWDEVICE_TYPE_VAAPI`. For Intel iGPUs (Skylake+) and modern AMD APUs.
//!
//! Unlike the encoder side which uses `avcodec_find_encoder_by_name("hevc_vaapi")`,
//! the decode side uses the generic `hevc` codec entry plus a `get_format`
//! callback that returns `AV_PIX_FMT_VAAPI` — that's how libavcodec
//! distinguishes HW-backed decoding from SW for the same codec.
//!
//! Output is NV12 8-bit. The HW decoder operates on a `hw_frames_ctx` pool
//! (allocated automatically when `hw_device_ctx` is set), and per frame we
//! issue exactly one `av_hwframe_transfer_data as hw_download` call to read
//! the decoded surface back into CPU memory (P2 contract; GPU-to-GPU zero
//! copy lands in P2.5).

use std::path::PathBuf;
use std::ptr;
use std::ptr::NonNull;

use prdt_media_core::{DecodeError, Nv12Frame};
use rusty_ffmpeg::ffi::{
    av_buffer_ref, av_frame_alloc, av_frame_free, av_frame_unref,
    av_hwframe_transfer_data as hw_download, av_packet_alloc, av_packet_free, av_packet_unref,
    avcodec_alloc_context3, avcodec_find_decoder_by_name, avcodec_free_context, avcodec_open2,
    avcodec_receive_frame, avcodec_send_packet, AVCodecContext, AVFrame, AVPacket, AVPixelFormat,
    AV_PIX_FMT_NONE, AV_PIX_FMT_VAAPI,
};

use crate::decoder_common::{
    copy_nv12_planes, ffmpeg_to_decode_err, HevcDecoderBackend, AVERROR_EAGAIN, AVERROR_EOF,
};
use crate::error::FfmpegError;
use crate::hwdevice::VaapiHwDevice;

pub struct HevcVaapiFfmpegDecoderConfig {
    pub width: u32,
    pub height: u32,
    pub render_node: Option<PathBuf>,
}

impl Default for HevcVaapiFfmpegDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            render_node: None,
        }
    }
}

pub struct HevcVaapiFfmpegDecoder {
    #[allow(dead_code)]
    device: VaapiHwDevice,
    codec_ctx: NonNull<AVCodecContext>,
    hw_frame: NonNull<AVFrame>,
    sw_frame: NonNull<AVFrame>,
    packet: NonNull<AVPacket>,
}

// SAFETY: HevcVaapiFfmpegDecoder owns its libavcodec + VAAPI resources
// exclusively via NonNull pointers; never aliased; decode pipeline runs
// single-threaded (parallel to HevcVaapiFfmpegEncoderAdapter's reasoning).
unsafe impl Send for HevcVaapiFfmpegDecoder {}

/// Pixel-format negotiation callback. libavcodec calls this when the
/// stream parameters become known; we must return `AV_PIX_FMT_VAAPI` so
/// the codec routes decoding through VAAPI rather than falling back to
/// SW. Returning AV_PIX_FMT_NONE asks libavcodec to error out.
///
/// # Safety
/// Called by libavcodec from the decode thread. `fmt` is a NUL-terminated
/// array of AVPixelFormat values.
unsafe extern "C" fn get_vaapi_format(
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

impl HevcVaapiFfmpegDecoder {
    pub fn new(cfg: HevcVaapiFfmpegDecoderConfig) -> Result<Self, FfmpegError> {
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

        // Attach the VAAPI device ref and install the format-negotiation
        // callback. libavcodec takes ownership of the device ref via
        // av_buffer_ref, and frees it on avcodec_free_context.
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
            (*codec_ctx_ptr).get_format = Some(get_vaapi_format);
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
            backend = "ffmpeg-vaapi-hevc",
            codec = "h265",
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

impl HevcDecoderBackend for HevcVaapiFfmpegDecoder {
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

    fn drain_frame(&mut self) -> Result<Option<Nv12Frame>, DecodeError> {
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
            // SAFETY: hw is the unique owner; release the frame ref so the next call can populate it.
            unsafe { av_frame_unref(hw) };
            return Err(ffmpeg_to_decode_err(FfmpegError::Transfer(xfer)));
        }

        // SAFETY: hw_download populated sw with the CPU-side NV12 planes.
        let (y_ptr, uv_ptr, y_stride, uv_stride, w, h, pts) = unsafe {
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

        // SAFETY: copy_nv12_planes copies bytes out of sw's planes into owned Vecs.
        let out = unsafe { copy_nv12_planes(y_ptr, uv_ptr, y_stride, uv_stride, w, h, pts) };

        // SAFETY: hw and sw are the unique owners.
        unsafe {
            av_frame_unref(sw);
            av_frame_unref(hw);
        }

        Ok(Some(out))
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-vaapi-hevc"
    }
}

impl Drop for HevcVaapiFfmpegDecoder {
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
        // SAFETY: codec_ctx is the unique owner; hw_device_ctx was consumed by avcodec_open2
        // (libavcodec frees it on avcodec_free_context).
        unsafe { avcodec_free_context(&mut ctx) };
        // device drops via its own Drop impl (av_buffer_unref).
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Intel iGPU / AMD APU with VAAPI HEVC decode"]
    fn constructs_with_real_vaapi() {
        let cfg = HevcVaapiFfmpegDecoderConfig::default();
        let dec = HevcVaapiFfmpegDecoder::new(cfg).expect("decoder created");
        assert_eq!(dec.backend_name(), "ffmpeg-vaapi-hevc");
    }
}
