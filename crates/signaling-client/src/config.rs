use prdt_signaling_proto::Candidate;
use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
    /// STUN server URL, e.g. `stun://stun.l.google.com:19302`. None = STUN disabled.
    pub stun_url: Option<Url>,
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
    /// All PeerCandidates received from the other side (order preserved).
    /// W2 still picks peer_addr from the first Host-typ candidate; W3 will
    /// use this list for selection/hole-punching.
    pub peer_candidates: Vec<Candidate>,
}
