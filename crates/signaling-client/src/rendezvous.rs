use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;

pub async fn rendezvous_as_host(
    _cfg: RendezvousConfig,
    _identity: HostIdentity,
    _local_udp_addr: std::net::SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    unimplemented!("Task 11")
}

pub async fn rendezvous_as_viewer(
    _cfg: RendezvousConfig,
    _local_udp_addr: std::net::SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    unimplemented!("Task 12")
}
