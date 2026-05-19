//! Shared scaffolding for the three HEVC decode backends in this crate
//! (sw / vaapi / nvdec). Holds the `HevcDecoderBackend` trait every
//! backend implements and small NV12 helpers used during the per-frame
//! plane copy-out.
//!
//! Each backend keeps its own per-file `unsafe`-LoC budget (≤ 120) and
//! its own single `av_hwframe_transfer_data as hw_download` call site
//! (CI grep guard at `ci.yml:115-130`).

// Hdr10Metadata + Nv12Frame16 are referenced only by Main10 decode paths;
// under 8-bit-only feature combos (e.g. ffmpeg-decode-hevc-sw-ffmpeg5) they
// look unused, which trips `-D warnings`. Allow on the import line itself.
#[allow(unused_imports)]
use prdt_media_core::{DecodeError, Hdr10Metadata, Nv12Frame, Nv12Frame16};

use crate::error::FfmpegError;

// AVERROR(EAGAIN) = -(EAGAIN) = -11 on Linux. Same value the encoders use.
pub(crate) const AVERROR_EAGAIN: i32 = -11;
// AVERROR_EOF = -('E' | 'O' | 'F' | ' '<<24) on Linux; FFmpeg's
// MKTAG('E','O','F',' ') XOR'd with 0xFFFFFFFF: -541478725.
pub(crate) const AVERROR_EOF: i32 = -0x20464F45;

/// Single contract every HEVC decode backend implements. The viewer-side
/// `HevcDecoderAdapter<B: HevcDecoderBackend>` (see `core_adapter.rs`)
/// is generic over this trait so the three new variants in
/// `PlatformConsumer` reuse one decode path regardless of backend.
pub trait HevcDecoderBackend: Send {
    /// Feed one access unit (Annex-B byte stream). The backend buffers it
    /// internally; call `drain_frame` afterwards to pop decoded pictures.
    fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError>;

    /// Pop one decoded NV12 frame, or `None` when the backend needs more
    /// input (e.g. has only seen SPS/PPS but no IDR yet).
    fn drain_frame(&mut self) -> Result<Option<Nv12Frame>, DecodeError>;

    /// Stable identifier for logs / overlay badge.
    fn backend_name(&self) -> &'static str;
}

/// Single contract every HEVC Main10 decode backend implements. Mirrors
/// `HevcDecoderBackend` for 8-bit but emits `Nv12Frame16` (P010LE) and
/// optionally carries HDR10 metadata on each decoded frame.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any",
))]
pub trait HevcDecoderBackend10: Send {
    /// Feed one access unit (Annex-B byte stream).
    fn feed_packet(&mut self, packet: &[u8], pts_us: u64) -> Result<(), DecodeError>;

    /// Pop one decoded P010LE frame with optional HDR10 sidecar, or `None`
    /// when the backend needs more input.
    fn drain_frame(&mut self) -> Result<Option<Nv12Frame16>, DecodeError>;

    /// Stable identifier for logs / overlay badge.
    fn backend_name(&self) -> &'static str;
}

/// Map a libavcodec return code to the matching `prdt_media_core::DecodeError`.
/// Keeps the call sites in each backend's `feed_packet` / `drain_frame` short.
pub(crate) fn ffmpeg_to_decode_err(e: FfmpegError) -> DecodeError {
    match e {
        FfmpegError::Bsf(_) | FfmpegError::Send(_) | FfmpegError::Receive(_) => {
            DecodeError::Bitstream(format!("{e}"))
        }
        other => DecodeError::Backend(format!("{other}")),
    }
}

/// Copy an `AVFrame`'s NV12 planes (Y plane at `data[0]`, interleaved UV
/// at `data[1]`) into an owned `Nv12Frame`. Sized for HEVC 8-bit Main
/// profile — 10-bit Main10 is out of scope for P2.
///
/// # Safety
/// - `y_ptr` must be a valid readable pointer to `y_stride * height` bytes.
/// - `uv_ptr` must be a valid readable pointer to `uv_stride * (height / 2)` bytes.
/// - Both pointers must outlive the function call (the data is copied out).
pub(crate) unsafe fn copy_nv12_planes(
    y_ptr: *const u8,
    uv_ptr: *const u8,
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
    pts_us: u64,
) -> Nv12Frame {
    let h = height as usize;
    let mut y = vec![0u8; y_stride * h];
    let mut uv = vec![0u8; uv_stride * (h / 2)];
    // SAFETY: caller guarantees y_ptr/uv_ptr are readable for the declared
    // byte counts; the dst Vecs are freshly allocated and owned.
    unsafe {
        std::ptr::copy_nonoverlapping(y_ptr, y.as_mut_ptr(), y_stride * h);
        std::ptr::copy_nonoverlapping(uv_ptr, uv.as_mut_ptr(), uv_stride * (h / 2));
    }
    Nv12Frame {
        width,
        height,
        y,
        uv,
        stride_y: y_stride as u32,
        stride_uv: uv_stride as u32,
        pts_us,
    }
}

/// Copy an `AVFrame`'s P010LE planes into an owned `Nv12Frame16`.
/// P010LE stores each sample as a `u16` with the valid 10 bits in the
/// high part of the container (FFmpeg P010LE convention). `linesize` is in
/// bytes; we divide by 2 to get element counts.
///
/// # Safety
/// - `y_ptr` must be a valid readable pointer to `y_stride_bytes * height` bytes.
/// - `uv_ptr` must be a valid readable pointer to `uv_stride_bytes * (height / 2)` bytes.
/// - Both pointers must outlive the function call (the data is copied out).
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any",
))]
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn copy_p010_planes(
    y_ptr: *const u8,
    uv_ptr: *const u8,
    y_stride_bytes: usize,
    uv_stride_bytes: usize,
    width: u32,
    height: u32,
    pts_us: u64,
    hdr10: Option<Hdr10Metadata>,
) -> Nv12Frame16 {
    let h = height as usize;
    let y_elems = y_stride_bytes / 2;
    let uv_elems = uv_stride_bytes / 2;
    let mut y = vec![0u16; y_elems * h];
    let mut uv = vec![0u16; uv_elems * (h / 2)];
    // SAFETY: caller guarantees y_ptr/uv_ptr are readable; dst Vecs are freshly allocated.
    unsafe {
        std::ptr::copy_nonoverlapping(y_ptr as *const u16, y.as_mut_ptr(), y_elems * h);
        std::ptr::copy_nonoverlapping(uv_ptr as *const u16, uv.as_mut_ptr(), uv_elems * (h / 2));
    }
    Nv12Frame16 {
        width,
        height,
        y,
        uv,
        stride_y: y_elems as u32,
        stride_uv: uv_elems as u32,
        pts_us,
        hdr10,
    }
}

/// Re-export the cross-platform HDR10 SEI parser so existing Linux callers
/// in this crate continue to work without changes.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any",
))]
pub(crate) use crate::hdr10_sei::extract_hdr10_sidecar;
