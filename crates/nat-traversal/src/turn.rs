//! TURN (RFC 5766) client.
//!
//! Scope: Allocate (with long-term credential auth), CreatePermission,
//! Send Indication encode, Data Indication decode. Refresh and ChannelBind
//! are out of scope (Phase 2 W5+).

use bytecodec::{DecodeExt, EncodeExt};
use rand_core::{OsRng, RngCore};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_codec::rfc5389::attributes::{ErrorCode, MessageIntegrity, Nonce, Realm, Username};
use stun_codec::rfc5766::attributes::{
    Data, Lifetime, RequestedTransport, XorPeerAddress, XorRelayAddress,
};
use stun_codec::rfc5766::methods::{ALLOCATE, CREATE_PERMISSION, DATA, SEND};
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId,
};
use tokio::net::UdpSocket;
use url::Url;

// Composite attribute enum combining RFC 5389 auth attributes with RFC 5766
// TURN attributes. The pre-composed `rfc5766::Attribute` does not include
// MESSAGE-INTEGRITY/USERNAME/REALM/NONCE/ERROR-CODE, so we build our own.
define_attribute_enums!(
    TurnAttribute,
    TurnAttributeDecoder,
    TurnAttributeEncoder,
    [
        // RFC 5389 attrs we need for auth / error handling.
        Username,
        MessageIntegrity,
        ErrorCode,
        Realm,
        Nonce,
        // RFC 5766 attrs we need for Allocate.
        Lifetime,
        RequestedTransport,
        XorRelayAddress,
        XorPeerAddress,
        Data
    ]
);

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

pub struct TurnClient {
    socket: Arc<UdpSocket>,
    config: TurnConfig,
    realm: Option<String>,
    nonce: Option<String>,
    relayed_addr: Option<SocketAddr>,
}

impl TurnClient {
    pub fn new(socket: Arc<UdpSocket>, config: TurnConfig) -> Self {
        Self {
            socket,
            config,
            realm: None,
            nonce: None,
            relayed_addr: None,
        }
    }

    pub fn realm(&self) -> Option<&str> {
        self.realm.as_deref()
    }
    pub fn nonce(&self) -> Option<&str> {
        self.nonce.as_deref()
    }
    pub fn relayed_addr(&self) -> Option<SocketAddr> {
        self.relayed_addr
    }
    pub fn config_server_addr(&self) -> SocketAddr {
        self.config.server_addr
    }

    /// Send Allocate request, handle 401 challenge, retry with MESSAGE-INTEGRITY.
    /// Stores relayed_addr, realm, nonce on success.
    pub async fn allocate(&mut self, timeout: Duration) -> Result<SocketAddr, TurnError> {
        let txn = random_transaction_id();
        let mut req1 = Message::<TurnAttribute>::new(MessageClass::Request, ALLOCATE, txn);
        // 17 = UDP per RFC 5766.
        req1.add_attribute(TurnAttribute::from(RequestedTransport::new(17)));
        let lifetime_secs: u32 = self
            .config
            .requested_lifetime
            .as_secs()
            .try_into()
            .unwrap_or(600);
        req1.add_attribute(TurnAttribute::from(Lifetime::from_u32(lifetime_secs)));
        let bytes = encode(&req1)?;
        self.socket.send_to(&bytes, self.config.server_addr).await?;

        let resp_bytes = recv_with_timeout(&self.socket, timeout).await?;
        let resp_msg = decode(&resp_bytes)?;
        if resp_msg.class() == MessageClass::ErrorResponse {
            let code = resp_msg
                .get_attribute::<ErrorCode>()
                .map(|e| e.code())
                .unwrap_or(0);
            if code == 401 {
                let realm = resp_msg
                    .get_attribute::<Realm>()
                    .ok_or_else(|| TurnError::Protocol("401 without REALM".into()))?
                    .text()
                    .to_string();
                let nonce = resp_msg
                    .get_attribute::<Nonce>()
                    .ok_or_else(|| TurnError::Protocol("401 without NONCE".into()))?
                    .value()
                    .to_string();
                self.realm = Some(realm);
                self.nonce = Some(nonce);
                return self.allocate_retry_with_auth(timeout).await;
            }
            return Err(TurnError::Server {
                code,
                reason: "allocate rejected".into(),
            });
        }
        if let Some(relayed) = resp_msg.get_attribute::<XorRelayAddress>() {
            self.relayed_addr = Some(relayed.address());
            return Ok(relayed.address());
        }
        Err(TurnError::Protocol(
            "no XOR-RELAYED-ADDRESS in success".into(),
        ))
    }

