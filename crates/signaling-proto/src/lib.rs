//! Wire types for the power-remote-dt signaling protocol.
//!
//! All messages are UTF-8 JSON over WebSocket Text frames, one message per frame.
//! See `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub typ: CandidateType,
    pub ip: String,
    pub port: u16,
    pub priority: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    Host,
    Srflx,
    Relay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DoneOutcome {
    Connected,
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    HostNotFound,
    HostAlreadyRegistered,
    UnsupportedCandidateType,
    ProtocolError,
    InternalError,
    HostIdPubkeyMismatch,
}

/// Default priorities per spec §Wire Protocol.
pub const PRIORITY_HOST: u32 = 100;
pub const PRIORITY_SRFLX: u32 = 50;
pub const PRIORITY_RELAY: u32 = 10;

/// Messages sent by host or viewer to the signaling server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        host_id: String,
        pubkey_b64: String,
    },
    Connect {
        host_id: String,
    },
    Candidate {
        session_id: String,
        candidate: Candidate,
    },
    Done {
        session_id: String,
        outcome: DoneOutcome,
    },
    ProbeHosts {
        host_ids: Vec<String>,
    },
}

/// Messages sent by the signaling server to host or viewer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMessage {
    Registered {
        host_id: String,
    },
    SessionStart {
        session_id: String,
        role: Role,
        peer_pubkey_b64: Option<String>,
    },
    PeerCandidate {
        session_id: String,
        candidate: Candidate,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    ProbeResult {
        online: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_hosts_round_trip() {
        let msg = ClientMessage::ProbeHosts {
            host_ids: vec!["111-111-111".into(), "222-222-222".into()],
        };
        let s = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn probe_result_round_trip() {
        let msg = ServerMessage::ProbeResult {
            online: vec!["111-111-111".into()],
        };
        let s = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&s).unwrap();
        assert_eq!(msg, back);
    }
}
