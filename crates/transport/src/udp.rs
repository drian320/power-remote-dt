use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use prdt_protocol::{
    control::ControlMessage,
    input::InputEvent,
    wire::{AudioPacket, InputPacket, PacketHeader, PacketType, HEADER_LEN},
    EncodedFrame,
};
use tokio::net::UdpSocket;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::TransportError;
use crate::fec::FecCodec;
use crate::packetize::packetize;
use crate::transport_trait::{ReceivedMessage, Transport};

/// Internal socket abstraction — direct UDP or TURN-relay-wrapped.
enum Socket {
    Direct(Arc<tokio::net::UdpSocket>),
    Relay(Arc<prdt_nat_traversal::TurnRelaySocket>),
}

impl Socket {
    async fn send_to(&self, buf: &[u8], dst: SocketAddr) -> std::io::Result<usize> {
        match self {
            Socket::Direct(s) => s.send_to(buf, dst).await,
            Socket::Relay(s) => s
                .send_to(buf, dst)
                .await
                .map_err(|e| std::io::Error::other(format!("turn: {e}"))),
        }
    }
    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        match self {
            Socket::Direct(s) => s.recv_from(buf).await,
            Socket::Relay(s) => s
                .recv_from(buf)
                .await
                .map_err(|e| std::io::Error::other(format!("turn: {e}"))),
        }
    }
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Socket::Direct(s) => s.local_addr(),
            Socket::Relay(s) => s.local_addr(),
        }
    }
    fn is_relay(&self) -> bool {
        matches!(self, Socket::Relay(_))
    }
    fn direct(&self) -> Option<&Arc<tokio::net::UdpSocket>> {
        match self {
            Socket::Direct(s) => Some(s),
            Socket::Relay(_) => None,
        }
    }
}

/// Default timeout for `handshake_as_client`. If the server doesn't respond
/// with NoiseE2 within this window (e.g. because the viewer was given the
/// wrong pubkey, or the host is unreachable), the handshake returns
/// [`TransportError::HandshakeTimeout`] instead of hanging forever.
pub const DEFAULT_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Interval between Probe retransmissions inside `probe_and_commit_peer`.
/// 200ms is short enough for LAN RTT (typically <5ms) plus firewall
/// connection-tracking install, yet long enough to avoid flooding. Exposed
/// for integration tests.
pub const PROBE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Total number of Probe transmissions per candidate (initial + retries).
/// With `PROBE_RETRY_INTERVAL = 200ms`, `PROBE_RETRY_COUNT = 5` means probes
/// are sent over the first 800ms of the overall timeout; the remaining
/// budget stays passive (only receiving). Exposed for integration tests.
pub const PROBE_RETRY_COUNT: u32 = 5;

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
    socket: Socket,
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

/// SO_RCVBUF target for the UDP socket. The viewer can spend ~100ms in NVDEC /
/// D3D11 setup between Noise handshake and the recv-loop draining packets,
/// during which the host has already produced a large IDR (~80KB at 1080p
/// 20Mbps) plus several P-frames. With Windows' default ~64KB UDP recv buffer
/// the IDR's chunks get dropped, the assembler can't reconstruct it, NVDEC
/// never sees a sequence header, and decode stays at zero forever. 4MB is
/// sized to absorb several seconds of 30Mbps video on top of the IDR while
/// the consumer warms up; OS will silently clamp if `net.ipv4.udp_rmem_max`
/// (Linux) / per-socket maximum (Windows) is smaller, which is fine.
const UDP_RCVBUF_TARGET: usize = 4 * 1024 * 1024;

/// Build a tokio `UdpSocket` whose underlying file descriptor has SO_RCVBUF
/// set to [`UDP_RCVBUF_TARGET`]. Returns `Err` if the OS rejects the bind;
/// SO_RCVBUF setting is best-effort — failures are logged and ignored, since
/// the socket is still functional with the default size.
fn bind_with_rcvbuf(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket as Sock2, Type};
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let sock = Sock2::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_nonblocking(true)?;
    if let Err(e) = sock.set_recv_buffer_size(UDP_RCVBUF_TARGET) {
        tracing::warn!(
            ?e,
            target = UDP_RCVBUF_TARGET,
            "set_recv_buffer_size failed; using default"
        );
    }
    sock.bind(&addr.into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
}

