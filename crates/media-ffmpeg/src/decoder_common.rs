//! Shared scaffolding for the three HEVC decode backends in this crate
//! (sw / vaapi / nvdec). Holds the `HevcDecoderBackend` trait every
//! backend implements and small NV12 helpers used during the per-frame
//! plane copy-out.
//!
//! Each backend keeps its own per-file `unsafe`-LoC budget (≤ 120) and
//! its own single `av_hwframe_transfer_data as hw_download` call site
//! (CI grep guard at `ci.yml:115-130`).

use prdt_media_core::{DecodeError, Nv12Frame};

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
