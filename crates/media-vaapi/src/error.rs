//! VAAPI error model + VAStatus → Result mapping.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VaapiError {
    #[error("display open failed: {0}")]
    DisplayOpen(String),
    #[error("no /dev/dri/renderD* found")]
    NoRenderNode,
    #[error("configuration not supported: {0}")]
    NotSupported(String),
    #[error("hardware busy (retry exhausted, attempts={attempts})")]
    HardwareBusy { attempts: u32 },
    #[error("driver returned VA_STATUS_ERROR_{0}")]
    DriverError(i32),
    #[error("bitstream normalization failed: {0}")]
    Bitstream(String),
    #[error("encoder closed (call new() to reopen)")]
    Closed,
}

impl From<prdt_media_core::AnnexBError> for VaapiError {
    fn from(e: prdt_media_core::AnnexBError) -> Self {
        VaapiError::Bitstream(format!("annex-b: {e}"))
    }
}

/// Classifier for raw VAStatus codes. Only handles error mapping at the
/// boundary; success codes return Ok at the FFI call site directly.
#[allow(dead_code)]
pub(crate) fn classify_va_status(status: i32, ctx: &'static str) -> VaapiError {
    // libva: VA_STATUS_SUCCESS=0, error codes follow.
    // From <va/va.h>:
    //   VA_STATUS_ERROR_OPERATION_FAILED = 0x00000001
    //   VA_STATUS_ERROR_ALLOCATION_FAILED = 0x00000002
    //   VA_STATUS_ERROR_INVALID_CONFIG = 0x00000007
    //   VA_STATUS_ERROR_HW_BUSY = 0x00000017
    //   VA_STATUS_ERROR_TIMEDOUT (alias of HW_BUSY context, ~0x00000017)
    //   VA_STATUS_ERROR_UNIMPLEMENTED = 0x00000022
    //   VA_STATUS_ERROR_UNSUPPORTED_PROFILE = 0x00000020
    //   VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT = 0x00000021
    match status {
        0x17 /* HW_BUSY / TIMEDOUT */ => VaapiError::HardwareBusy { attempts: 0 },
        0x07 /* INVALID_CONFIG */
        | 0x20 /* UNSUPPORTED_PROFILE */
        | 0x21 /* UNSUPPORTED_ENTRYPOINT */
        | 0x22 /* UNIMPLEMENTED */ => VaapiError::NotSupported(format!("{ctx}: status={status:#x}")),
        other => VaapiError::DriverError(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_va_status_classifies_hw_busy() {
        assert_eq!(
            classify_va_status(0x17, "test"),
            VaapiError::HardwareBusy { attempts: 0 }
        );
    }

    #[test]
    fn classify_va_status_classifies_unsupported_profile() {
        let e = classify_va_status(0x20, "config");
        assert!(matches!(e, VaapiError::NotSupported(_)));
    }

    #[test]
    fn classify_va_status_falls_through_to_driver_error() {
        assert_eq!(
            classify_va_status(0x99, "any"),
            VaapiError::DriverError(0x99)
        );
    }

    #[test]
    fn vaapi_error_display_includes_context() {
        let e = VaapiError::DisplayOpen("permission denied".into());
        let s = format!("{e}");
        assert!(s.contains("permission denied"));
    }
}
