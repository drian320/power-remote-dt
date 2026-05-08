use thiserror::Error;

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("input injection backend error: {0}")]
    Backend(String),
    #[error("permission denied (uinput / portal access not granted): {0}")]
    PermissionDenied(String),
    #[error("no input injection backend available on this platform/compositor: {0}")]
    BackendUnavailable(String),
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard backend error: {0}")]
    Backend(String),
    #[error("no text content available")]
    NoText,
    #[error("clipboard payload too large: {0} bytes")]
    TooLarge(usize),
}
