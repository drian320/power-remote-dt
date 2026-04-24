use crate::host_store::HostStore;
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

pub struct ServerState {
    pub hosts: DashMap<String, HostEntry>,
    pub sessions: DashMap<String, SessionEntry>,
    pub store: Arc<HostStore>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            hosts: DashMap::new(),
            sessions: DashMap::new(),
            store: Arc::new(HostStore::open_in_memory().expect("in-memory sqlite")),
        }
    }

    pub fn with_store(store: Arc<HostStore>) -> Self {
        Self {
            hosts: DashMap::new(),
            sessions: DashMap::new(),
            store,
        }
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.hosts.len(), self.sessions.len())
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedState = Arc<ServerState>;
