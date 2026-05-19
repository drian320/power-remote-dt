//! Windows-side HDR10 SEI parser. Sibling of `prdt-media-ffmpeg/src/hdr10_sei.rs`
//! that runs against the renamed `rusty_ffmpeg-win` dep instead of the
//! Linux-only `rusty_ffmpeg` dep in `prdt-media-ffmpeg`.
//!
//! Why a sibling instead of a re-export: `rusty_ffmpeg`'s build.rs requires
//! `FFMPEG_LIBS_DIR` to be set at dep-resolution time, which on Windows is
//! only available AFTER `scripts/fetch-ffmpeg-windows.ps1` has run inside
//! the CI job. Making `rusty_ffmpeg` a cross-platform dep in `media-ffmpeg`
//! caused the A2b cross-target `compile_error!` cell (which intentionally
//! enables a Linux-only feature on Windows to assert the bail) to fail at
//! dep build instead of at source compile.
//!
//! Cargo cfg gate: `#[cfg(feature = "media-win-ffmpeg-hdr10-any")]`
//! via `mod.rs` (shared with `hdr10_sidedata`).

/// Extract HDR10 mastering display + content light level metadata from an
/// `AVFrame`'s side-data list. Returns `None` if neither SEI is present.
///
/// `AVMasteringDisplayMetadata` chromaticity values are `AVRational` with
/// denominator 50000 (units of 1/50000 = 0.00002).
///
/// # Safety
/// `frame` must be a valid `AVFrame` pointer with a valid `side_data` array
/// of length `nb_side_data`. The pointer must remain valid for the duration
/// of the call (side-data is not retained).
pub unsafe fn extract_hdr10_sidecar(
    frame: *const rusty_ffmpeg_win::ffi::AVFrame,
) -> Option<prdt_media_core::Hdr10Metadata> {
    use rusty_ffmpeg_win::ffi::{
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
            &*((*mastering_sd).data as *const rusty_ffmpeg_win::ffi::AVMasteringDisplayMetadata)
        };
        let rat_to_u16 = |r: rusty_ffmpeg_win::ffi::AVRational| -> u16 {
            if r.den == 0 {
                return 0;
            }
            ((r.num as i64 * 50000) / r.den as i64).clamp(0, u16::MAX as i64) as u16
        };
        for (i, slot) in display_primaries.iter_mut().enumerate() {
            *slot = (
                rat_to_u16(md.display_primaries[i][0]),
                rat_to_u16(md.display_primaries[i][1]),
            );
        }
        white_point = (rat_to_u16(md.white_point[0]), rat_to_u16(md.white_point[1]));
        let lum_to_u32 = |r: rusty_ffmpeg_win::ffi::AVRational| -> u32 {
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
        let cl =
            unsafe { &*((*cll_sd).data as *const rusty_ffmpeg_win::ffi::AVContentLightMetadata) };
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
