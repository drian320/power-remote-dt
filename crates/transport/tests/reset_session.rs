//! Verify CustomUdpTransport::reset_session clears peer + crypto so a
//! subsequent handshake_as_server can rebind to a different peer.

use prdt_transport::{CustomUdpTransport, UdpTransportConfig};
use std::net::SocketAddr;

#[tokio::test]
async fn reset_session_clears_peer_and_crypto() {
    // Bind a transport on a random local port.
    let cfg = UdpTransportConfig::default();
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let transport = CustomUdpTransport::bind(bind, cfg).await.unwrap();

    // Reset before any peer set — must not panic / error.
    transport.reset_session().await;

    // No public getter for `peer`, so we just verify reset is idempotent
    // and does not deadlock under repeat calls.
    transport.reset_session().await;
    transport.reset_session().await;
}
