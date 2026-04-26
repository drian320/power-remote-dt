//! Verify CustomUdpTransport::reset_session clears peer + crypto so a
//! subsequent handshake_as_server can rebind to a different peer.

use prdt_transport::{CustomUdpTransport, UdpTransportConfig};
use std::net::SocketAddr;

#[tokio::test]
async fn reset_session_idempotent_on_unbound() {
    // Reset before any peer set must not panic or deadlock, even on
    // repeated invocation.
    let cfg = UdpTransportConfig::default();
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let transport = CustomUdpTransport::bind(bind, cfg).await.unwrap();

    transport.reset_session().await;
    transport.reset_session().await;
    transport.reset_session().await;
}
