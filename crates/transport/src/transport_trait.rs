use crate::error::TransportError;
use prdt_protocol::{control::ControlMessage, input::InputEvent, EncodedFrame};

/// A message delivered to `Transport::recv()`. Video frames are returned
/// only after all chunks have been reassembled (or reconstructed via FEC).
#[derive(Debug, Clone)]
pub enum ReceivedMessage {
    Video(EncodedFrame),
    Input(InputEvent),
    Control(ControlMessage),
}

/// Transport trait: async UDP-ish bidirectional channel.
///
/// Implementations: `CustomUdpTransport` (real UDP) and `InProcTransport`
/// (in-memory, test-only, supports drop/latency injection).
#[async_trait::async_trait]
pub trait Transport: Send {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError>;
    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError>;
    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<ReceivedMessage, TransportError>;
}
