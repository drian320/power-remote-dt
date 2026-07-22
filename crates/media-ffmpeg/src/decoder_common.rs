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
/// - `uv_ptr` must be a valid readable pointer to `uv_stride * height.div_ceil(2)` bytes.
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
    // Chroma rows = ceil(height / 2). NV12 4:2:0 stores one chroma row per
    // two luma rows; for an ODD display height the top luma row of the final
    // pair still needs its chroma row, and the BGRA converters read it when
    // processing that luma row. Integer `h / 2` truncates it away, leaving
    // the UV plane one row short and the converters indexing past the end.
    // The source AVFrame plane is sized for the (always even) coded height,
    // so this row is guaranteed present to copy from.
    let uv_h = h.div_ceil(2);
    let mut y = vec![0u8; y_stride * h];
    let mut uv = vec![0u8; uv_stride * uv_h];
    // SAFETY: caller guarantees y_ptr/uv_ptr are readable for the declared
    // byte counts; the dst Vecs are freshly allocated and owned.
    unsafe {
        std::ptr::copy_nonoverlapping(y_ptr, y.as_mut_ptr(), y_stride * h);
        std::ptr::copy_nonoverlapping(uv_ptr, uv.as_mut_ptr(), uv_stride * uv_h);
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

/// Copy an `AVFrame`'s planar YUV420P planes (Y at `data[0]`, U at
/// `data[1]`, V at `data[2]`) into an owned `Nv12Frame`, interleaving U/V
/// into the packed chroma plane (U at byte 0, V at byte 1 of each pair).
///
/// FFmpeg's software `hevc` decoder emits **YUV420P** for 8-bit Main
/// regardless of the `pix_fmt` we request on the codec context (a decoder
/// picks its own native output format), and a hardware surface can also
/// download as planar. Wrapping such a frame as if `data[1]` were already
/// interleaved NV12 chroma copies only the U plane (a quarter-size buffer)
/// and then reads off its end — the production viewer crash. Callers detect
/// the source layout from `AVFrame::format` and route planar frames here.
///
/// The produced frame is tightly packed: `stride_y = width`,
/// `stride_uv = width.next_multiple_of(2)` (one U/V byte pair per chroma
/// column, `width.div_ceil(2)` columns per row).
///
/// # Safety
/// - `y_ptr` must be a valid readable pointer to `y_stride * height` bytes.
/// - `u_ptr`/`v_ptr` must each be valid readable pointers to
///   `c_stride * height.div_ceil(2)` bytes.
/// - All pointers must outlive the function call (the data is copied out).
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn copy_yuv420p_as_nv12_planes(
    y_ptr: *const u8,
    u_ptr: *const u8,
    v_ptr: *const u8,
    y_stride: usize,
    c_stride: usize,
    width: u32,
    height: u32,
    pts_us: u64,
) -> Nv12Frame {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2); // chroma columns per row
    let ch = h.div_ceil(2); // chroma rows
    let uv_stride = cw * 2; // interleaved U/V byte stride (packed)

    let mut y = vec![0u8; w * h];
    let mut uv = vec![0u8; uv_stride * ch];
    // SAFETY: the caller guarantees the source planes are readable for the
    // declared strides/heights; we copy `w` luma bytes per row (dropping any
    // source stride padding) and interleave `cw` U and `cw` V bytes per
    // chroma row into freshly allocated, owned Vecs.
    unsafe {
        for row in 0..h {
            std::ptr::copy_nonoverlapping(
                y_ptr.add(row * y_stride),
                y.as_mut_ptr().add(row * w),
                w,
            );
        }
        for row in 0..ch {
            let u_row = u_ptr.add(row * c_stride);
            let v_row = v_ptr.add(row * c_stride);
            let dst = uv.as_mut_ptr().add(row * uv_stride);
            for col in 0..cw {
                *dst.add(col * 2) = *u_row.add(col);
                *dst.add(col * 2 + 1) = *v_row.add(col);
            }
        }
    }

    Nv12Frame {
        width,
        height,
        y,
        uv,
        stride_y: w as u32,
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
/// - `uv_ptr` must be a valid readable pointer to `uv_stride_bytes * height.div_ceil(2)` bytes.
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
    // Chroma rows = ceil(height / 2); see `copy_nv12_planes` for why the
    // ceiling matters for odd display heights (the tone-map converter reads
    // the final chroma row that `h / 2` would truncate).
    let uv_h = h.div_ceil(2);
    let mut y = vec![0u16; y_elems * h];
    let mut uv = vec![0u16; uv_elems * uv_h];
    // SAFETY: caller guarantees y_ptr/uv_ptr are readable; dst Vecs are freshly allocated.
    unsafe {
        std::ptr::copy_nonoverlapping(y_ptr as *const u16, y.as_mut_ptr(), y_elems * h);
        std::ptr::copy_nonoverlapping(uv_ptr as *const u16, uv.as_mut_ptr(), uv_elems * uv_h);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A planar YUV420P source (the layout the SW `hevc` decoder actually
    /// emits) with **padded** Y and chroma strides must be interleaved into a
    /// tightly-packed, self-consistent NV12 frame: U at even byte, V at odd
    /// byte of each chroma pair, source stride padding dropped. Guards against
    /// the production bug where a YUV420P frame was mis-wrapped as NV12 (only
    /// the quarter-size U plane copied, then read past its end).
    #[test]
    fn yuv420p_to_nv12_interleaves_and_drops_padding() {
        let (w, h) = (4usize, 4usize);
        let y_stride = 8usize; // padded beyond width
        let c_stride = 6usize; // padded beyond width/2
        let (cw, ch) = (w / 2, h / 2);

        let mut y = vec![0u8; y_stride * h];
        for row in 0..h {
            for col in 0..w {
                y[row * y_stride + col] = (row * 10 + col) as u8;
            }
        }
        let mut u = vec![0u8; c_stride * ch];
        let mut v = vec![0u8; c_stride * ch];
        for row in 0..ch {
            for col in 0..cw {
                u[row * c_stride + col] = (100 + row * 10 + col) as u8;
                v[row * c_stride + col] = (200 + row * 10 + col) as u8;
            }
        }

        // SAFETY: the planes are readable for the declared strides/heights.
        let f = unsafe {
            copy_yuv420p_as_nv12_planes(
                y.as_ptr(),
                u.as_ptr(),
                v.as_ptr(),
                y_stride,
                c_stride,
                w as u32,
                h as u32,
                42,
            )
        };

        // Tightly packed and self-consistent (no quarter-size UV plane).
        assert_eq!((f.width, f.height), (4, 4));
        assert_eq!(f.stride_y, 4);
        assert_eq!(f.stride_uv, 4); // cw * 2
        assert_eq!(f.y.len(), 16);
        assert_eq!(f.uv.len(), 8); // stride_uv * ch
        assert_eq!(f.pts_us, 42);

        // Y padding dropped.
        for row in 0..h {
            for col in 0..w {
                assert_eq!(f.y[row * w + col], (row * 10 + col) as u8);
            }
        }
        // Interleaved chroma: (U, V) per sample, padding dropped.
        for row in 0..ch {
            for col in 0..cw {
                let off = row * (cw * 2) + col * 2;
                assert_eq!(f.uv[off], (100 + row * 10 + col) as u8, "U at {row},{col}");
                assert_eq!(
                    f.uv[off + 1],
                    (200 + row * 10 + col) as u8,
                    "V at {row},{col}"
                );
            }
        }
    }

    /// Odd display height: the interleaved NV12 output must carry
    /// `ceil(h/2)` chroma rows so a downstream converter that reads the final
    /// luma row's chroma row does not run off the end.
    #[test]
    fn yuv420p_to_nv12_odd_height_has_ceil_chroma_rows() {
        let (w, h) = (4usize, 3usize); // odd height
        let y = vec![16u8; w * h];
        let ch = h.div_ceil(2); // 2 chroma rows
        let u = vec![110u8; (w / 2) * ch];
        let v = vec![200u8; (w / 2) * ch];
        // SAFETY: tight strides; planes sized for ceil(h/2) chroma rows.
        let f = unsafe {
            copy_yuv420p_as_nv12_planes(
                y.as_ptr(),
                u.as_ptr(),
                v.as_ptr(),
                w,
                w / 2,
                w as u32,
                h as u32,
                0,
            )
        };
        assert_eq!(f.stride_uv as usize, w); // cw*2 == 4
        assert_eq!(f.uv.len(), w * ch, "UV must have ceil(h/2) rows");
    }
}
