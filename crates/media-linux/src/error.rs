//! Linux-side media error with mappings to the cross-platform
//! `prdt_media_core` error variants. See spec §10.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LinuxMediaError {
    #[error("X11 connection failed: {0}")]
    X11Connect(String),
    #[error("MIT-SHM extension unavailable")]
    ShmUnavailable,
    #[error("XGetImage failed for root window")]
    XGetImageFailed,
    #[error("openh264 backend error: {0}")]
    Openh264(#[from] prdt_media_sw::MediaSwError),
    #[error("invalid frame dimensions: {0}x{1}")]
    InvalidDimensions(u32, u32),
}

impl From<LinuxMediaError> for prdt_media_core::CaptureError {
    fn from(e: LinuxMediaError) -> Self {
        prdt_media_core::CaptureError::Backend(e.to_string())
    }
}

impl From<LinuxMediaError> for prdt_media_core::EncodeError {
    fn from(e: LinuxMediaError) -> Self {
        match e {
            LinuxMediaError::InvalidDimensions(_, _) => {
                prdt_media_core::EncodeError::FormatMismatch(e.to_string())
            }
            other => prdt_media_core::EncodeError::Backend(other.to_string()),
        }
    }
}

impl From<LinuxMediaError> for prdt_media_core::DecodeError {
    fn from(e: LinuxMediaError) -> Self {
        prdt_media_core::DecodeError::Backend(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_dims_routes_to_format_mismatch_in_encode_error() {
        let e = LinuxMediaError::InvalidDimensions(0, 1080);
        let enc: prdt_media_core::EncodeError = e.into();
        assert!(matches!(
            enc,
            prdt_media_core::EncodeError::FormatMismatch(_)
        ));
    }

    #[test]
    fn shm_unavailable_routes_to_capture_backend() {
        let e = LinuxMediaError::ShmUnavailable;
        let cap: prdt_media_core::CaptureError = e.into();
        assert!(matches!(cap, prdt_media_core::CaptureError::Backend(_)));
    }
}
