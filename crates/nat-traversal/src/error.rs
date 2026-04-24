#[derive(thiserror::Error, Debug)]
pub enum StunError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout waiting for STUN response")]
    Timeout,
    #[error("decode: {0}")]
    Decode(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("no XOR-MAPPED-ADDRESS attribute")]
    NoMappedAddress,
}
