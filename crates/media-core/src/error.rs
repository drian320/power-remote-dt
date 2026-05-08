use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("backend lost (display reset / device removal): {0}")]
    BackendLost(String),
    #[error("capture backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder backend error: {0}")]
    Backend(String),
    #[error("input frame format mismatch: {0}")]
    FormatMismatch(String),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("decoder backend error: {0}")]
    Backend(String),
    #[error("bitstream parse error: {0}")]
    Bitstream(String),
}
