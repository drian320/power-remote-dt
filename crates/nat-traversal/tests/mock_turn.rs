//! In-process TURN mock + TurnClient integration tests.

use bytecodec::{DecodeExt, EncodeExt};
use prdt_nat_traversal::turn::{TurnAttribute, TurnClient, TurnConfig};
use std::net::SocketAddr;
use std::time::Duration;
use stun_codec::rfc5389::attributes::{ErrorCode, MessageIntegrity, Nonce, Realm, Username};
use stun_codec::rfc5766::attributes::{Lifetime, XorRelayAddress};
use stun_codec::rfc5766::methods::ALLOCATE;
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder};
use tokio::net::UdpSocket;

const REALM: &str = "prdt-test";
const NONCE: &str = "01234567";

async fn spawn_mock_turn(
    username: &'static str,
    _password: &'static str,
) -> (SocketAddr, SocketAddr) {
    let relayed: SocketAddr = "127.0.0.1:55555".parse().unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else {
                break;
            };
            let mut decoder = MessageDecoder::<TurnAttribute>::new();
            let req = match decoder.decode_from_bytes(&buf[..n]) {
                Ok(Ok(m)) => m,
                _ => continue,
            };
            if req.class() != MessageClass::Request {
                continue;
            }
            if req.method() == ALLOCATE {
                let has_mi = req.get_attribute::<MessageIntegrity>().is_some();
                if !has_mi {
                    // 401 challenge
                    let mut resp = Message::<TurnAttribute>::new(
                        MessageClass::ErrorResponse,
                        ALLOCATE,
                        req.transaction_id(),
                    );
                    resp.add_attribute(TurnAttribute::from(
                        ErrorCode::new(401, "Unauthorized".to_string()).unwrap(),
                    ));
                    resp.add_attribute(TurnAttribute::from(
                        Realm::new(REALM.to_string()).unwrap(),
                    ));
                    resp.add_attribute(TurnAttribute::from(
                        Nonce::new(NONCE.to_string()).unwrap(),
                    ));
                    let mut enc = MessageEncoder::<TurnAttribute>::new();
                    let bytes = enc.encode_into_bytes(resp).unwrap();
                    let _ = socket.send_to(&bytes, src).await;
                } else {
                    // Validate username matches (real servers also verify HMAC; mock trusts MI presence)
                    let got_user = req
                        .get_attribute::<Username>()
                        .map(|u| u.name().to_string());
                    if got_user.as_deref() != Some(username) {
                        continue;
                    }
                    // Send Allocate success
                    let mut resp = Message::<TurnAttribute>::new(
                        MessageClass::SuccessResponse,
                        ALLOCATE,
                        req.transaction_id(),
                    );
                    resp.add_attribute(TurnAttribute::from(XorRelayAddress::new(relayed)));
                    resp.add_attribute(TurnAttribute::from(Lifetime::from_u32(600)));
                    let mut enc = MessageEncoder::<TurnAttribute>::new();
                    let bytes = enc.encode_into_bytes(resp).unwrap();
                    let _ = socket.send_to(&bytes, src).await;
                }
            } else if req.method() == stun_codec::rfc5766::methods::CREATE_PERMISSION {
                // Accept any request that has MI; reply with success (no attrs needed).
                if req.get_attribute::<MessageIntegrity>().is_none() {
                    // Would normally 401 — for simplicity in test just drop
                    continue;
                }
                let resp = Message::<TurnAttribute>::new(
                    MessageClass::SuccessResponse,
                    stun_codec::rfc5766::methods::CREATE_PERMISSION,
                    req.transaction_id(),
                );
                let mut enc = MessageEncoder::<TurnAttribute>::new();
                let bytes = enc.encode_into_bytes(resp).unwrap();
                let _ = socket.send_to(&bytes, src).await;
            }
        }
    });
    (addr, relayed)
}

#[tokio::test]
async fn allocate_survives_401_retry() {
    let (server_addr, expected_relayed) = spawn_mock_turn("u", "p").await;
    let cfg = TurnConfig {
        server_addr,
        username: "u".into(),
        password: "p".into(),
        requested_lifetime: Duration::from_secs(600),
    };
    let socket = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let mut client = TurnClient::new(socket, cfg);
    let relayed = client
        .allocate(Duration::from_secs(3))
        .await
        .expect("allocate should succeed after 401 retry");
    assert_eq!(relayed, expected_relayed);
    assert!(client.realm().is_some());
    assert!(client.nonce().is_some());
}

#[tokio::test]
async fn create_permission_after_allocate() {
    let (server_addr, _) = spawn_mock_turn("u", "p").await;
    let cfg = TurnConfig {
        server_addr,
        username: "u".into(),
        password: "p".into(),
        requested_lifetime: Duration::from_secs(600),
    };
    let socket = std::sync::Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let mut client = TurnClient::new(socket, cfg);
    client.allocate(Duration::from_secs(3)).await.unwrap();

    let peer: SocketAddr = "198.51.100.77:33000".parse().unwrap();
    client
        .create_permission(peer, Duration::from_secs(3))
        .await
        .expect("permission ok");
}
