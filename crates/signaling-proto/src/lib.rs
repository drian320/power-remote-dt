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
}

/// Default priorities per spec §Wire Protocol.
pub const PRIORITY_HOST: u32 = 100;
pub const PRIORITY_SRFLX: u32 = 50;
pub const PRIORITY_RELAY: u32 = 10;
