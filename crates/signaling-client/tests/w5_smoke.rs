//! W5 E2E: empty-register → allocated ID → re-register with same pubkey →
//! reuses ID → re-register with different pubkey → HostIdPubkeyMismatch.

use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::{ClientMessage, ErrorCode, ServerMessage};
use prdt_signaling_server::{router, HostStore, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

async fn spawn_signaling_with_store(store: Arc<HostStore>) -> SocketAddr {
    let state = Arc::new(ServerState::with_store(store));
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn register_raw(addr: SocketAddr, host_id: &str, pubkey_b64: &str) -> ServerMessage {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal"))
        .await
        .unwrap();
    let msg = ClientMessage::Register {
        host_id: host_id.into(),
        pubkey_b64: pubkey_b64.into(),
    };
    ws.send(WsMessage::Text(serde_json::to_string(&msg).unwrap()))
        .await
        .unwrap();
    let frame = ws.next().await.unwrap().unwrap();
    let t = match frame {
        WsMessage::Text(s) => s,
        o => panic!("{o:?}"),
    };
    serde_json::from_str(&t).unwrap()
}

#[tokio::test]
async fn empty_register_allocates_id_and_reuses_on_same_pubkey() {
    let store = Arc::new(HostStore::open_in_memory().unwrap());
    let addr = spawn_signaling_with_store(store).await;

    let m = register_raw(addr, "", "PK1").await;
    let allocated = match m {
        ServerMessage::Registered { host_id } => host_id,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(allocated.len(), 11, "expected dashed 9-digit: {allocated}");

    // Allow brief time for the first register's WS close to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    let m = register_raw(addr, &allocated, "PK1").await;
    match m {
        ServerMessage::Registered { host_id } => assert_eq!(host_id, allocated),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn re_register_with_different_pubkey_is_mismatch() {
    let store = Arc::new(HostStore::open_in_memory().unwrap());
    let addr = spawn_signaling_with_store(store).await;

    let m = register_raw(addr, "", "PK1").await;
    let allocated = match m {
        ServerMessage::Registered { host_id } => host_id,
        other => panic!("unexpected: {other:?}"),
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    let m = register_raw(addr, &allocated, "PK-OTHER").await;
    match m {
        ServerMessage::Error { code, .. } => assert_eq!(code, ErrorCode::HostIdPubkeyMismatch),
        other => panic!("unexpected: {other:?}"),
    }
}
