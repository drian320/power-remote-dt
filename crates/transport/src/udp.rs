use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use prdt_protocol::{
    control::ControlMessage,
    input::InputEvent,
    wire::{InputPacket, PacketHeader, PacketType, HEADER_LEN},
    EncodedFrame,
};
use tokio::net::UdpSocket;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::TransportError;
use crate::fec::FecCodec;
use crate::packetize::packetize;
use crate::transport_trait::{ReceivedMessage, Transport};

/// Configuration for a CustomUdpTransport instance.
#[derive(Debug, Clone, Copy)]
pub struct UdpTransportConfig {
    pub session_id: u64,
    pub chunk_payload_len: usize,
    pub fec_k: usize,
    pub fec_m: usize,
}

impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            session_id: 0,
            chunk_payload_len: prdt_protocol::DEFAULT_CHUNK_PAYLOAD_LEN,
            fec_k: 8,
            fec_m: 2,
        }
    }
}

/// UDP transport with per-frame FEC. Recv path lives in a separate task.
pub struct CustomUdpTransport {
    socket: Arc<UdpSocket>,
    cfg: UdpTransportConfig,
    peer: AsyncMutex<Option<SocketAddr>>, // set after first packet received or configure_peer()
    fec: FecCodec,
    input_seq: AsyncMutex<u64>,
}

impl CustomUdpTransport {
    pub async fn bind(addr: SocketAddr, cfg: UdpTransportConfig) -> Result<Self, TransportError> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let fec = FecCodec::new(cfg.fec_k, cfg.fec_m)?;
        Ok(Self {
            socket,
            cfg,
            peer: AsyncMutex::new(None),
            fec,
            input_seq: AsyncMutex::new(0),
        })
    }

    pub async fn configure_peer(&self, peer: SocketAddr) {
        *self.peer.lock().await = Some(peer);
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    async fn current_peer(&self) -> Result<SocketAddr, TransportError> {
        self.peer.lock().await.ok_or_else(|| {
            TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "peer address not set",
            ))
        })
    }

    async fn send_raw(&self, hdr: PacketHeader, body: &[u8]) -> Result<(), TransportError> {
        let peer = self.current_peer().await?;
        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(&hdr.encode());
        buf.extend_from_slice(body);
        self.socket.send_to(&buf, peer).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for CustomUdpTransport {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError> {
        let pkts = packetize(&frame, &self.fec, self.cfg.chunk_payload_len)?;
        for pkt in pkts {
            let body = pkt.encode();
            let hdr = PacketHeader {
                packet_type: PacketType::Video,
                flags: 0,
                session_id: self.cfg.session_id,
                payload_len: body.len() as u32,
            };
            self.send_raw(hdr, &body).await?;
        }
        Ok(())
    }

    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError> {
        let seq = {
            let mut g = self.input_seq.lock().await;
            *g += 1;
            *g
        };
        let pkt = InputPacket {
            input_seq: seq,
            timestamp_viewer_us: now_monotonic_us(),
            event: ev,
        };
        let body = pkt.encode();
        let hdr = PacketHeader {
            packet_type: PacketType::Input,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw(hdr, &body).await
    }

    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError> {
        let body = prdt_protocol::encode_control(&msg)?;
        let hdr = PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw(hdr, &body).await
    }

    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        // Recv path is wired up in the next task (Task 19).
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "recv not yet implemented (see Task 19)",
        )))
    }
}

/// Monotonic clock reading in microseconds (u64). Uses Instant::now() on a
/// per-process epoch that is set the first time this function is called.
pub fn now_monotonic_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_micros() as u64
}
