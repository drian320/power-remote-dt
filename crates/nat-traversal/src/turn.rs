//! TURN (RFC 5766) client.
//!
//! Scope: Allocate (with long-term credential auth), CreatePermission,
//! Send Indication encode, Data Indication decode. Refresh and ChannelBind
//! are out of scope (Phase 2 W5+).

use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct TurnConfig {
    pub server_addr: SocketAddr,
    pub username: String,
    pub password: String,
    pub requested_lifetime: Duration,
}

impl TurnConfig {
    /// Parse a `turn://user:pass@host:port` URL into a config.
    /// DNS lookup for the host is performed. `turn:` scheme only (no `turns:`).
    pub async fn from_url(url: &Url) -> Result<Self, TurnError> {
        if url.scheme() != "turn" {
            return Err(TurnError::BadUrl(format!(
                "unsupported scheme: {}",
                url.scheme()
            )));
        }
        let username = url.username().to_string();
        let password = url.password().unwrap_or("").to_string();
        if username.is_empty() || password.is_empty() {
            return Err(TurnError::BadUrl("username and password required".into()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| TurnError::BadUrl("missing host".into()))?;
        let port = url.port().unwrap_or(3478);
        let server_addr = tokio::net::lookup_host(format!("{host}:{port}"))
            .await
            .map_err(|e| TurnError::BadUrl(format!("resolve: {e}")))?
            .next()
            .ok_or_else(|| TurnError::BadUrl("no addrs".into()))?;
        Ok(Self {
            server_addr,
            username,
            password,
            requested_lifetime: Duration::from_secs(600),
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TurnError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad turn URL: {0}")]
    BadUrl(String),
    #[error("timeout during {stage}")]
    Timeout { stage: &'static str },
    #[error("server error: code={code} reason={reason}")]
    Server { code: u16, reason: String },
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("bad auth: {0}")]
    Auth(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn config_from_url_parses() {
        let url = "turn://user:pass@127.0.0.1:3478".parse().unwrap();
        let cfg = TurnConfig::from_url(&url).await.unwrap();
        assert_eq!(cfg.username, "user");
        assert_eq!(cfg.password, "pass");
        assert_eq!(cfg.server_addr.port(), 3478);
    }

    #[tokio::test]
    async fn config_rejects_stun_scheme() {
        let url = "stun://user:pass@127.0.0.1:3478".parse().unwrap();
        let err = TurnConfig::from_url(&url).await.unwrap_err();
        assert!(matches!(err, TurnError::BadUrl(_)));
    }

    #[tokio::test]
    async fn config_requires_credentials() {
        let url = "turn://127.0.0.1:3478".parse().unwrap();
        let err = TurnConfig::from_url(&url).await.unwrap_err();
        assert!(matches!(err, TurnError::BadUrl(_)));
    }
}
