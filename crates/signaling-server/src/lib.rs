pub mod state;
pub mod ws;

use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;
use std::time::Duration;

pub use state::{ServerState, SharedState};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub session_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { session_timeout: Duration::from_millis(60_000) }
    }
}

pub fn router(state: SharedState, _cfg: ServerConfig) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(state)
}

async fn health(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let (hosts, sessions) = state.counts();
    Json(json!({ "hosts": hosts, "sessions": sessions }))
}
