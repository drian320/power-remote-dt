//! AC-9: offline-first provisioning registration is idempotent by key.
//!
//! `register_host` is the register-only round trip used by provisioning (no
//! viewer wait). Re-registering the same key must return the *same*
//! server-allocated 9-digit ID — whether the client resends its persisted ID or
//! comes back with an empty ID after losing its local record (the server's
//! reverse-lookup-by-pubkey recovers it).

use prdt_crypto::KeyPair;
use prdt_signaling_client::register_host;
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

async fn spawn_signaling() -> Url {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("ws://{addr}/signal").parse().unwrap()
}

const TIMEOUT: Duration = Duration::from_secs(5);

/// Let the server's per-connection task remove the just-closed host from its
/// in-memory live map before the next Register (which rejects an ID that is
/// still live). Provisioning closes immediately after `Registered`, so the
/// durable SQLite row remains while the live entry drains.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resending_persisted_id_returns_same_id() {
    let url = spawn_signaling().await;
    let kp = KeyPair::generate();
    let pubkey = kp.public.to_base64();

    // First launch: empty ID → server allocates a fresh 9-digit dashed ID.
    let id1 = register_host(&url, "", &pubkey, TIMEOUT).await.unwrap();
    assert_eq!(id1.len(), 11, "expected 9 digits + 2 dashes, got {id1:?}");
    settle().await;

    // Subsequent launch resends the persisted ID + key → same ID, no new alloc.
    let id2 = register_host(&url, &id1, &pubkey, TIMEOUT).await.unwrap();
    assert_eq!(id1, id2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reallocating_with_same_key_recovers_id() {
    let url = spawn_signaling().await;
    let kp = KeyPair::generate();
    let pubkey = kp.public.to_base64();

    // Provisioned once...
    let id1 = register_host(&url, "", &pubkey, TIMEOUT).await.unwrap();
    settle().await;

    // ...then the local record is lost, so the client comes back with an empty
    // ID. Reverse-lookup-by-pubkey must recover the original ID, not mint a new
    // one.
    let recovered = register_host(&url, "", &pubkey, TIMEOUT).await.unwrap();
    assert_eq!(id1, recovered, "same key must recover the same id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distinct_keys_get_distinct_ids() {
    let url = spawn_signaling().await;
    let a = KeyPair::generate().public.to_base64();
    let b = KeyPair::generate().public.to_base64();

    let id_a = register_host(&url, "", &a, TIMEOUT).await.unwrap();
    settle().await;
    let id_b = register_host(&url, "", &b, TIMEOUT).await.unwrap();
    assert_ne!(id_a, id_b, "different devices must get different ids");
}
