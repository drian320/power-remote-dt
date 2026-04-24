use prdt_signaling_proto::Candidate;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
    pub stun_url: Option<Url>,
    pub turn_url: Option<Url>,
    /// After the first PeerCandidate arrives, wait this long for more before
    /// returning. Default 2s. Tests typically use 100-300ms for speed.
    pub aggregation_window: Duration,
}

impl RendezvousConfig {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
    pub const DEFAULT_AGGREGATION_WINDOW: Duration = Duration::from_secs(2);
}

#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub pubkey_b64: String,
}

#[derive(Debug, Clone)]
pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_pubkey_b64: Option<String>,
    /// All PeerCandidates collected during the aggregation window (order of
    /// arrival preserved). W3's `probe_and_commit_peer` selects the actual
    /// peer address from this list via live probing.
    pub peer_candidates: Vec<Candidate>,
    /// Server-allocated / confirmed host_id. For rendezvous_as_host, this is
    /// the ID the signaling-server returned in its `Registered` reply. For
    /// rendezvous_as_viewer, this is an empty string.
    pub allocated_host_id: String,
}
