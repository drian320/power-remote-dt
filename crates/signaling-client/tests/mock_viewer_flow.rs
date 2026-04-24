use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn viewer_rendezvous_gets_host_addr_and_pubkey() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Host: raw WS that registers and replies with its candidate when session_start arrives.
    let host_task = tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();

        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(),
            pubkey_b64: "HPK".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await.unwrap(); // Registered

        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };

        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40010, priority: PRIORITY_HOST },
        }).unwrap())).await.unwrap();

        // Keep the WS alive briefly so the server can deliver.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    // Give the mock host time to register before the viewer sends Connect.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local_udp: SocketAddr = "127.0.0.1:40011".parse().unwrap();
    let outcome = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(5), stun_url: None, aggregation_window: std::time::Duration::from_millis(100) },
        local_udp,
    ).await.unwrap();
    host_task.await.unwrap();

    let peer_addr = outcome.peer_candidates.iter()
        .find(|c| c.typ == prdt_signaling_proto::CandidateType::Host)
        .and_then(|c| format!("{}:{}", c.ip, c.port).parse::<std::net::SocketAddr>().ok())
        .expect("no host candidate in peer_candidates");
    assert_eq!(peer_addr.port(), 40010);
    assert_eq!(outcome.peer_pubkey_b64.as_deref(), Some("HPK"));
}
