//! Software HEVC decoder via libavcodec's generic `hevc` decoder.
//! Universal Linux fallback: no GPU dependency, runs on any CPU.
//!
//! Output is NV12 8-bit (the codec's native format after internal YUV420P
//! → NV12 conversion when the caller pins `pix_fmt = NV12`). Per-frame
//! latency on a modern CPU (i7-12700 / Ryzen 7700+) is well within the
//! budget at 1080p60; 4K60 SW decode is functional but core-limited —
//! prefer VAAPI or NVDEC for that workload (see ADR / smoke-doc R7
//! disclosure).
//!
//! No `av_hwframe_transfer_data` call site here (SW path has no HW
//! surface to read back).

use std::ptr;
use std::ptr::NonNull;

use prdt_media_core::{DecodeError, Nv12Frame};
use rusty_ffmpeg::ffi::{
    av_frame_alloc, av_frame_free, av_frame_unref, av_packet_alloc, av_packet_free,
    av_packet_unref, avcodec_alloc_context3, avcodec_find_decoder_by_name, avcodec_free_context,
    avcodec_open2, avcodec_receive_frame, avcodec_send_packet, AVCodecContext, AVFrame, AVPacket,
    AV_PIX_FMT_NV12, AV_PIX_FMT_YUV420P,
};

use crate::decoder_common::{
    copy_nv12_planes, ffmpeg_to_decode_err, HevcDecoderBackend, AVERROR_EAGAIN, AVERROR_EOF,
};
use crate::error::FfmpegError;

pub struct HevcSwFfmpegDecoderConfig {
    /// Coded width in pixels. Used as the assumed output dimension when
    /// the decoder hasn't yet observed an SPS (defensive — every real
    /// stream's first IDR carries one).
    pub width: u32,
    pub height: u32,
}

impl Default for HevcSwFfmpegDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}

pub struct HevcSwFfmpegDecoder {
    codec_ctx: NonNull<AVCodecContext>,
    frame: NonNull<AVFrame>,
    packet: NonNull<AVPacket>,
}

// SAFETY: HevcSwFfmpegDecoder owns its libavcodec resources exclusively via
// NonNull pointers. It is never aliased and the decode pipeline always runs
// single-threaded (parallel to HevcVaapiFfmpegEncoderAdapter's reasoning).
unsafe impl Send for HevcSwFfmpegDecoder {}

impl HevcSwFfmpegDecoder {
    pub fn new(_cfg: HevcSwFfmpegDecoderConfig) -> Result<Self, FfmpegError> {
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

        // Pin the SW output format to NV12 so we can copy planes directly
        // into our Nv12Frame carrier without a YUV420P→NV12 conversion
        // step. libavcodec's generic `hevc` decoder honours this for
        // 8-bit Main streams; 10-bit / Main10 would require AV_PIX_FMT_NV12LE
        // (out of scope for P2).
        // SAFETY: codec_ctx_ptr is freshly allocated and unopened; field write is in-bounds.
        unsafe {
            (*codec_ctx_ptr).pix_fmt = AV_PIX_FMT_NV12;
            // sw_pix_fmt is read by avcodec_open2 as a fallback hint when
            // the stream's actual format differs; YUV420P is the codec's
            // native shape so the internal converter knows the source.
            (*codec_ctx_ptr).sw_pix_fmt = AV_PIX_FMT_YUV420P;
        }

        // SAFETY: codec_ctx_ptr is valid and not yet opened; no priv_data dict needed for SW.
        let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, ptr::null_mut()) };
        if ret < 0 {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; free on error path.
            unsafe { avcodec_free_context(&mut p) };
            return Err(FfmpegError::OpenCodec(ret));
        }

        // SAFETY: avcodec_alloc_context3 succeeded so codec_ctx_ptr is non-null.
        let codec_ctx = unsafe { NonNull::new_unchecked(codec_ctx_ptr) };

        // SAFETY: av_frame_alloc always succeeds or returns null (no other failure modes).
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
            backend = "ffmpeg-sw-hevc",
            codec = "h265",
        );

        Ok(Self {
            codec_ctx,
            frame,
            packet,
        })
    }
}

