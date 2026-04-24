use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig, SignalingError};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn bad_candidate_parse_error() {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Host replies with unparseable IP.
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(), pubkey_b64: "P".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await;
        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid,
            candidate: Candidate { typ: CandidateType::Host, ip: "not-an-ip".into(), port: 1, priority: 100 },
        }).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    // Give the mock host time to register before viewer tries to connect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50100".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(2), stun_url: None },
        local,
    ).await.unwrap_err();
    match err {
        SignalingError::BadCandidate(msg) => assert!(msg.contains("not-an-ip")),
        other => panic!("unexpected: {other:?}"),
    }
}
