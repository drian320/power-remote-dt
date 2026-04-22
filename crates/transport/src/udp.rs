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
    assembler: AsyncMutex<crate::assembler::FrameAssembler>,
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
            assembler: AsyncMutex::new(crate::assembler::FrameAssembler::new(
                1920,
                1080,
                prdt_protocol::frame::Codec::H265,
            )),
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
        let mut buf = vec![0u8; 2048];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            // Record peer on first packet if not yet set.
            {
                let mut p = self.peer.lock().await;
                if p.is_none() {
                    *p = Some(from);
                }
            }

            let hdr = match prdt_protocol::wire::PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(?e, "dropping malformed packet");
                    continue;
                }
            };
            if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                tracing::warn!(
                    "session mismatch: got {}, expected {}",
                    hdr.session_id,
                    self.cfg.session_id
                );
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                tracing::warn!(
                    "truncated packet: hdr.payload_len={} but only {} bytes received",
                    hdr.payload_len,
                    n - HEADER_LEN,
                );
                continue;
            }
            let body = &buf[HEADER_LEN..body_end];

            match hdr.packet_type {
                PacketType::Video => {
                    let vp = match prdt_protocol::VideoPacket::decode(body) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(?e, "bad VideoPacket");
                            continue;
                        }
                    };
                    let mut asm = self.assembler.lock().await;
                    match asm.feed(vp, &self.fec) {
                        Ok(crate::FeedResult::Complete(frame)) => {
                            return Ok(ReceivedMessage::Video(frame));
                        }
                        Ok(crate::FeedResult::Pending) | Ok(crate::FeedResult::Stale) => continue,
                        Err(e) => {
                            tracing::warn!(?e, "assembler error");
                            continue;
                        }
                    }
                }
                PacketType::Input => {
                    let ip = prdt_protocol::InputPacket::decode(body)?;
                    return Ok(ReceivedMessage::Input(ip.event));
                }
                PacketType::Control => {
                    let msg = prdt_protocol::decode_control(body)?;
                    return Ok(ReceivedMessage::Control(msg));
                }
            }
        }
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
