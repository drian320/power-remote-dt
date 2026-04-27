//! Error surface for media-sw. Wraps `openh264::Error` and adds higher-level
//! semantic failures specific to the SW codec path.

#[derive(Debug, thiserror::Error)]
pub enum MediaSwError {
    #[error("openh264: {context}: {source}")]
    OpenH264 {
        context: &'static str,
        #[source]
        source: openh264::Error,
    },

    #[error("invalid I420 frame: {reason}")]
    InvalidFrame { reason: String },

    #[error("decoder produced no frame for input ({hint})")]
    NoFrame { hint: &'static str },

    #[error("dimension mismatch: expected {expected_w}x{expected_h}, got {got_w}x{got_h}")]
    DimensionMismatch {
        expected_w: u32,
        expected_h: u32,
        got_w: u32,
        got_h: u32,
    },

    #[error("other: {0}")]
    Other(String),
}

impl MediaSwError {
    pub fn openh264(context: &'static str, err: openh264::Error) -> Self {
        Self::OpenH264 {
            context,
            source: err,
        }
    }
}

pub type Result<T> = std::result::Result<T, MediaSwError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = MediaSwError::InvalidFrame {
            reason: "y plane too short".into(),
        };
        assert_eq!(e.to_string(), "invalid I420 frame: y plane too short");

        let e = MediaSwError::DimensionMismatch {
            expected_w: 1920,
            expected_h: 1080,
            got_w: 640,
            got_h: 480,
        };
        assert_eq!(
            e.to_string(),
            "dimension mismatch: expected 1920x1080, got 640x480"
        );
    }
}
