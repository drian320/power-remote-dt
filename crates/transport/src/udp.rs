use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
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
            // fec_k=64, fec_m=6 allows 64*1200 = 76,800B per frame,
            // covering 4K60 up to ~30 Mbps average. Smoke-test feedback
            // showed lower defaults (k=8, k=32) were too tight. Plan 4
            // will add dynamic FEC sizing per spec §5.3 so we pick the
            // smallest k per frame rather than a one-size default.
            fec_k: 64,
            fec_m: 6,
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
    /// Noise transport session. `None` means no encryption has been
    /// negotiated yet; once a handshake completes, this is `Some` and all
    /// subsequent `send_raw` calls encrypt bodies and set the ENCRYPTED
    /// header flag, while `recv` auto-decrypts packets carrying that flag.
    crypto: AsyncMutex<Option<prdt_crypto::Session>>,
    /// Counter matching snow's internal sending nonce. Advanced under the
    /// `crypto` mutex together with every successful `Session::encrypt()`
    /// call, so this value equals the nonce snow used and is written into
    /// the wire packet so the receiver can call `set_receiving_nonce`
    /// before decrypt (necessary because UDP reorders chunks).
    send_nonce: AtomicU64,
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
            crypto: AsyncMutex::new(None),
            send_nonce: AtomicU64::new(0),
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

    /// Server-side Noise handshake. Call after `bind` (and after the peer
    /// address is known, typically via an initial client packet) and before
    /// the main recv loop. Waits for a NoiseE1 control message from the
    /// client, responds with NoiseE2, and installs the transport session so
    /// that all subsequent traffic is encrypted.
    pub async fn handshake_as_server(
        &self,
        server_keypair: &prdt_crypto::KeyPair,
    ) -> Result<(), TransportError> {
        use prdt_crypto::ServerHandshake;

        let mut hs = Some(
            ServerHandshake::new(server_keypair)
                .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?,
        );
        loop {
            match self.recv_raw_unencrypted().await? {
                ReceivedMessage::Control(ControlMessage::NoiseE1 { payload }) => {
                    let hs_taken = hs.take().expect("handshake already consumed");
                    let (e2_payload, session) = hs_taken.respond(&payload).map_err(|e| {
                        TransportError::Io(std::io::Error::other(format!("crypto: {e}")))
                    })?;
                    // Send NoiseE2 unencrypted (session not yet installed).
                    self.send_control_unencrypted(ControlMessage::NoiseE2 {
                        payload: e2_payload,
                    })
                    .await?;
                    // Install the session — all subsequent traffic will be
                    // encrypted automatically by send_raw / recv.
                    *self.crypto.lock().await = Some(session);
                    return Ok(());
                }
                _ => continue, // drop any non-handshake traffic during handshake
            }
        }
    }

    /// Client-side Noise handshake. Sends NoiseE1 to the configured peer and
    /// awaits the server's NoiseE2, installing the transport session on
    /// completion.
    pub async fn handshake_as_client(
        &self,
        server_pubkey: &prdt_crypto::PubKey,
    ) -> Result<(), TransportError> {
        use prdt_crypto::ClientHandshake;

        let mut hs = ClientHandshake::new(server_pubkey)
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?;
        let e1 = hs
            .initiate()
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?;
        self.send_control_unencrypted(ControlMessage::NoiseE1 { payload: e1 })
            .await?;

        loop {
            match self.recv_raw_unencrypted().await? {
                ReceivedMessage::Control(ControlMessage::NoiseE2 { payload }) => {
                    let session = hs.finalize(&payload).map_err(|e| {
                        TransportError::Io(std::io::Error::other(format!("crypto: {e}")))
                    })?;
                    *self.crypto.lock().await = Some(session);
                    return Ok(());
                }
                _ => continue,
            }
        }
    }

    /// Send a raw datagram without ever consulting the crypto session. Used
    /// during the handshake itself where the session is not yet installed.
    async fn send_raw_unencrypted(
        &self,
        hdr: PacketHeader,
        body: &[u8],
    ) -> Result<(), TransportError> {
        let peer = self.current_peer().await?;
        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(&hdr.encode());
        buf.extend_from_slice(body);
        self.socket.send_to(&buf, peer).await?;
        Ok(())
    }

    /// Send a raw datagram. If a crypto session is installed, the body is
    /// encrypted, prefixed with an explicit 8-byte big-endian nonce, and the
    /// ENCRYPTED header flag is set. The explicit nonce is required because
    /// the UDP + FEC transport can reorder chunks; snow's internal nonce
    /// counter assumes ordered delivery and would fail decryption otherwise.
    /// Without an active session, the body is sent verbatim.
    async fn send_raw(&self, mut hdr: PacketHeader, body: &[u8]) -> Result<(), TransportError> {
        let maybe_framed = {
            let mut guard = self.crypto.lock().await;
            if let Some(session) = guard.as_mut() {
                // Capture the nonce BEFORE encrypt — this is the nonce snow
                // is about to use (its internal sending counter starts at 0
                // and advances by 1 on each successful write_message, just
                // like our AtomicU64). Both the AtomicU64 bump and the snow
                // encrypt happen under this same mutex so they stay in lock
                // step.
                let nonce = self.send_nonce.fetch_add(1, Ordering::Relaxed);
                let ct = session.encrypt(body).map_err(|e| {
                    TransportError::Io(std::io::Error::other(format!("encrypt: {e}")))
                })?;
                // Prepend 8-byte big-endian nonce to the ciphertext.
                let mut framed = Vec::with_capacity(8 + ct.len());
                framed.extend_from_slice(&nonce.to_be_bytes());
                framed.extend_from_slice(&ct);
                hdr.flags |= prdt_protocol::packet_flags::ENCRYPTED;
                hdr.payload_len = framed.len() as u32;
                Some(framed)
            } else {
                None
            }
        };
        match maybe_framed {
            Some(framed) => self.send_raw_unencrypted(hdr, &framed).await,
            None => self.send_raw_unencrypted(hdr, body).await,
        }
    }

    /// Send a control message without encryption. Used for Noise handshake
    /// frames (NoiseE1/E2) which must cross the wire in the clear because no
    /// session exists yet.
    async fn send_control_unencrypted(&self, msg: ControlMessage) -> Result<(), TransportError> {
        let body = prdt_protocol::encode_control(&msg)?;
        let hdr = PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw_unencrypted(hdr, &body).await
    }

    /// Receive a single datagram without performing decryption. Used by the
    /// handshake to read pre-session NoiseE1/E2 frames. Unlike `recv`, any
    /// packet arriving with the ENCRYPTED flag set is dropped rather than
    /// forwarded (we cannot decrypt it without a session).
    async fn recv_raw_unencrypted(&self) -> Result<ReceivedMessage, TransportError> {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            // Record peer on first packet if not yet set.
            {
                let mut p = self.peer.lock().await;
                if p.is_none() {
                    *p = Some(from);
                }
            }

            let hdr = match PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(?e, "dropping malformed packet (handshake)");
                    continue;
                }
            };
            if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                tracing::warn!(
                    "session mismatch during handshake: got {}, expected {}",
                    hdr.session_id,
                    self.cfg.session_id
                );
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                tracing::warn!(
                    "truncated packet during handshake: payload_len={}, received_body_bytes={}",
                    hdr.payload_len,
                    n.saturating_sub(HEADER_LEN),
                );
                continue;
            }
            if hdr.flags & prdt_protocol::packet_flags::ENCRYPTED != 0 {
                tracing::warn!("dropping ENCRYPTED packet during handshake");
                continue;
            }
            let body = buf[HEADER_LEN..body_end].to_vec();
            if let Some(msg) = self.dispatch_packet(&hdr, &body).await? {
                return Ok(msg);
            }
        }
    }

    /// Parse and dispatch a single already-decrypted packet body according to
    /// its header type. Returns:
    /// - `Ok(Some(msg))` — message is ready to hand back to the caller
    /// - `Ok(None)` — no message yet (e.g. a video chunk was consumed by the
    ///   reassembler but the frame is still pending)
    /// - `Err(e)` — fatal decoding error
    async fn dispatch_packet(
        &self,
        hdr: &PacketHeader,
        body: &[u8],
    ) -> Result<Option<ReceivedMessage>, TransportError> {
        match hdr.packet_type {
            PacketType::Video => {
                let vp = match prdt_protocol::VideoPacket::decode(body) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(?e, "bad VideoPacket");
                        return Ok(None);
                    }
                };
                let mut asm = self.assembler.lock().await;
                match asm.feed(vp, &self.fec) {
                    Ok(crate::FeedResult::Complete(frame)) => {
                        Ok(Some(ReceivedMessage::Video(frame)))
                    }
                    Ok(crate::FeedResult::Pending) | Ok(crate::FeedResult::Stale) => Ok(None),
                    Err(e) => {
                        tracing::warn!(?e, "assembler error");
                        Ok(None)
                    }
                }
            }
            PacketType::Input => {
                let ip = InputPacket::decode(body)?;
                Ok(Some(ReceivedMessage::Input(ip.event)))
            }
            PacketType::Control => {
                let msg = prdt_protocol::decode_control(body)?;
                Ok(Some(ReceivedMessage::Control(msg)))
            }
        }
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
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            // Record peer on first packet if not yet set.
            {
                let mut p = self.peer.lock().await;
                if p.is_none() {
                    *p = Some(from);
                }
            }

            let hdr = match PacketHeader::decode(&buf[..n]) {
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
                    n.saturating_sub(HEADER_LEN),
                );
                continue;
            }
            let raw_body = &buf[HEADER_LEN..body_end];

            // Decrypt if flagged. We own the plaintext as a Vec<u8> so the
            // non-encrypted branch can borrow from `buf` without overlapping
            // lifetimes. Encrypted packets carry an explicit 8-byte
            // big-endian nonce prefix so the receiver can call
            // `set_receiving_nonce` before `decrypt`, tolerating UDP/FEC
            // reorder.
            let body_owned: Vec<u8>;
            let body: &[u8] = if hdr.flags & prdt_protocol::packet_flags::ENCRYPTED != 0 {
                if raw_body.len() < 8 {
                    tracing::warn!("encrypted packet body shorter than 8-byte nonce prefix");
                    continue;
                }
                let mut nonce_bytes = [0u8; 8];
                nonce_bytes.copy_from_slice(&raw_body[..8]);
                let nonce = u64::from_be_bytes(nonce_bytes);
                let ct = &raw_body[8..];

                let mut guard = self.crypto.lock().await;
                match guard.as_mut() {
                    Some(session) => {
                        session.set_receiving_nonce(nonce);
                        match session.decrypt(ct) {
                            Ok(pt) => {
                                body_owned = pt;
                                &body_owned[..]
                            }
                            Err(e) => {
                                tracing::warn!(?e, nonce, "decrypt failed; dropping packet");
                                continue;
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "received ENCRYPTED packet but no crypto session installed; dropping"
                        );
                        continue;
                    }
                }
            } else {
                raw_body
            };

            match self.dispatch_packet(&hdr, body).await? {
                Some(msg) => return Ok(msg),
                None => continue,
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
