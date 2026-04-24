//! Verify rendezvous_as_host sends BOTH Host and Srflx candidates when stun_url is given.

use bytecodec::{DecodeExt, EncodeExt};
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_client::{rendezvous_as_host, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder,
};
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_stun_mock(report_addr: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut dec = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = dec.decode_from_bytes(&buf[..n]) else { continue };
            if req.class() != MessageClass::Request || req.method() != BINDING {
                continue;
            }
            let mut resp = Message::new(MessageClass::SuccessResponse, BINDING, req.transaction_id());
            resp.add_attribute(Attribute::from(XorMappedAddress::new(report_addr)));
            let mut enc = MessageEncoder::<Attribute>::new();
            let bytes = enc.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
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
async fn host_sends_both_host_and_srflx_when_stun_url_given() {
    let sig_addr = spawn_signaling().await;
    let fake_public: SocketAddr = "198.51.100.7:55555".parse().unwrap();
    let stun_addr = spawn_stun_mock(fake_public).await;

    let sig_url: Url = format!("ws://{sig_addr}/signal").parse().unwrap();
    let stun_url: Url = format!("stun://{stun_addr}").parse().unwrap();

    let host_task = tokio::spawn(async move {
        rendezvous_as_host(
            RendezvousConfig {
                url: sig_url,
                host_id: "h1".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(stun_url),
            },
            HostIdentity { pubkey_b64: "HPK".into() },
            "127.0.0.1:40100".parse().unwrap(),
        ).await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(format!("ws://{sig_addr}/signal")).await.unwrap();
    viewer_ws.send(WsMessage::Text(serde_json::to_string(
        &ClientMessage::Connect { host_id: "h1".into() }
    ).unwrap())).await.unwrap();

    let _ = viewer_ws.next().await.unwrap().unwrap();

    let mut got_host = None::<Candidate>;
    let mut got_srflx = None::<Candidate>;
    for _ in 0..2 {
        let frame = viewer_ws.next().await.unwrap().unwrap();
        let t = match frame { WsMessage::Text(s) => s, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&t).unwrap();
        match m {
            prdt_signaling_proto::ServerMessage::PeerCandidate { candidate, .. } => match candidate.typ {
                CandidateType::Host => got_host = Some(candidate),
                CandidateType::Srflx => got_srflx = Some(candidate),
                _ => panic!("unexpected typ: {:?}", candidate.typ),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    host_task.abort();
    let _ = host_task.await;

    let host_cand = got_host.expect("Host candidate missing");
    assert_eq!(host_cand.ip, "127.0.0.1");
    assert_eq!(host_cand.port, 40100);
    assert_eq!(host_cand.priority, PRIORITY_HOST);

    let srflx_cand = got_srflx.expect("Srflx candidate missing");
    assert_eq!(srflx_cand.ip, "198.51.100.7");
    assert_eq!(srflx_cand.port, 55555);
}