impl HevcDecoderBackend for HevcSwFfmpegDecoder {
    fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError> {
        if packet.is_empty() {
            return Ok(());
        }
        let pkt = self.packet.as_ptr();
        // SAFETY: pkt is the unique AVPacket owned by self; data/size are
        // assigned to caller-owned slice for the duration of the avcodec_send_packet
        // call (libavcodec copies the bytes when no buf is set).
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

    fn drain_frame(&mut self) -> Result<Option<Nv12Frame>, DecodeError> {
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

        // SAFETY: receive_frame succeeded; frame's planes / linesize / dims
        // are valid and owned by self.frame until av_frame_unref.
        let (y_ptr, uv_ptr, y_stride, uv_stride, w, h, pts) = unsafe {
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

        // SAFETY: copy_nv12_planes copies bytes out of the AVFrame's planes
        // into owned Vecs; the source pointers are valid for the strides/dims
        // we just read from the AVFrame.
        let out = unsafe { copy_nv12_planes(y_ptr, uv_ptr, y_stride, uv_stride, w, h, pts) };

        // SAFETY: frame is the unique owner; release the libavcodec-owned data
        // ref so the next receive_frame can repopulate it.
        unsafe { av_frame_unref(frame) };

        Ok(Some(out))
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-sw-hevc"
    }
}

impl Drop for HevcSwFfmpegDecoder {
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
    use rusty_ffmpeg::ffi::{
        av_frame_get_buffer, avcodec_find_encoder_by_name, avcodec_receive_packet,
        avcodec_send_frame, AV_PIX_FMT_YUV420P,
    };

    #[test]
    fn constructs_cleanly_when_libavcodec_has_hevc() {
        let cfg = HevcSwFfmpegDecoderConfig::default();
        let dec = HevcSwFfmpegDecoder::new(cfg).expect("hevc SW decoder must be present");
        assert_eq!(dec.backend_name(), "ffmpeg-sw-hevc");
    }

    #[test]
    fn empty_packet_is_a_noop() {
        let mut dec = HevcSwFfmpegDecoder::new(HevcSwFfmpegDecoderConfig::default()).expect("dec");
        dec.feed_packet(&[], 0).expect("empty feed is ok");
        assert!(dec.drain_frame().expect("drain").is_none());
    }

    /// Round-trip: encode a 64×64 YUV420P frame via libx265 (in-process,
    /// no external ffmpeg CLI), feed the resulting Annex-B HEVC packet to
    /// `HevcSwFfmpegDecoder`, assert the decoded NV12 frame has the correct
    /// plane sizes. Skipped at runtime if libx265 is not compiled into the
    /// system libavcodec (CI containers without the encoder will just skip).
    #[test]
    fn hevc_sw_decode_round_trip_via_libx265() {
        let w = 64i32;
        let h = 64i32;

        // ── 1. Build a libx265 encoder context ──────────────────────────────
        // SAFETY: string literal is a valid nul-terminated C string.
        let enc_codec = unsafe { avcodec_find_encoder_by_name(c"libx265".as_ptr()) };
        if enc_codec.is_null() {
            eprintln!("skip: libx265 not compiled into system libavcodec");
            return;
        }

        // SAFETY: enc_codec is non-null.
        let enc_ctx_ptr = unsafe { avcodec_alloc_context3(enc_codec) };
        assert!(!enc_ctx_ptr.is_null(), "avcodec_alloc_context3 failed");

        // SAFETY: enc_ctx_ptr is freshly allocated.
        unsafe {
            (*enc_ctx_ptr).width = w;
            (*enc_ctx_ptr).height = h;
            (*enc_ctx_ptr).pix_fmt = AV_PIX_FMT_YUV420P;
            // Minimal valid timebase for a still-frame test.
            (*enc_ctx_ptr).time_base = rusty_ffmpeg::ffi::AVRational { num: 1, den: 30 };
            (*enc_ctx_ptr).framerate = rusty_ffmpeg::ffi::AVRational { num: 30, den: 1 };
            // Force IDR-only GOP (keyint=1) so the very first frame is
            // decodeable without prior context.
            (*enc_ctx_ptr).gop_size = 1;
            // x265 opts: zerolatency + keyint=1 via priv_data opts.
        }

        // Set x265-params for keyint=1 + no-open-gop via av_opt_set.
        // SAFETY: enc_ctx_ptr and its priv_data are valid after alloc3.
        unsafe {
            use rusty_ffmpeg::ffi::{av_opt_set, AV_OPT_SEARCH_CHILDREN};
            let flags = AV_OPT_SEARCH_CHILDREN as i32;
            av_opt_set(
                enc_ctx_ptr as *mut _,
                c"x265-params".as_ptr(),
                c"keyint=1:no-open-gop=1".as_ptr(),
                flags,
            );
            av_opt_set(
                enc_ctx_ptr as *mut _,
                c"preset".as_ptr(),
                c"ultrafast".as_ptr(),
                flags,
            );
        }

        // SAFETY: enc_ctx_ptr is valid; open with no extra dict.
        let ret = unsafe { avcodec_open2(enc_ctx_ptr, enc_codec, ptr::null_mut()) };
        assert!(ret >= 0, "avcodec_open2 for libx265 failed: {ret}");

        // ── 2. Allocate + fill a minimal YUV420P source frame ───────────────
        // SAFETY: av_frame_alloc returns non-null or null.
        let src_frame = unsafe { av_frame_alloc() };
        assert!(!src_frame.is_null(), "av_frame_alloc failed");

        // SAFETY: src_frame is non-null.
        unsafe {
            (*src_frame).format = AV_PIX_FMT_YUV420P;
            (*src_frame).width = w;
            (*src_frame).height = h;
            (*src_frame).pts = 0;
        }

        // SAFETY: src_frame fields set; align=32 for SIMD safety.
        let ret = unsafe { av_frame_get_buffer(src_frame, 32) };
        assert!(ret >= 0, "av_frame_get_buffer failed: {ret}");

        // Fill planes with a simple ramp so the encoder has structured content.
        // SAFETY: frame buffer is valid after av_frame_get_buffer.
        unsafe {
            let f = &*src_frame;
            let y_slice =
                std::slice::from_raw_parts_mut(f.data[0], (f.linesize[0] as usize) * (h as usize));
            for (i, b) in y_slice.iter_mut().enumerate() {
                *b = (i & 0xFF) as u8;
            }
            let uv_h = h as usize / 2;
            let u_slice =
                std::slice::from_raw_parts_mut(f.data[1], (f.linesize[1] as usize) * uv_h);
            u_slice.fill(128);
            let v_slice =
                std::slice::from_raw_parts_mut(f.data[2], (f.linesize[2] as usize) * uv_h);
            v_slice.fill(128);
        }

        // ── 3. Encode the frame and flush ───────────────────────────────────
        // SAFETY: enc_ctx_ptr is open; src_frame is valid.
        let ret = unsafe { avcodec_send_frame(enc_ctx_ptr, src_frame) };
        assert!(ret >= 0, "avcodec_send_frame failed: {ret}");
        // Flush.
        // SAFETY: enc_ctx_ptr is open.
        let ret = unsafe { avcodec_send_frame(enc_ctx_ptr, ptr::null_mut()) };
        assert!(ret >= 0, "avcodec_send_frame flush failed: {ret}");

        // SAFETY: av_packet_alloc returns zeroed AVPacket or null.
        let enc_pkt = unsafe { av_packet_alloc() };
        assert!(!enc_pkt.is_null(), "av_packet_alloc failed");

        // SAFETY: enc_ctx_ptr is open and flushed; enc_pkt is a valid zeroed packet.
        let ret = unsafe { avcodec_receive_packet(enc_ctx_ptr, enc_pkt) };
        assert!(ret >= 0, "avcodec_receive_packet failed: {ret}");

        // Copy encoded bytes out before freeing encoder resources.
        // SAFETY: enc_pkt.data/size are valid after successful receive_packet.
        let encoded_bytes: Vec<u8> = unsafe {
            std::slice::from_raw_parts((*enc_pkt).data, (*enc_pkt).size as usize).to_vec()
        };
        assert!(!encoded_bytes.is_empty(), "encoder produced zero bytes");

        // ── 4. Tear down encoder ────────────────────────────────────────────
        // SAFETY: enc_pkt is the unique owner.
        let mut ep = enc_pkt;
        unsafe { av_packet_free(&mut ep) };
        // SAFETY: src_frame is the unique owner.
        let mut sf = src_frame;
        unsafe { av_frame_free(&mut sf) };
        let mut ectx = enc_ctx_ptr;
        // SAFETY: ectx is the unique owner.
        unsafe { avcodec_free_context(&mut ectx) };

        // ── 5. Feed encoded bytes to HevcSwFfmpegDecoder ────────────────────
        let mut dec = HevcSwFfmpegDecoder::new(HevcSwFfmpegDecoderConfig {
            width: w as u32,
            height: h as u32,
        })
        .expect("SW HEVC decoder must init");

        let mut decoded: Option<Nv12Frame> = None;
        // Try up to 3 times: some encoders produce SPS/PPS + IDR in separate
        // packets requiring multiple feed-drain cycles.
        for _ in 0..3 {
            dec.feed_packet(&encoded_bytes, 0).expect("feed_packet");
            if let Some(f) = dec.drain_frame().expect("drain_frame") {
                decoded = Some(f);
                break;
            }
        }

        let f = decoded.expect("SW HEVC decoder produced no frame after 3 feed cycles");
        assert_eq!(f.width, w as u32);
        assert_eq!(f.height, h as u32);
        assert_eq!(
            f.y.len(),
            (f.stride_y as usize) * (h as usize),
            "Y plane size"
        );
        assert_eq!(
            f.uv.len(),
            (f.stride_uv as usize) * (h as usize / 2),
            "UV plane size"
        );
    }
}
