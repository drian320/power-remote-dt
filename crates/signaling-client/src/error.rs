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

impl SignalingError {
    /// Returns `true` when this error represents a *transient* rendezvous
    /// condition that a persistent host should recover from by re-registering
    /// and waiting again, rather than a fatal error that must abort the host.
    ///
    /// This is the distinction the host's re-register loop uses to keep a
    /// signaling host AVAILABLE until a viewer connects (or the operator
    /// stops), instead of the old one-shot behavior where a mere
    /// `session_start` timeout killed the listener ~10s after "Start sharing".
    ///
    /// Retryable (host re-registers and stays available):
    /// - [`Timeout`](Self::Timeout) — most importantly the "no viewer connected
    ///   within the window" case (`stage == "session_start"`), plus `connect` /
    ///   `peer_candidate` timeouts (server momentarily slow / a viewer that
    ///   bailed after `SessionStart`).
    /// - [`WebSocket`](Self::WebSocket) / [`Io`](Self::Io) — the WS/TCP
    ///   connection dropped or reset mid-wait ("Connection reset without
    ///   closing handshake"), or the signaling server is briefly unreachable.
    /// - [`Server`](Self::Server) with [`ErrorCode::HostAlreadyRegistered`] — a
    ///   race in which the server has not yet reaped our just-dropped previous
    ///   registration; backing off and re-registering clears it.
    /// - [`Server`](Self::Server) with [`ErrorCode::InternalError`] —
    ///   server-side session timeout or a transient store error; a fresh
    ///   rendezvous recovers.
    ///
    /// Fatal (host aborts with `Err`):
    /// - [`Server`](Self::Server) with [`ErrorCode::HostIdPubkeyMismatch`] — the
    ///   host_id is bound to a different key (auth failure); retrying can never
    ///   succeed.
    /// - [`Server`](Self::Server) with [`ErrorCode::HostNotFound`] /
    ///   [`ErrorCode::ProtocolError`] /
    ///   [`ErrorCode::UnsupportedCandidateType`], plus
    ///   [`Json`](Self::Json), [`Protocol`](Self::Protocol) and
    ///   [`BadCandidate`](Self::BadCandidate) — protocol-contract violations a
    ///   retry won't fix.
    pub fn is_retryable(&self) -> bool {
        match self {
            SignalingError::Timeout { .. }
            | SignalingError::WebSocket(_)
            | SignalingError::Io(_) => true,
            SignalingError::Server { code, .. } => matches!(
                code,
                ErrorCode::HostAlreadyRegistered | ErrorCode::InternalError
            ),
            SignalingError::Json(_)
            | SignalingError::Protocol(_)
            | SignalingError::BadCandidate(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_retryable() {
        // The load-bearing case: "no viewer connected within the window".
        assert!(SignalingError::Timeout {
            stage: "session_start"
        }
        .is_retryable());
        assert!(SignalingError::Timeout {
            stage: "peer_candidate"
        }
        .is_retryable());
        assert!(SignalingError::Timeout { stage: "connect" }.is_retryable());
    }

    #[test]
    fn transient_transport_errors_are_retryable() {
        assert!(
            SignalingError::WebSocket(tokio_tungstenite::tungstenite::Error::ConnectionClosed)
                .is_retryable()
        );
        assert!(SignalingError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        ))
        .is_retryable());
    }

    #[test]
    fn register_race_is_retryable() {
        // The server reaps the previous registration asynchronously; a fast
        // re-register can transiently collide. Backing off must recover.
        assert!(SignalingError::Server {
            code: ErrorCode::HostAlreadyRegistered,
            message: "host_id already in use".into(),
        }
        .is_retryable());
        assert!(SignalingError::Server {
            code: ErrorCode::InternalError,
            message: "session timeout".into(),
        }
        .is_retryable());
    }

    #[test]
    fn auth_and_protocol_errors_are_fatal() {
        assert!(!SignalingError::Server {
            code: ErrorCode::HostIdPubkeyMismatch,
            message: "pubkey mismatch".into(),
        }
        .is_retryable());
        assert!(!SignalingError::Server {
            code: ErrorCode::ProtocolError,
            message: "bad message".into(),
        }
        .is_retryable());
        assert!(!SignalingError::Protocol("unexpected message".into()).is_retryable());
        assert!(!SignalingError::BadCandidate("bad".into()).is_retryable());
    }
}