    async fn allocate_retry_with_auth(
        &mut self,
        timeout: Duration,
    ) -> Result<SocketAddr, TurnError> {
        let realm_str = self
            .realm
            .clone()
            .ok_or_else(|| TurnError::Auth("no realm".into()))?;
        let nonce_str = self
            .nonce
            .clone()
            .ok_or_else(|| TurnError::Auth("no nonce".into()))?;
        let txn = random_transaction_id();
        let mut req = Message::<TurnAttribute>::new(MessageClass::Request, ALLOCATE, txn);
        req.add_attribute(TurnAttribute::from(RequestedTransport::new(17)));
        let lifetime_secs: u32 = self
            .config
            .requested_lifetime
            .as_secs()
            .try_into()
            .unwrap_or(600);
        req.add_attribute(TurnAttribute::from(Lifetime::from_u32(lifetime_secs)));
        let username = Username::new(self.config.username.clone())
            .map_err(|e| TurnError::Auth(format!("bad username: {e:?}")))?;
        let realm = Realm::new(realm_str.clone())
            .map_err(|e| TurnError::Auth(format!("bad realm: {e:?}")))?;
        let nonce = Nonce::new(nonce_str.clone())
            .map_err(|e| TurnError::Auth(format!("bad nonce: {e:?}")))?;
        req.add_attribute(TurnAttribute::from(username.clone()));
        req.add_attribute(TurnAttribute::from(realm.clone()));
        req.add_attribute(TurnAttribute::from(nonce));
        let mi = MessageIntegrity::new_long_term_credential(
            &req,
            &username,
            &realm,
            &self.config.password,
        )
        .map_err(|e| TurnError::Auth(format!("MI: {e:?}")))?;
        req.add_attribute(TurnAttribute::from(mi));

        let bytes = encode(&req)?;
        self.socket.send_to(&bytes, self.config.server_addr).await?;

        let resp_bytes = recv_with_timeout(&self.socket, timeout).await?;
        let resp_msg = decode(&resp_bytes)?;
        if resp_msg.class() != MessageClass::SuccessResponse {
            let code = resp_msg
                .get_attribute::<ErrorCode>()
                .map(|e| e.code())
                .unwrap_or(0);
            return Err(TurnError::Server {
                code,
                reason: "allocate retry failed".into(),
            });
        }
        let relayed = resp_msg
            .get_attribute::<XorRelayAddress>()
            .ok_or_else(|| TurnError::Protocol("success without XOR-RELAYED".into()))?
            .address();
        self.relayed_addr = Some(relayed);
        Ok(relayed)
    }

    /// Send CreatePermission request for a peer address, authenticated with the
    /// realm/nonce cached from a prior `allocate()`. RFC 5766 Sec. 9.
    pub async fn create_permission(
        &mut self,
        peer: SocketAddr,
        timeout: Duration,
    ) -> Result<(), TurnError> {
        let realm_str = self
            .realm
            .clone()
            .ok_or_else(|| TurnError::Auth("no realm — call allocate first".into()))?;
        let nonce_str = self
            .nonce
            .clone()
            .ok_or_else(|| TurnError::Auth("no nonce".into()))?;
        let txn = random_transaction_id();
        let mut req = Message::<TurnAttribute>::new(MessageClass::Request, CREATE_PERMISSION, txn);
        req.add_attribute(TurnAttribute::from(XorPeerAddress::new(peer)));
        let username = Username::new(self.config.username.clone())
            .map_err(|e| TurnError::Auth(format!("bad username: {e:?}")))?;
        let realm = Realm::new(realm_str.clone())
            .map_err(|e| TurnError::Auth(format!("bad realm: {e:?}")))?;
        let nonce = Nonce::new(nonce_str.clone())
            .map_err(|e| TurnError::Auth(format!("bad nonce: {e:?}")))?;
        req.add_attribute(TurnAttribute::from(username.clone()));
        req.add_attribute(TurnAttribute::from(realm.clone()));
        req.add_attribute(TurnAttribute::from(nonce));
        let mi = MessageIntegrity::new_long_term_credential(
            &req,
            &username,
            &realm,
            &self.config.password,
        )
        .map_err(|e| TurnError::Auth(format!("MI: {e:?}")))?;
        req.add_attribute(TurnAttribute::from(mi));

        let bytes = encode(&req)?;
        self.socket.send_to(&bytes, self.config.server_addr).await?;

        let resp_bytes = recv_with_timeout(&self.socket, timeout).await?;
        let resp_msg = decode(&resp_bytes)?;
        if resp_msg.class() != MessageClass::SuccessResponse {
            let code = resp_msg
                .get_attribute::<ErrorCode>()
                .map(|e| e.code())
                .unwrap_or(0);
            return Err(TurnError::Server {
                code,
                reason: "create_permission failed".into(),
            });
        }
        Ok(())
    }

