use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
}

impl RendezvousConfig {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
}

#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub pubkey_b64: String,
}

#[derive(Debug, Clone)]
pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_addr: SocketAddr,
    pub peer_pubkey_b64: Option<String>,
}
