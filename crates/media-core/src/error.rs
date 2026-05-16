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
    /// The underlying GPU device is gone (TDR, driver crash, hot-unplug,
    /// hybrid-GPU switch). The encoder and every resource bound to its
    /// device are unusable; callers must tear down and recreate the
    /// device + encoder before retrying.
    #[error("device lost — recreate device and encoder: {0}")]
    DeviceLost(String),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("decoder backend error: {0}")]
    Backend(String),
    #[error("bitstream parse error: {0}")]
    Bitstream(String),
}

#[derive(Debug, Error, PartialEq)]
pub enum AnnexBError {
    #[error("coded buffer empty")]
    Empty,
    #[error("no Annex-B start code found in {len} byte coded buffer")]
    NoStartCode { len: usize },
}
