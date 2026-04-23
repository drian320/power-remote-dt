#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("cpal build: {0}")]
    CpalBuild(String),
    #[error("no default output device")]
    NoOutputDevice,
    #[error("no default input device")]
    NoInputDevice,
    #[error("opus: {0}")]
    Opus(String),
    #[error("format unsupported: {0}")]
    UnsupportedFormat(String),
    #[error("other: {0}")]
    Other(String),
}

impl From<audiopus::Error> for AudioError {
    fn from(e: audiopus::Error) -> Self {
        Self::Opus(format!("{e}"))
    }
}
