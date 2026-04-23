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
