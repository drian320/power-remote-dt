//! Optional supervisor channel between host's main loop and an embedding
//! GUI (Phase 4 G1). When `None`, the loop runs as before with no status
//! reporting.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Idle,
    Listening,
    Stopping,
}

#[derive(Debug)]
pub struct HostStatus {
    pub state: HostState,
    pub pubkey_b64: String,
    pub allocated_host_id: Option<String>,
    pub listening_addr: Option<SocketAddr>,
    pub peers_connected: u32,
    pub bitrate_mbps_actual: f32,
    pub last_log_lines: VecDeque<String>,
}

impl Default for HostStatus {
    fn default() -> Self {
        Self {
            state: HostState::Idle,
            pubkey_b64: String::new(),
            allocated_host_id: None,
            listening_addr: None,
            peers_connected: 0,
            bitrate_mbps_actual: 0.0,
            last_log_lines: VecDeque::with_capacity(200),
        }
    }
}

pub type SharedStatus = Arc<Mutex<HostStatus>>;
