use prdt_protocol::{HelloRejectCode, ProtocolError};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),

    #[error("session_id mismatch: expected {expected}, got {actual}")]
    SessionIdMismatch { expected: u64, actual: u64 },

    #[error("handshake timeout")]
    HandshakeTimeout,

    #[error("hello rejected by host: {reason}")]
    HelloRejectedWithCode {
        code: HelloRejectCode,
        reason: String,
    },

    /// Legacy alias kept for call-sites that only need the reason string.
    #[error("hello rejected by host: {0}")]
    HelloRejected(String),

    #[error("peer sent Bye")]
    PeerClosed,

    #[error("frame assembler timed out for seq {frame_seq}")]
    FrameTimeout { frame_seq: u64 },

    #[error("FEC recovery failed for seq {frame_seq}: have {have}, need {need}")]
    FecFailed {
        frame_seq: u64,
        have: usize,
        need: usize,
    },

    #[error("encoded frame too large: {bytes} bytes, max {max_bytes}")]
    FrameTooLarge { bytes: usize, max_bytes: usize },

    #[error("fec configuration error: {0}")]
    FecConfig(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = TransportError::SessionIdMismatch {
            expected: 1,
            actual: 2,
        };
        assert_eq!(e.to_string(), "session_id mismatch: expected 1, got 2");
    }
}
