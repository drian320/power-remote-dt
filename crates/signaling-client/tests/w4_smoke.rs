//! W4 smoke: when turn_url is set, rendezvous_as_host emits a Relay candidate
//! via signaling, and a viewer observer receives it.
//!
//! The mock TURN server only implements Allocate (enough to produce a relayed
//! addr for the Relay candidate). Probe-over-TURN and Noise-over-TURN are
//! future work.

use bytecodec::{DecodeExt, EncodeExt};
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_client::{rendezvous_as_host, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::{CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

use prdt_nat_traversal::TurnAttribute;
use stun_codec::rfc5389::attributes::{ErrorCode, MessageIntegrity, Nonce, Realm, Username};
use stun_codec::rfc5766::attributes::{Lifetime, XorRelayAddress};
use stun_codec::rfc5766::methods::ALLOCATE;
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder};

const REALM: &str = "prdt-test";
const NONCE: &str = "01234567";

async fn spawn_mock_turn(username: &'static str, relayed: SocketAddr) -> SocketAddr {
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
            if req.method() != ALLOCATE || req.class() != MessageClass::Request {
                continue;
            }
            let has_mi = req.get_attribute::<MessageIntegrity>().is_some();
            if !has_mi {
                let mut resp = Message::<TurnAttribute>::new(
                    MessageClass::ErrorResponse,
                    ALLOCATE,
                    req.transaction_id(),
                );
                resp.add_attribute(TurnAttribute::from(
                    ErrorCode::new(401, "Unauthorized".into()).unwrap(),
                ));
                resp.add_attribute(TurnAttribute::from(Realm::new(REALM.into()).unwrap()));
                resp.add_attribute(TurnAttribute::from(Nonce::new(NONCE.into()).unwrap()));
                let mut enc = MessageEncoder::<TurnAttribute>::new();
                let bytes = enc.encode_into_bytes(resp).unwrap();
                let _ = socket.send_to(&bytes, src).await;
            } else {
                if req
                    .get_attribute::<Username>()
                    .map(|u| u.name().to_string())
                    != Some(username.to_string())
                {
                    continue;
                }
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
        }
    });
    addr
}

async fn spawn_signaling() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn relay_candidate_flows_through_signaling() {
    let sig_addr = spawn_signaling().await;
    let relayed_for_host: SocketAddr = "198.51.100.50:50050".parse().unwrap();
    let host_turn_addr = spawn_mock_turn("u", relayed_for_host).await;

    let sig_url: Url = format!("ws://{sig_addr}/signal").parse().unwrap();
    let turn_url: Url = format!("turn://u:p@{host_turn_addr}").parse().unwrap();

    let host_task = tokio::spawn({
        let sig = sig_url.clone();
        let turn = turn_url.clone();
        async move {
            rendezvous_as_host(
                RendezvousConfig {
                    url: sig,
                    host_id: "w4".into(),
                    timeout: Duration::from_secs(5),
                    stun_url: None,
                    turn_url: Some(turn),
                    aggregation_window: Duration::from_millis(300),
                },
                HostIdentity {
                    pubkey_b64: "HPK".into(),
                },
                "127.0.0.1:40400".parse().unwrap(),
            )
            .await
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(format!("ws://{sig_addr}/signal"))
        .await
        .unwrap();
    viewer_ws
        .send(WsMessage::Text(
            serde_json::to_string(&ClientMessage::Connect {
                host_id: "w4".into(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();

    let _ = viewer_ws.next().await.unwrap().unwrap(); // SessionStart

    let mut saw_host = false;
    let mut saw_relay_from_turn = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && (!saw_host || !saw_relay_from_turn) {
        let frame =
            match tokio::time::timeout(deadline - tokio::time::Instant::now(), viewer_ws.next())
                .await
            {
                Ok(Some(Ok(f))) => f,
                _ => break,
            };
        let t = match frame {
            WsMessage::Text(s) => s,
            _ => continue,
        };
        let m: prdt_signaling_proto::ServerMessage = match serde_json::from_str(&t) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if let prdt_signaling_proto::ServerMessage::PeerCandidate { candidate, .. } = m {
            match candidate.typ {
                CandidateType::Host => {
                    assert_eq!(candidate.priority, PRIORITY_HOST);
                    saw_host = true;
                }
                CandidateType::Relay => {
                    assert_eq!(candidate.ip, "198.51.100.50");
                    assert_eq!(candidate.port, 50050);
                    saw_relay_from_turn = true;
                }
                CandidateType::Srflx => {}
            }
        }
    }

    host_task.abort();
    let _ = host_task.await;

    assert!(saw_host, "Host candidate missing");
    assert!(
        saw_relay_from_turn,
        "Relay candidate (from TURN allocate) missing"
    );
}
