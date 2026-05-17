//! Shared bookkeeping helpers for the two NVENC encoder backends
//! (`hevc_nvenc_encoder.rs` + `hevc_nvenc_npp_encoder.rs`).
//!
//! Extracted per P2.5 plan §3 (R6 synthesis) so both encoders cannot drift
//! on libavcodec init / drain / PTS / first-frame logging by construction.
//! Only the per-frame input path differs between the two encoders (CPU NV12
//! upload via `av_hwframe_transfer_data` vs GPU BGRA→NV12 via NPP +
//! `cuMemcpy2DAsync`).

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use prdt_media_core::EncodedPacket;
use rusty_ffmpeg::ffi::{
    av_packet_alloc, av_packet_free, av_packet_rescale_ts, av_packet_unref, avcodec_alloc_context3,
    avcodec_find_encoder_by_name, avcodec_free_context, avcodec_open2, avcodec_receive_packet,
    AVCodecContext, AVPacket, AVRational, AV_PKT_FLAG_KEY,
};

use crate::error::FfmpegError;
use crate::options::{apply_low_latency_hevc_nvenc, build_priv_data_dict_nvenc, EncoderTunables};
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
use crate::options::{apply_low_latency_hevc_nvenc_main10, build_priv_data_dict_nvenc_main10};

/// AVERROR(EAGAIN) = -(EAGAIN) = -11 on Linux.
pub(crate) const AVERROR_EAGAIN: i32 = -11;
/// AVERROR_EOF = FFERRTAG('E','O','F',' ') = -0x5fb9b0bb on all FFmpeg versions.
pub(crate) const AVERROR_EOF: i32 = -0x5fb9b0bb;

/// Open and configure an NVENC codec context for HEVC (preset=p1, tune=ull,
/// rc=cbr, zerolatency=1, bf=0, forced-idr=1; private-data dict via
/// `build_priv_data_dict_nvenc(gop_size)`).
///
/// OWNERSHIP. Caller passes a unique `AVBufferRef` for `hw_frames_ctx`
/// (typically obtained via `av_buffer_ref(frames.raw())`). On success the ref
/// is consumed by `avcodec_open2` (libavcodec takes ownership for the
/// lifetime of the returned context). On error the ref is **also** consumed
/// (we free the codec context, which releases its `hw_frames_ctx`). Caller
/// MUST NOT free the input ref in either case.
///
/// Returns a non-null, opened `*mut AVCodecContext` whose lifetime the caller
/// owns until `avcodec_free_context`.
pub(crate) fn open_nvenc_codec_context(
    tunables: &EncoderTunables,
    hw_frames_ctx: NonNull<rusty_ffmpeg::ffi::AVBufferRef>,
) -> Result<NonNull<AVCodecContext>, FfmpegError> {
    // 1. Probe encoder.
    // SAFETY: string literal is a valid nul-terminated C string.
    let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_nvenc".as_ptr()) };
    if codec.is_null() {
        // Caller passed in a ref it owns; we have not consumed it yet, but the
        // contract says we always consume on error. Free it here.
        let mut p = hw_frames_ctx.as_ptr();
        // SAFETY: hw_frames_ctx is a unique AVBufferRef ref the caller owns.
        unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut p) };
        return Err(FfmpegError::EncoderNotFound("hevc_nvenc"));
    }

    // 2. Allocate context + apply tunables.
    // SAFETY: codec is a valid non-null AVCodec pointer.
    let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
    if codec_ctx_ptr.is_null() {
        let mut p = hw_frames_ctx.as_ptr();
        // SAFETY: ditto contract — consume on error.
        unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut p) };
        return Err(FfmpegError::OpenCodec(-1));
    }
    // SAFETY: codec_ctx_ptr is a freshly allocated, unopened AVCodecContext.
    unsafe { apply_low_latency_hevc_nvenc(codec_ctx_ptr, tunables) };

    // 3. Attach hw_frames_ctx — avcodec_open2 will take ownership of the ref.
    // SAFETY: codec_ctx_ptr is valid and not yet opened.
    unsafe { (*codec_ctx_ptr).hw_frames_ctx = hw_frames_ctx.as_ptr() };

    // 4. Open codec with priv_data_dict (avcodec_open2 consumes the dict).
    let dict = match build_priv_data_dict_nvenc(tunables.gop_size) {
        Ok(d) => d,
        Err(e) => {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; this frees its
            // hw_frames_ctx ref too.
            unsafe { avcodec_free_context(&mut p) };
            return Err(e);
        }
    };
    // SAFETY: codec_ctx_ptr, codec, and dict are all valid; avcodec_open2
    // frees dict on success or failure.
    let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, &mut dict.as_ptr()) };
    if ret < 0 {
        let mut p = codec_ctx_ptr;
        // SAFETY: codec_ctx_ptr is the unique owner.
        unsafe { avcodec_free_context(&mut p) };
        return Err(FfmpegError::OpenCodec(ret));
    }

    // SAFETY: avcodec_alloc_context3 succeeded; pointer is non-null.
    Ok(unsafe { NonNull::new_unchecked(codec_ctx_ptr) })
}