    /// Wrap `data` in a Send Indication addressed to `peer` and send it to the
    /// TURN server. Send Indications are NOT authenticated per RFC 5766 §10.
    pub async fn send_indication(&self, peer: SocketAddr, data: &[u8]) -> Result<(), TurnError> {
        let txn = random_transaction_id();
        let mut msg = Message::<TurnAttribute>::new(MessageClass::Indication, SEND, txn);
        msg.add_attribute(TurnAttribute::from(XorPeerAddress::new(peer)));
        msg.add_attribute(TurnAttribute::from(
            Data::new(data.to_vec())
                .map_err(|e| TurnError::Protocol(format!("data too large: {e:?}")))?,
        ));
        let bytes = encode(&msg)?;
        self.socket.send_to(&bytes, self.config.server_addr).await?;
        Ok(())
    }
}

/// One decoded Data Indication: opaque payload + the peer addr it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataIndication {
    pub peer: SocketAddr,
    pub data: Vec<u8>,
}

/// If `bytes` is a TURN Data Indication, return Some(DataIndication). Otherwise
/// (not a STUN message at all, or another type), return Ok(None).
pub fn try_decode_data_indication(bytes: &[u8]) -> Result<Option<DataIndication>, TurnError> {
    let mut d = MessageDecoder::<TurnAttribute>::new();
    let msg = match d.decode_from_bytes(bytes) {
        Ok(Ok(m)) => m,
        _ => return Ok(None),
    };
    if msg.class() != MessageClass::Indication || msg.method() != DATA {
        return Ok(None);
    }
    let peer = msg
        .get_attribute::<XorPeerAddress>()
        .ok_or_else(|| TurnError::Protocol("DataIndication without XOR-PEER-ADDRESS".into()))?
        .address();
    let data = msg
        .get_attribute::<Data>()
        .ok_or_else(|| TurnError::Protocol("DataIndication without DATA".into()))?
        .data()
        .to_vec();
    Ok(Some(DataIndication { peer, data }))
}

pub(crate) fn random_transaction_id() -> TransactionId {
    let mut id = [0u8; 12];
    OsRng.fill_bytes(&mut id);
    TransactionId::new(id)
}

pub(crate) fn encode(msg: &Message<TurnAttribute>) -> Result<Vec<u8>, TurnError> {
    let mut e = MessageEncoder::<TurnAttribute>::new();
    e.encode_into_bytes(msg.clone())
        .map_err(|err| TurnError::Protocol(format!("encode: {err}")))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Message<TurnAttribute>, TurnError> {
    let mut d = MessageDecoder::<TurnAttribute>::new();
    match d.decode_from_bytes(bytes) {
        Ok(Ok(m)) => Ok(m),
        Ok(Err(e)) => Err(TurnError::Protocol(format!("decode body: {e:?}"))),
        Err(e) => Err(TurnError::Protocol(format!("decode: {e}"))),
    }
}

pub(crate) async fn recv_with_timeout(
    socket: &UdpSocket,
    dur: Duration,
) -> Result<Vec<u8>, TurnError> {
    let mut buf = vec![0u8; 1500];
    match tokio::time::timeout(dur, socket.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => {
            buf.truncate(n);
            Ok(buf)
        }
        Ok(Err(e)) => Err(TurnError::Io(e)),
        Err(_) => Err(TurnError::Timeout { stage: "turn_recv" }),
    }
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
