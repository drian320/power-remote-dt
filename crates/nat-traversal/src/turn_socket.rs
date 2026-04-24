//! UdpSocket-compatible wrapper for TURN relay traffic. `send_to` wraps data
//! in a TURN Send Indication addressed to the specified peer; `recv_from`
//! auto-unwraps Data Indications arriving from the TURN server, returning
//! the inner payload and the real peer addr.

use crate::turn::{try_decode_data_indication, TurnClient, TurnConfig, TurnError};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

pub struct TurnRelaySocket {
    socket: Arc<UdpSocket>,
    client: Mutex<TurnClient>,
    permissions: Mutex<HashSet<SocketAddr>>,
    relayed: SocketAddr,
    server_addr: SocketAddr,
}

impl TurnRelaySocket {
    /// Bind a new UDP socket on `0.0.0.0:0` and allocate on the TURN server.
    pub async fn allocate(config: TurnConfig) -> Result<Self, TurnError> {
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        Self::allocate_with_socket(socket, config).await
    }

    /// Use an existing bound UDP socket for the TURN allocation.
    pub async fn allocate_with_socket(
        socket: Arc<UdpSocket>,
        config: TurnConfig,
    ) -> Result<Self, TurnError> {
        let server_addr = config.server_addr;
        let mut client = TurnClient::new(socket.clone(), config);
        let relayed = client.allocate(Duration::from_secs(5)).await?;
        Ok(Self {
            socket,
            client: Mutex::new(client),
            permissions: Mutex::new(HashSet::new()),
            relayed,
            server_addr,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// What peers see us as (the address on the TURN server).
    pub fn relayed_addr(&self) -> SocketAddr {
        self.relayed
    }

    /// The TURN server we're allocated on.
    pub fn server_addr(&self) -> SocketAddr {
        self.server_addr
    }

    /// Idempotently call CreatePermission for `peer`.
    pub async fn ensure_permission(&self, peer: SocketAddr) -> Result<(), TurnError> {
        {
            let mut perms = self.permissions.lock().await;
            if perms.contains(&peer) {
                return Ok(());
            }
            perms.insert(peer);
        }
        let mut client = self.client.lock().await;
        client.create_permission(peer, Duration::from_secs(3)).await
    }

    /// Wrap `data` in a Send Indication targeting `peer` (via the TURN server).
    /// Caller should have called `ensure_permission(peer)` first.
    pub async fn send_to(&self, data: &[u8], peer: SocketAddr) -> Result<usize, TurnError> {
        let client = self.client.lock().await;
        client.send_indication(peer, data).await?;
        Ok(data.len())
    }

    /// Read one datagram. If it's a Data Indication from the TURN server,
    /// unwrap it — returns `(bytes_written, real_peer_addr)`. The decoded
    /// payload is copied into `buf`. If the packet is direct traffic (not a
    /// valid Data Indication), the raw packet is copied as-is and the
    /// `SocketAddr` is the UDP source.
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), TurnError> {
        let mut tmp = vec![0u8; buf.len().max(1500)];
        loop {
            let (n, src) = self.socket.recv_from(&mut tmp).await?;
            if src == self.server_addr {
                // Could be a Data Indication. Try to decode.
                match try_decode_data_indication(&tmp[..n])? {
                    Some(ind) => {
                        if ind.data.len() > buf.len() {
                            return Err(TurnError::Protocol(format!(
                                "data too big for buf: {} > {}",
                                ind.data.len(), buf.len()
                            )));
                        }
                        let len = ind.data.len();
                        buf[..len].copy_from_slice(&ind.data);
                        return Ok((len, ind.peer));
                    }
                    None => continue, // was a control response, ignore
                }
            }
            // Direct traffic — pass through.
            if n > buf.len() {
                return Err(TurnError::Protocol(format!(
                    "direct datagram too big for buf: {} > {}", n, buf.len()
                )));
            }
            buf[..n].copy_from_slice(&tmp[..n]);
            return Ok((n, src));
        }
    }
}
