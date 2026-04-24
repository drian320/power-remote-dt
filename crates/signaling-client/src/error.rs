use prdt_signaling_proto::ErrorCode;

#[derive(thiserror::Error, Debug)]
pub enum SignalingError {
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server: {code:?} {message}")]
    Server { code: ErrorCode, message: String },
    #[error("timeout waiting for {stage}")]
    Timeout { stage: &'static str },
    #[error("bad candidate: {0}")]
    BadCandidate(String),
    #[error("unexpected message: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
