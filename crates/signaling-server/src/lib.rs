pub mod host_store;
pub mod state;
pub mod ws;

pub use host_store::{HostStore, StoreError};

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
        Self {
            session_timeout: Duration::from_millis(60_000),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub state: SharedState,
    pub cfg: ServerConfig,
}

pub fn router(state: SharedState, cfg: ServerConfig) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/signal", get(ws::handle_upgrade))
        .with_state(AppState { state, cfg })
}

async fn health(State(app): State<AppState>) -> Json<serde_json::Value> {
    let (hosts, sessions) = app.state.counts();
    Json(json!({ "hosts": hosts, "sessions": sessions }))
}
