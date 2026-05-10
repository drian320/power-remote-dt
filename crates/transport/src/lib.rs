//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod handshake;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use handshake::{
    host_handshake, viewer_handshake, HelloRequest, SessionAck, DEFAULT_HELLO_RETRIES,
    DEFAULT_HELLO_TIMEOUT,
};
pub use loopback::{InProcTransport, LoopbackOptions};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
pub use udp::{
    now_monotonic_us, CustomUdpTransport, UdpTransportConfig, DEFAULT_HANDSHAKE_TIMEOUT,
    PROBE_RETRY_COUNT, PROBE_RETRY_INTERVAL,
};

#[cfg(test)]
mod idr_loss_test;
