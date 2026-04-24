use dashmap::DashMap;
use prdt_signaling_proto::ServerMessage;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub type Tx = mpsc::Sender<ServerMessage>;

pub struct HostEntry {
    pub pubkey_b64: String,
    pub tx: Tx,
    pub registered_at: Instant,
}

pub struct SessionEntry {
    pub host_id: String,
    pub host_tx: Tx,
    pub viewer_tx: Tx,
    pub created_at: Instant,
}

#[derive(Default)]
pub struct ServerState {
    pub hosts: DashMap<String, HostEntry>,
    pub sessions: DashMap<String, SessionEntry>,
}

impl ServerState {
    pub fn new() -> Self { Self::default() }

    pub fn counts(&self) -> (usize, usize) {
        (self.hosts.len(), self.sessions.len())
    }
}

pub type SharedState = Arc<ServerState>;
