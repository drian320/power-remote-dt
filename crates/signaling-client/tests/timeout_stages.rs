use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig, SignalingError};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

async fn spawn_server() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(
        state,
        ServerConfig {
            session_timeout: Duration::from_millis(10_000),
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn viewer_session_start_timeout() {
    let addr = spawn_server().await;
    // No host registered → viewer will get HostNotFound error, not SessionStart.
    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50001".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig {
            url,
            host_id: "ghost".into(),
            timeout: Duration::from_millis(300),
            stun_url: None,
            turn_url: None,
            aggregation_window: std::time::Duration::from_millis(100),
        },
        local,
    )
    .await
    .unwrap_err();
    match err {
        SignalingError::Server { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::HostNotFound);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn viewer_peer_candidate_timeout() {
    let addr = spawn_server().await;
    // Register a host that will just hang (never send a candidate).
    let (mut host_ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal"))
        .await
        .unwrap();
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    host_ws
        .send(Message::Text(
            serde_json::to_string(&prdt_signaling_proto::ClientMessage::Register {
                host_id: "h1".into(),
                pubkey_b64: "P".into(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let _ = host_ws.next().await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50002".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig {
            url,
            host_id: "h1".into(),
            timeout: Duration::from_secs(1),
            stun_url: None,
            turn_url: None,
            aggregation_window: std::time::Duration::from_millis(100),
        },
        local,
    )
    .await
    .unwrap_err();
    match err {
        SignalingError::Timeout { stage } => assert_eq!(stage, "peer_candidate"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn connect_timeout_when_server_unreachable() {
    // port 1 is reserved; tokio should fail quickly or we bound the connect timeout via the library (5s).
    let url: Url = "ws://127.0.0.1:1/signal".parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50003".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig {
            url,
            host_id: "h1".into(),
            timeout: Duration::from_secs(1),
            stun_url: None,
            turn_url: None,
            aggregation_window: std::time::Duration::from_millis(100),
        },
        local,
    )
    .await
    .unwrap_err();
    // Either WebSocket connect error OR Timeout — both are acceptable signals; in CI we prefer the
    // explicit timeout so the assertion accepts both shapes.
    match err {
        SignalingError::Timeout { stage: "connect" } => {}
        SignalingError::WebSocket(_) => {}
        other => panic!("unexpected: {other:?}"),
    }
}
