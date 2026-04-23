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
