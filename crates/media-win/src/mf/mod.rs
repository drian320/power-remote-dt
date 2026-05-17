//! Media Foundation H.265 decoder and encoder support.

pub mod decoder;
#[cfg(feature = "media-win-hevc-main10")]
pub mod decoder_main10;
pub mod encoder;

pub use decoder::H265Decoder;
#[cfg(feature = "media-win-hevc-main10")]
pub use decoder_main10::MfHevcMain10Decoder;
pub use encoder::MfH265Encoder;

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::error::MediaError;

static MF_INIT: OnceLock<Option<String>> = OnceLock::new();

pub(crate) fn ensure_mf_runtime() -> Result<(), MediaError> {
    let cached = MF_INIT.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    });
    if let Some(err) = cached {
        return Err(MediaError::Other(err.clone()));
    }
    Ok(())
}
