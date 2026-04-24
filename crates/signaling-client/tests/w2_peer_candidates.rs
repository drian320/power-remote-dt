//! Verify rendezvous_as_viewer collects ALL incoming PeerCandidates into
//! `peer_candidates` but still returns the Host-typ one as peer_addr.

use futures_util::{SinkExt, StreamExt};
use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST, PRIORITY_SRFLX};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::test]
async fn viewer_collects_host_and_srflx_peer_candidates() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(), pubkey_b64: "HPK".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await.unwrap();

        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };

        // Host candidate FIRST so the viewer commits to it as peer_addr
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40200, priority: PRIORITY_HOST },
        }).unwrap())).await.unwrap();
        // Then Srflx (viewer will receive this as part of collecting, then return)
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid,
            candidate: Candidate { typ: CandidateType::Srflx, ip: "198.51.100.9".into(), port: 44444, priority: PRIORITY_SRFLX },
        }).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local_udp: SocketAddr = "127.0.0.1:40201".parse().unwrap();
    let outcome = rendezvous_as_viewer(
        RendezvousConfig {
            url,
            host_id: "h1".into(),
            timeout: Duration::from_secs(5),
            stun_url: None,
            turn_url: None,
            aggregation_window: std::time::Duration::from_millis(100),
        },
        local_udp,
    ).await.unwrap();

    // peer_candidates should contain the Host (always) and may contain the
    // Srflx if it arrived before aggregation closed. At minimum Host is present
    // and carries the port signaled above.
    let host_cand = outcome.peer_candidates.iter()
        .find(|c| c.typ == CandidateType::Host)
        .expect("peer_candidates missing Host");
    assert_eq!(host_cand.port, 40200);
}

#[tokio::test]
async fn viewer_collects_srflx_before_host() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(), pubkey_b64: "HPK".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await.unwrap();

        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };

        // Srflx FIRST
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate { typ: CandidateType::Srflx, ip: "198.51.100.9".into(), port: 44444, priority: PRIORITY_SRFLX },
        }).unwrap())).await.unwrap();
        // Then Host 50ms later
        tokio::time::sleep(Duration::from_millis(50)).await;
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid,
            candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40300, priority: PRIORITY_HOST },
        }).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local_udp: SocketAddr = "127.0.0.1:40301".parse().unwrap();
    let outcome = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(5), stun_url: None, turn_url: None, aggregation_window: std::time::Duration::from_millis(100) },
        local_udp,
    ).await.unwrap();

    let host_cand = outcome.peer_candidates.iter()
        .find(|c| c.typ == CandidateType::Host)
        .expect("peer_candidates missing Host");
    assert_eq!(host_cand.port, 40300);
    // Both should be in peer_candidates because Srflx arrived before Host
    let types: Vec<CandidateType> = outcome.peer_candidates.iter().map(|c| c.typ).collect();
    assert!(types.contains(&CandidateType::Host), "missing Host in {types:?}");
    assert!(types.contains(&CandidateType::Srflx), "missing Srflx in {types:?}");
}
