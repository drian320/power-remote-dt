//! Cross-platform HDR10 SEI parser.
//!
//! Extracts mastering display + content light level metadata from an
//! `AVFrame`'s side-data list. Used by both the Linux NVDEC Main10 decoders
//! (`crates/media-ffmpeg`) and the Windows FFmpeg-NVDEC Main10 decoder
//! (`crates/media-win/src/ffmpeg/nvdec_main10_decoder.rs`).
//!
//! The module is gated on `#[cfg(feature = "ffmpeg")]` (NOT on `target_os`)
//! so it compiles on any platform where the `ffmpeg` umbrella feature is
//! enabled. The Linux-only decoder modules in `decoder_common.rs` re-export
//! this function via `pub(crate) use`.

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
pub unsafe fn extract_hdr10_sidecar(
    frame: *const rusty_ffmpeg::ffi::AVFrame,
) -> Option<prdt_media_core::Hdr10Metadata> {
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
        for (i, slot) in display_primaries.iter_mut().enumerate() {
            *slot = (
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

    Some(prdt_media_core::Hdr10Metadata {
        display_primaries,
        white_point,
        min_mastering_luminance,
        max_mastering_luminance,
        max_content_light_level,
        max_frame_average_light_level,
    })
}
