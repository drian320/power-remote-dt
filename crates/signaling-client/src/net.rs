//! Outbound-interface discovery helper.
//!
//! Opens a temp UDP socket, `connect`s it (no packets sent) to the signaling
//! server's resolved addr, and reads `local_addr`. The kernel picks the
//! outbound route, so the returned IP is the one the OS will actually route
//! from — useful for announcing a Host candidate on the correct LAN interface
//! instead of `0.0.0.0` or `127.0.0.1`.

use std::io;
use std::net::IpAddr;

/// Discover the local IP the OS would route outbound traffic over toward the
/// signaling server at `url`. The URL scheme is ignored; only `host_str()`
/// and `port()` are consulted (port defaults to 80 when absent).
pub async fn discover_outbound_ip(url: &url::Url) -> io::Result<IpAddr> {
    let host = url
        .host_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing host"))?;
    let port = url.port().unwrap_or(80);
    let resolved = tokio::net::lookup_host(format!("{host}:{port}"))
        .await?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no addr"))?;
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    probe.connect(resolved).await?;
    Ok(probe.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_loopback_target_to_loopback_local() {
        let url = url::Url::parse("ws://127.0.0.1:8080/signal").unwrap();
        let ip = discover_outbound_ip(&url).await.unwrap();
        assert!(ip.is_loopback(), "expected loopback, got {ip}");
    }

    #[tokio::test]
    async fn rejects_url_without_host() {
        let url = url::Url::parse("file:///tmp/x").unwrap();
        let err = discover_outbound_ip(&url).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
