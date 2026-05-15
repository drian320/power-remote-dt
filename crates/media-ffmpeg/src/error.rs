use thiserror::Error;

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("libavcodec runtime probe failed")]
    NoLibavcodec,
    #[error("encoder not found: {0}")]
    EncoderNotFound(&'static str),
    #[error("hw device error: {0}")]
    HwDevice(String),
    #[error("hw frames error: {0}")]
    HwFrames(String),
    #[error("avcodec_open2 failed: {0}")]
    OpenCodec(i32),
    #[error("avcodec_send_frame failed: {0}")]
    Send(i32),
    #[error("avcodec_receive_packet failed: {0}")]
    Receive(i32),
    #[error("BSF error: {0}")]
    Bsf(i32),
    #[error("hw frame transfer failed: {0}")]
    Transfer(i32),
    #[error("encoder closed")]
    Closed,
    #[error("drop assertion failed: {0}")]
    Drop(String),
}

// AVERROR(ENODEV) = -(ENODEV) in POSIX errno space, sign-extended to i32.
// On Linux, ENODEV = 19, so AVERROR(ENODEV) = -19.
const AVERROR_ENODEV: i32 = -19;

impl From<FfmpegError> for prdt_media_core::EncodeError {
    fn from(e: FfmpegError) -> Self {
        match &e {
            FfmpegError::Send(code) | FfmpegError::Receive(code) if *code == AVERROR_ENODEV => {
                prdt_media_core::EncodeError::DeviceLost(format!("{e}"))
            }
            FfmpegError::HwDevice(msg) if msg.contains("device lost") => {
                prdt_media_core::EncodeError::DeviceLost(format!("{e}"))
            }
            _ => prdt_media_core::EncodeError::Backend(format!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_not_found_maps_to_backend() {
        let e = FfmpegError::EncoderNotFound("hevc_vaapi");
        let enc: prdt_media_core::EncodeError = e.into();
        assert!(matches!(enc, prdt_media_core::EncodeError::Backend(_)));
    }

    #[test]
    fn vaapi_device_lost_maps_to_device_lost() {
        let e = FfmpegError::Send(AVERROR_ENODEV);
        let enc: prdt_media_core::EncodeError = e.into();
        assert!(matches!(enc, prdt_media_core::EncodeError::DeviceLost(_)));
    }
}
