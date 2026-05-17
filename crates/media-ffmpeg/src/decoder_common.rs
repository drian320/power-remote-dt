//! Shared scaffolding for the three HEVC decode backends in this crate
//! (sw / vaapi / nvdec). Holds the `HevcDecoderBackend` trait every
//! backend implements and small NV12 helpers used during the per-frame
//! plane copy-out.
//!
//! Each backend keeps its own per-file `unsafe`-LoC budget (≤ 120) and
//! its own single `av_hwframe_transfer_data as hw_download` call site
//! (CI grep guard at `ci.yml:115-130`).

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

/// Extract HDR10 mastering display + content light level metadata from an
/// `AVFrame`'s side-data list. Returns `None` if neither SEI is present.
///
/// `AVMasteringDisplayMetadata` chromaticity values are `AVRational` with
/// denominator 50000 (units of 1/50000 = 0.00002). We round to the nearest
/// u16 after scaling to match `Hdr10Metadata`'s units of 0.00002 (i.e. we
/// divide by denom and multiply by 50000 to get the stored integer).
///
/// `AVContentLightMetadata` MaxCLL/MaxFALL are plain u32 cd/m² values.
///
/// # Safety
/// `frame` must be a valid `AVFrame` pointer with a valid `side_data` array
/// of length `nb_side_data`. The pointer must remain valid for the duration
/// of the call (side-data is not retained).
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any",
))]
pub(crate) unsafe fn extract_hdr10_sidecar(
    frame: *const rusty_ffmpeg::ffi::AVFrame,
) -> Option<Hdr10Metadata> {
    use rusty_ffmpeg::ffi::{
        av_frame_get_side_data, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
        AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
    };

    // SAFETY: frame is a valid AVFrame for the call duration.
    let mastering_sd = unsafe {
        av_frame_get_side_data(frame as *mut _, AV_FRAME_DATA_MASTERING_DISPLAY_METADATA)
    };
    let cll_sd =
        // SAFETY: frame is a valid AVFrame for the call duration.
        unsafe { av_frame_get_side_data(frame as *mut _, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL) };

    if mastering_sd.is_null() && cll_sd.is_null() {
        return None;
    }

    let mut display_primaries = [(0u16, 0u16); 3];
    let mut white_point = (0u16, 0u16);
    let mut min_mastering_luminance = 0u32;
    let mut max_mastering_luminance = 0u32;
    let mut max_content_light_level = 0u16;
    let mut max_frame_average_light_level = 0u16;

    if !mastering_sd.is_null() {
        // SAFETY: side-data pointer is non-null; data points to an AVMasteringDisplayMetadata.
        let md = unsafe {
            &*((*mastering_sd).data as *const rusty_ffmpeg::ffi::AVMasteringDisplayMetadata)
        };
        // Chromaticity: AVRational {num, den}. Scale: value = num/den in units of
        // 1; Hdr10Metadata stores in units of 0.00002 → multiply num by 50000 / den.
        let rat_to_u16 = |r: rusty_ffmpeg::ffi::AVRational| -> u16 {
            if r.den == 0 {
                return 0;
            }
            ((r.num as i64 * 50000) / r.den as i64).clamp(0, u16::MAX as i64) as u16
        };
        // display_primaries[i] = (x, y); order R, G, B as per SMPTE 2086.
        for i in 0..3usize {
            display_primaries[i] = (
                rat_to_u16(md.display_primaries[i][0]),
                rat_to_u16(md.display_primaries[i][1]),
            );
        }
        white_point = (rat_to_u16(md.white_point[0]), rat_to_u16(md.white_point[1]));
        // Luminance: AVRational in cd/m²; Hdr10Metadata stores in units of 0.0001 cd/m²
        // → multiply num by 10000 / den.
        let lum_to_u32 = |r: rusty_ffmpeg::ffi::AVRational| -> u32 {
            if r.den == 0 {
                return 0;
            }
            ((r.num as i64 * 10000) / r.den as i64).clamp(0, u32::MAX as i64) as u32
        };
        min_mastering_luminance = lum_to_u32(md.min_luminance);
        max_mastering_luminance = lum_to_u32(md.max_luminance);
    }

    if !cll_sd.is_null() {
        // SAFETY: side-data pointer is non-null; data points to an AVContentLightMetadata.
        let cl = unsafe { &*((*cll_sd).data as *const rusty_ffmpeg::ffi::AVContentLightMetadata) };
        max_content_light_level = cl.MaxCLL.clamp(0, u16::MAX as u32) as u16;
        max_frame_average_light_level = cl.MaxFALL.clamp(0, u16::MAX as u32) as u16;
    }

    Some(Hdr10Metadata {
        display_primaries,
        white_point,
        min_mastering_luminance,
        max_mastering_luminance,
        max_content_light_level,
        max_frame_average_light_level,
    })
}
