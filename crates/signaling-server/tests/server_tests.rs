use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::sync::Arc;

#[tokio::test]
async fn health_endpoint_returns_counts() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // small yield to let the server come up
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let body = reqwest::get(format!("http://{addr}/health")).await.unwrap().text().await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["hosts"], 0);
    assert_eq!(v["sessions"], 0);
}

use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::{ClientMessage, ServerMessage};
use tokio_tungstenite::tungstenite::Message;

async fn start_test_server() -> (std::net::SocketAddr, Arc<ServerState>) {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, state)
}

fn ws_url(addr: std::net::SocketAddr) -> String {
    format!("ws://{addr}/signal")
}

async fn ws_send(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    msg: ClientMessage,
) {
    let s = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(s)).await.unwrap();
}

async fn ws_recv(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> ServerMessage {
    let frame = ws.next().await.unwrap().unwrap();
    let text = match frame {
        Message::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn register_gets_ack() {
    let (addr, state) = start_test_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();

    ws_send(&mut ws, ClientMessage::Register {
        host_id: "h1".into(),
        pubkey_b64: "AAA".into(),
    }).await;

    let msg = ws_recv(&mut ws).await;
    assert!(matches!(msg, ServerMessage::Registered { host_id } if host_id == "h1"));

    // state should have 1 host
    assert_eq!(state.counts().0, 1);
}

#[tokio::test]
async fn duplicate_register_rejected() {
    let (addr, _state) = start_test_server().await;

    let (mut ws1, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws1, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "A".into() }).await;
    let _ = ws_recv(&mut ws1).await;

    let (mut ws2, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws2, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "B".into() }).await;

    let msg = ws_recv(&mut ws2).await;
    match msg {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::HostAlreadyRegistered);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn connect_triggers_session_start_on_both_sides() {
    let (addr, _) = start_test_server().await;

    // host registers
    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register {
        host_id: "h1".into(),
        pubkey_b64: "PUBKEY".into(),
    }).await;
    let _ = ws_recv(&mut host_ws).await;

    // viewer connects
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;

    let host_start = ws_recv(&mut host_ws).await;
    let viewer_start = ws_recv(&mut viewer_ws).await;

    let (host_sid, viewer_sid) = match (host_start, viewer_start) {
        (
            ServerMessage::SessionStart { session_id: h, role: prdt_signaling_proto::Role::Host, peer_pubkey_b64: None },
            ServerMessage::SessionStart { session_id: v, role: prdt_signaling_proto::Role::Viewer, peer_pubkey_b64: Some(pk) },
        ) => {
            assert_eq!(pk, "PUBKEY");
            (h, v)
        }
        (h, v) => panic!("unexpected fan-out: host={h:?} viewer={v:?}"),
    };
    assert_eq!(host_sid, viewer_sid, "both sides must see the same session_id");
}

#[tokio::test]
async fn connect_unknown_host_returns_error() {
    let (addr, _) = start_test_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws, ClientMessage::Connect { host_id: "ghost".into() }).await;
    let msg = ws_recv(&mut ws).await;
    assert!(matches!(msg, ServerMessage::Error { code, .. } if code == prdt_signaling_proto::ErrorCode::HostNotFound));
}

use prdt_signaling_proto::{Candidate, CandidateType, PRIORITY_HOST};

#[tokio::test]
async fn candidate_forwarded_both_ways() {
    let (addr, _) = start_test_server().await;

    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;

    let h_start = ws_recv(&mut host_ws).await;
    let v_start = ws_recv(&mut viewer_ws).await;
    let sid = match h_start {
        ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };
    let _ = v_start;

    // viewer sends its candidate
    ws_send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 60001, priority: PRIORITY_HOST },
    }).await;

    // host should receive PeerCandidate
    let m = ws_recv(&mut host_ws).await;
    match m {
        ServerMessage::PeerCandidate { session_id, candidate } => {
            assert_eq!(session_id, sid);
            assert_eq!(candidate.port, 60001);
        }
        other => panic!("unexpected: {other:?}"),
    }

    // host sends its candidate
    ws_send(&mut host_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 60002, priority: PRIORITY_HOST },
    }).await;

    let m = ws_recv(&mut viewer_ws).await;
    match m {
        ServerMessage::PeerCandidate { session_id, candidate } => {
            assert_eq!(session_id, sid);
            assert_eq!(candidate.port, 60002);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn non_host_candidate_type_rejected() {
    let (addr, _) = start_test_server().await;
    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;
    let h_start = ws_recv(&mut host_ws).await;
    let _ = ws_recv(&mut viewer_ws).await;
    let sid = match h_start {
        ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };
    ws_send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid,
        candidate: Candidate { typ: CandidateType::Srflx, ip: "1.2.3.4".into(), port: 1, priority: 50 },
    }).await;
    let err = ws_recv(&mut viewer_ws).await;
    match err {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::UnsupportedCandidateType);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