impl CustomUdpTransport {
    pub async fn bind(addr: SocketAddr, cfg: UdpTransportConfig) -> Result<Self, TransportError> {
        let socket = Socket::Direct(Arc::new(bind_with_rcvbuf(addr)?));
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

    /// Bind via TURN relay. Uses [`prdt_nat_traversal::TurnRelaySocket`] as the
    /// underlying socket so all traffic is tunnelled through a TURN server.
    /// `_bind_addr` is accepted for API symmetry with [`bind`] but is unused:
    /// the relay socket binds internally to `0.0.0.0:0`.
    pub async fn bind_with_relay(
        _bind_addr: SocketAddr,
        cfg: UdpTransportConfig,
        turn: prdt_nat_traversal::TurnConfig,
    ) -> Result<Self, TransportError> {
        let relay = Arc::new(
            prdt_nat_traversal::TurnRelaySocket::allocate(turn)
                .await
                .map_err(|e| TransportError::Io(std::io::Error::other(format!("turn: {e}"))))?,
        );
        let fec = FecCodec::new(cfg.fec_k, cfg.fec_m)?;
        Ok(Self {
            socket: Socket::Relay(relay),
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

    /// Borrow the underlying UDP socket for pre-handshake operations such as
    /// STUN learning and probe/ack exchange. The returned `Arc` clones the
    /// internal socket ref; returning ownership is safe because all transport
    /// recv/send paths hold their own clone internally.
    ///
    /// Panics if called on a relay-mode transport (see [`bind_with_relay`]).
    /// Callers that need to support both modes should first check
    /// [`is_relay`](Self::is_relay).
    pub fn socket(&self) -> std::sync::Arc<tokio::net::UdpSocket> {
        self.socket
            .direct()
            .expect("socket() called on relay-mode transport")
            .clone()
    }

    /// Returns true if this transport was created via [`bind_with_relay`] and
    /// is tunnelled through a TURN relay rather than a direct UDP socket.
    pub fn is_relay(&self) -> bool {
        self.socket.is_relay()
    }

    /// Reset session state so the next `handshake_as_server` accepts a
    /// fresh peer. Used by the host's outer session loop after a viewer
    /// disconnects or times out. Idempotent.
    ///
    /// # Preconditions
    ///
    /// All worker tasks that send or receive on this transport must have
    /// been cancelled and joined before this method is called. The internal
    /// state writes are not performed atomically; a concurrent `send` or
    /// `recv` would observe inconsistent state.
    pub async fn reset_session(&self) {
        *self.peer.lock().await = None;
        *self.crypto.lock().await = None;
        self.send_nonce.store(0, Ordering::Relaxed);
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
    ///
    /// Returns the initiator's static public key (recovered from the IK
    /// handshake under encryption) so the caller can identify the peer
    /// cryptographically and gate `accept` on a known-peer-ids list.
    pub async fn handshake_as_server(
        &self,
        server_keypair: &prdt_crypto::KeyPair,
    ) -> Result<prdt_crypto::PubKey, TransportError> {
        use prdt_crypto::ServerHandshake;

        let mut hs = Some(
            ServerHandshake::new(server_keypair)
                .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?,
        );
        loop {
            match self.recv_raw_unencrypted().await? {
                ReceivedMessage::Control(ControlMessage::NoiseE1 { payload }) => {
                    let hs_taken = hs.take().expect("handshake already consumed");
                    let (e2_payload, session, peer_pubkey) =
                        hs_taken.respond(&payload).map_err(|e| {
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
                    return Ok(peer_pubkey);
                }
                _ => continue, // drop any non-handshake traffic during handshake
            }
        }
    }

    /// Send Probe to each candidate; concurrently listen for incoming Probes
    /// (respond with ProbeAck) and incoming ProbeAcks (first match commits
    /// peer and returns). Retries Probes every `PROBE_RETRY_INTERVAL` up to
    /// `PROBE_RETRY_COUNT` total sends per candidate, to mask transient
    /// single-packet drops (common with stateful firewalls that reject the
    /// first inbound UDP until a mapping is established). Times out if no
    /// candidate replies within `timeout_duration`. Intended to be called
    /// BEFORE `handshake_as_*`.
    pub async fn probe_and_commit_peer(
        &self,
        candidates: &[SocketAddr],
        timeout_duration: std::time::Duration,
    ) -> Result<SocketAddr, TransportError> {
        use rand_core::{OsRng, RngCore};
        use std::collections::HashMap;

        // nonce → addr map. Nonces stay in the map until success (we return
        // on the first matching ProbeAck) so periodic retries know where to
        // resend. `candidates` being empty is tolerated: the loop below just
        // waits for the overall timeout.
        let mut pending: HashMap<[u8; 16], SocketAddr> = HashMap::new();
        for &addr in candidates {
            let mut nonce = [0u8; 16];
            OsRng.fill_bytes(&mut nonce);
            pending.insert(nonce, addr);
            if let Err(e) = self
                .send_control_to(ControlMessage::Probe { nonce }, addr)
                .await
            {
                tracing::warn!(?addr, error = ?e, "probe send failed; skipping candidate");
            }
        }

        let deadline = tokio::time::Instant::now() + timeout_duration;
        // Start the retry ticker one interval out so its first tick fires at
        // t=PROBE_RETRY_INTERVAL (not immediately, which would double-send).
        // Skip missed ticks rather than bursting them — under executor
        // contention we want retries spread across the first
        // PROBE_RETRY_COUNT*PROBE_RETRY_INTERVAL window, not collapsed into
        // back-to-back sends.
        let mut retry_ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + PROBE_RETRY_INTERVAL,
            PROBE_RETRY_INTERVAL,
        );
        retry_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut sends_done: u32 = 1;
        let mut buf = vec![0u8; 4096];

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::HandshakeTimeout);
            }

            tokio::select! {
                biased;
                recv = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)) => {
                    let (n, from) = match recv {
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                            // Stale ICMP unreachable from a previous peer that has gone
                            // away. UDP socket is fine — skip and wait for real packet.
                            tracing::debug!(?e, "WSAECONNRESET on recv (handshake timeout loop); ignoring");
                            continue;
                        }
                        Ok(Err(e)) => return Err(TransportError::Io(e)),
                        Err(_) => return Err(TransportError::HandshakeTimeout),
                    };
                    let hdr = match PacketHeader::decode(&buf[..n]) {
                        Ok(h) => h,
                        Err(_) => continue,
                    };
                    if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                        continue;
                    }
                    if hdr.packet_type != PacketType::Control {
                        continue;
                    }
                    if hdr.flags & prdt_protocol::packet_flags::ENCRYPTED != 0 {
                        continue;
                    }
                    let body_end = HEADER_LEN + hdr.payload_len as usize;
                    if body_end > n {
                        continue;
                    }
                    let msg = match prdt_protocol::decode_control(&buf[HEADER_LEN..body_end]) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    match msg {
                        ControlMessage::Probe { nonce } => {
                            let _ = self
                                .send_control_to(ControlMessage::ProbeAck { nonce }, from)
                                .await;
                        }
                        ControlMessage::ProbeAck { nonce } => {
                            if pending.contains_key(&nonce) {
                                self.configure_peer(from).await;
                                tracing::info!(peer = ?from, "probe winner");
                                return Ok(from);
                            }
                        }
                        _ => continue,
                    }
                }
                _ = retry_ticker.tick(), if sends_done < PROBE_RETRY_COUNT && !pending.is_empty() => {
                    sends_done += 1;
                    tracing::trace!(attempt = sends_done, pending = pending.len(), "probe retry");
                    // Copy to avoid holding borrow across await.
                    let snapshot: Vec<(_, _)> = pending
                        .iter()
                        .map(|(nonce, addr)| (*nonce, *addr))
                        .collect();
                    for (nonce, addr) in snapshot {
                        if let Err(e) = self.send_control_to(ControlMessage::Probe { nonce }, addr).await {
                            tracing::warn!(?addr, error = ?e, "probe retry send failed");
                        }
                    }
                }
            }
        }
    }

    /// Client-side Noise handshake. Sends NoiseE1 to the configured peer and
    /// awaits the server's NoiseE2, installing the transport session on
    /// completion.
    ///
    /// `client_keypair` is the viewer's long-term static keypair; the IK
    /// pattern transmits its public component to the host inside the first
    /// encrypted handshake message so the host can identify this viewer.
    ///
    /// If `timeout` elapses before NoiseE2 is received — most commonly because
    /// the viewer was given the wrong server pubkey (so the server silently
    /// drops our NoiseE1) or the host is unreachable — returns
    /// [`TransportError::HandshakeTimeout`] instead of hanging forever. Use
    /// [`DEFAULT_HANDSHAKE_TIMEOUT`] for the standard 5s budget.
    pub async fn handshake_as_client(
        &self,
        server_pubkey: &prdt_crypto::PubKey,
        client_keypair: &prdt_crypto::KeyPair,
        timeout: std::time::Duration,
    ) -> Result<(), TransportError> {
        use prdt_crypto::ClientHandshake;

        let mut hs = ClientHandshake::new(server_pubkey, client_keypair)
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?;
        let e1 = hs
            .initiate()
            .map_err(|e| TransportError::Io(std::io::Error::other(format!("crypto: {e}"))))?;
        self.send_control_unencrypted(ControlMessage::NoiseE1 { payload: e1 })
            .await?;

        let result = tokio::time::timeout(timeout, async move {
            loop {
                match self.recv_raw_unencrypted().await? {
                    ReceivedMessage::Control(ControlMessage::NoiseE2 { payload }) => {
                        let session = hs.finalize(&payload).map_err(|e| {
                            TransportError::Io(std::io::Error::other(format!("crypto: {e}")))
                        })?;
                        *self.crypto.lock().await = Some(session);
                        return Ok::<(), TransportError>(());
                    }
                    _ => continue,
                }
            }
        })
        .await;

        match result {
            Ok(r) => r,
            Err(_) => Err(TransportError::HandshakeTimeout),
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

    /// Send a ControlMessage unencrypted to an explicit destination addr,
    /// bypassing `configure_peer`. Used by `probe_and_commit_peer` which
    /// broadcasts Probes to multiple candidates before any peer is committed.
    async fn send_control_to(
        &self,
        msg: ControlMessage,
        dst: SocketAddr,
    ) -> Result<(), TransportError> {
        let body = prdt_protocol::encode_control(&msg)?;
        let hdr = PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(&hdr.encode());
        buf.extend_from_slice(&body);
        self.socket.send_to(&buf, dst).await?;
        Ok(())
    }

    /// Receive a single datagram without performing decryption. Used by the
    /// handshake to read pre-session NoiseE1/E2 frames. Unlike `recv`, any
    /// packet arriving with the ENCRYPTED flag set is dropped rather than
    /// forwarded (we cannot decrypt it without a session).
    ///
    /// # WSAECONNRESET / ConnectionReset filtering
    ///
    /// On Windows, after a viewer disconnects abruptly the OS may queue one or
    /// more ICMP "destination unreachable" responses (WSAECONNRESET, Os error
    /// 10054) against the UDP socket. Because UDP is connectionless these are
    /// stale echoes from the dead peer — the socket itself is healthy. Both
    /// this method and `recv` silently swallow `ErrorKind::ConnectionReset`
    /// and loop back to wait for the next real datagram rather than propagating
    /// the error and causing the session loop to spin.
    async fn recv_raw_unencrypted(&self) -> Result<ReceivedMessage, TransportError> {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                    // Stale ICMP unreachable from a previous peer that has gone
                    // away. UDP socket is fine — skip and wait for real packet.
                    tracing::debug!(?e, "WSAECONNRESET on recv (handshake); ignoring");
                    continue;
                }
                Err(e) => return Err(TransportError::Io(e)),
            };
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
            PacketType::Audio => {
                let pkt = AudioPacket::decode(body)?;
                Ok(Some(ReceivedMessage::Audio(pkt)))
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

    async fn send_audio(&self, pkt: AudioPacket) -> Result<(), TransportError> {
        let body = pkt.encode();
        let hdr = PacketHeader {
            packet_type: PacketType::Audio,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw(hdr, &body).await
    }

    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = match self.socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                    // Stale ICMP unreachable from a previous peer that has gone
                    // away. UDP socket is fine — skip and wait for real packet.
                    tracing::debug!(?e, "WSAECONNRESET on recv (encrypted); ignoring");
                    continue;
                }
                Err(e) => return Err(TransportError::Io(e)),
            };
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

/// Re-export the shared process-wide monotonic clock from prdt_protocol so
/// host, viewer, producer, and probes all emit timestamps on the same epoch.
pub use prdt_protocol::now_monotonic_us;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UdpTransportConfig;

    #[tokio::test]
    async fn reset_session_nulls_state() {
        let cfg = UdpTransportConfig::default();
        let bind: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let transport = CustomUdpTransport::bind(bind, cfg).await.unwrap();

        *transport.peer.lock().await = Some("127.0.0.1:9999".parse().unwrap());
        transport.send_nonce.store(42, Ordering::Relaxed);

        transport.reset_session().await;

        assert!(transport.peer.lock().await.is_none(), "peer should be None");
        assert!(
            transport.crypto.lock().await.is_none(),
            "crypto should be None"
        );
        assert_eq!(
            transport.send_nonce.load(Ordering::Relaxed),
            0,
            "send_nonce should be reset to 0"
        );
    }
}
