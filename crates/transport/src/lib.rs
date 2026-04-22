//! Custom UDP transport with FEC for power-remote-dt.

pub mod error;
pub mod fec;
pub mod packetize;
pub mod transport_trait;

pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
