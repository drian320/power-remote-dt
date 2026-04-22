//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use loopback::{InProcTransport, LoopbackOptions};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
pub use udp::{now_monotonic_us, CustomUdpTransport, UdpTransportConfig};
