//! WebSocket client for the power-remote-dt signaling rendezvous.

mod config;
mod error;
mod net;
mod rendezvous;

pub use config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
pub use error::SignalingError;
pub use net::discover_outbound_ip;
pub use rendezvous::{rendezvous_as_host, rendezvous_as_viewer};
