use thiserror::Error;

pub const KEY_MAX: u32 = 0x2FF;

#[derive(Debug, Error)]
pub enum LinuxInputError {
    #[error("/dev/uinput open failed: {0} (hint: add user to 'input' group or install udev rule)")]
    UinputOpenDenied(std::io::Error),
    #[error("uinput ioctl failed: {0}")]
    UinputIoctl(std::io::Error),
    #[error("scancode {0:#x} out of Linux KEY_* range (max=0x2FF)")]
    ScancodeOutOfRange(u32),
    #[error("X11 connection failed: {0}")]
    X11Connect(String),
    #[error("clipboard selection request timed out")]
    ClipboardTimeout,
    #[error("clipboard returned non-UTF-8 bytes")]
    ClipboardNonUtf8,
    #[error("clipboard payload too large: {0} bytes")]
    ClipboardTooLarge(usize),
    #[error("RandR returned no CRTCs")]
    NoCrtcs,
    #[error("failed to spawn background thread: {0}")]
    ThreadSpawn(std::io::Error),
}

impl From<LinuxInputError> for prdt_input_core::InjectError {
    fn from(e: LinuxInputError) -> Self {
        use prdt_input_core::InjectError;
        match e {
            LinuxInputError::UinputOpenDenied(_) => InjectError::BackendUnavailable(e.to_string()),
            LinuxInputError::UinputIoctl(_) => InjectError::Backend(e.to_string()),
            other => InjectError::Backend(other.to_string()),
        }
    }
}

impl From<LinuxInputError> for prdt_input_core::ClipboardError {
    fn from(e: LinuxInputError) -> Self {
        use prdt_input_core::ClipboardError;
        match e {
            LinuxInputError::ClipboardTimeout | LinuxInputError::ClipboardNonUtf8 => {
                ClipboardError::NoText
            }
            LinuxInputError::ClipboardTooLarge(n) => ClipboardError::TooLarge(n),
            other => ClipboardError::Backend(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uinput_eacces_routes_to_backend_unavailable() {
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let e = LinuxInputError::UinputOpenDenied(io);
        let conv: prdt_input_core::InjectError = e.into();
        assert!(matches!(
            conv,
            prdt_input_core::InjectError::BackendUnavailable(_)
        ));
    }

    #[test]
    fn clipboard_timeout_routes_to_no_text() {
        let conv: prdt_input_core::ClipboardError = LinuxInputError::ClipboardTimeout.into();
        assert!(matches!(conv, prdt_input_core::ClipboardError::NoText));
    }

    #[test]
    fn clipboard_too_large_preserves_byte_count() {
        let conv: prdt_input_core::ClipboardError =
            LinuxInputError::ClipboardTooLarge(70_000).into();
        assert!(matches!(
            conv,
            prdt_input_core::ClipboardError::TooLarge(70_000)
        ));
    }
}