/// Open and configure an NVENC codec context for HEVC Main10 (profile=main10,
/// pix_fmt=P010LE, HDR10 color metadata; all other tunables identical to the
/// 8-bit `open_nvenc_codec_context`). Duplicates the 8-bit body — the twin
/// MUST stay byte-identical (CI guard F4.b).
///
/// OWNERSHIP contract is identical to [`open_nvenc_codec_context`].
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
pub(crate) fn open_nvenc_codec_context_main10(
    tunables: &EncoderTunables,
    hw_frames_ctx: NonNull<rusty_ffmpeg::ffi::AVBufferRef>,
) -> Result<NonNull<AVCodecContext>, FfmpegError> {
    // 1. Probe encoder.
    // SAFETY: string literal is a valid nul-terminated C string.
    let codec = unsafe { avcodec_find_encoder_by_name(c"hevc_nvenc".as_ptr()) };
    if codec.is_null() {
        let mut p = hw_frames_ctx.as_ptr();
        // SAFETY: hw_frames_ctx is a unique AVBufferRef ref the caller owns.
        unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut p) };
        return Err(FfmpegError::EncoderNotFound("hevc_nvenc"));
    }

    // 2. Allocate context + apply Main10 tunables.
    // SAFETY: codec is a valid non-null AVCodec pointer.
    let codec_ctx_ptr = unsafe { avcodec_alloc_context3(codec) };
    if codec_ctx_ptr.is_null() {
        let mut p = hw_frames_ctx.as_ptr();
        // SAFETY: ditto contract — consume on error.
        unsafe { rusty_ffmpeg::ffi::av_buffer_unref(&mut p) };
        return Err(FfmpegError::OpenCodec(-1));
    }
    // SAFETY: codec_ctx_ptr is a freshly allocated, unopened AVCodecContext.
    unsafe { apply_low_latency_hevc_nvenc_main10(codec_ctx_ptr, tunables) };

    // 3. Attach hw_frames_ctx — avcodec_open2 will take ownership of the ref.
    // SAFETY: codec_ctx_ptr is valid and not yet opened.
    unsafe { (*codec_ctx_ptr).hw_frames_ctx = hw_frames_ctx.as_ptr() };

    // 4. Open codec with priv_data_dict (avcodec_open2 consumes the dict).
    let dict = match build_priv_data_dict_nvenc_main10(tunables.gop_size) {
        Ok(d) => d,
        Err(e) => {
            let mut p = codec_ctx_ptr;
            // SAFETY: codec_ctx_ptr is the unique owner; this frees its
            // hw_frames_ctx ref too.
            unsafe { avcodec_free_context(&mut p) };
            return Err(e);
        }
    };
    // SAFETY: codec_ctx_ptr, codec, and dict are all valid; avcodec_open2
    // frees dict on success or failure.
    let ret = unsafe { avcodec_open2(codec_ctx_ptr, codec, &mut dict.as_ptr()) };
    if ret < 0 {
        let mut p = codec_ctx_ptr;
        // SAFETY: codec_ctx_ptr is the unique owner.
        unsafe { avcodec_free_context(&mut p) };
        return Err(FfmpegError::OpenCodec(ret));
    }

    // SAFETY: avcodec_alloc_context3 succeeded; pointer is non-null.
    Ok(unsafe { NonNull::new_unchecked(codec_ctx_ptr) })
}

/// Drain all packets currently pending from the encoder. Returns Vec because
/// a single `send_frame` may queue 0 (typical pre-warmup or EAGAIN), 1
/// (typical steady-state), or >1 entries (multi-packet after a flush / IDR
/// forcing). Caller appends to its own output queue.
///
/// SEMANTICS.
/// - `Ok(vec![])` on `AVERROR(EAGAIN)` → "no packet this iteration, send next
///   frame". Caller treats as a normal idle state.
/// - `Ok(vec![])` on `AVERROR_EOF` → "encoder fully drained after a
///   send_frame(NULL) flush". Caller transitions to closed state.
/// - `Err(FfmpegError::Receive(_))` on any other return code from
///   `avcodec_receive_packet`; caller should propagate (encode fault).
///
/// The Vec entries are fully-owned EncodedPackets; AVPacket allocations are
/// freed inside the loop via `av_packet_unref` + `av_packet_free`.
pub(crate) fn drain_packets_on_eagain(
    codec_ctx: NonNull<AVCodecContext>,
    ts_us: u64,
) -> Result<Vec<EncodedPacket>, FfmpegError> {
    let ctx = codec_ctx.as_ptr();
    let mut out = Vec::new();
    loop {
        // SAFETY: av_packet_alloc returns zeroed packet or null.
        let pkt = unsafe { av_packet_alloc() };
        if pkt.is_null() {
            return Err(FfmpegError::HwFrames("av_packet_alloc failed".into()));
        }
        // SAFETY: ctx is open; pkt is freshly allocated.
        let recv_ret = unsafe { avcodec_receive_packet(ctx, pkt) };
        if recv_ret == AVERROR_EAGAIN || recv_ret == AVERROR_EOF {
            // SAFETY: pkt is the unique owner (no data attached yet on EAGAIN/EOF).
            unsafe {
                av_packet_unref(pkt);
                av_packet_free(&mut { pkt });
            }
            return Ok(out);
        }
        if recv_ret < 0 {
            // SAFETY: pkt is the unique owner.
            unsafe {
                av_packet_unref(pkt);
                av_packet_free(&mut { pkt });
            }
            return Err(FfmpegError::Receive(recv_ret));
        }

        // Successfully drained a packet — copy bytes + detect keyframe.
        // SAFETY: pkt.data/size are valid after successful avcodec_receive_packet.
        let (data_ptr, size, flags) = unsafe { ((*pkt).data, (*pkt).size as usize, (*pkt).flags) };
        // SAFETY: data_ptr is valid for `size` bytes for the lifetime of pkt.
        let slice = unsafe { std::slice::from_raw_parts(data_ptr, size) };
        let nal_bytes = slice.to_vec();
        let is_keyframe = (flags & AV_PKT_FLAG_KEY as i32) != 0;
        // SAFETY: pkt is the unique owner; unref before free.
        unsafe {
            av_packet_unref(pkt);
            av_packet_free(&mut { pkt });
        }
        out.push(EncodedPacket {
            nal_bytes,
            is_keyframe,
            timestamp_us: ts_us,
        });
    }
}

/// Wrap `av_packet_rescale_ts` from `src_tb` (encoder timebase) to `dst_tb`
/// (typically µs domain). Mutates `*packet` in place.
///
/// # Safety
/// Caller guarantees `packet` is a valid AVPacket allocated via
/// `av_packet_alloc` and not concurrently accessed.
pub(crate) unsafe fn rescale_pts(packet: *mut AVPacket, src_tb: AVRational, dst_tb: AVRational) {
    // SAFETY: contract delegated to caller.
    unsafe { av_packet_rescale_ts(packet, src_tb, dst_tb) };
}

/// Emit the first-frame `tracing::info!` at the call site. Fields:
/// `seq, codec="hevc_nvenc", hw_path="cuda", convert_path={"sw"|"npp"},
///  width, height, message="first encoded frame"`.
///
/// The `convert_path` field differs between encoders:
/// - `hevc_nvenc_encoder.rs` passes `"sw"` (CPU bgra_to_i420 +
///   i420_to_nv12_into + hw_upload).
/// - `hevc_nvenc_npp_encoder.rs` passes `"npp"` (GPU NPP path).
///
/// Helper is a no-op if `first_frame_logged` was already set; uses
/// `AtomicBool::swap` so concurrent encode calls log at most once.
pub(crate) fn emit_first_frame_log(
    seq: u64,
    codec: &'static str,
    hw_path: &'static str,
    convert_path: &'static str,
    width: u32,
    height: u32,
    first_frame_logged: &AtomicBool,
) {
    if !first_frame_logged.swap(true, Ordering::SeqCst) {
        tracing::info!(
            target: "video.pipeline",
            event = "first_frame_emitted",
            seq,
            codec,
            hw_path,
            convert_path,
            width,
            height,
            "first encoded frame delivered"
        );
    }
}
