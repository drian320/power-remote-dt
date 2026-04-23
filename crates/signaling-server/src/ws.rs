use crate::state::{HostEntry, SharedState};
use crate::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use prdt_signaling_proto::{ClientMessage, ErrorCode, ServerMessage};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, warn};

const SEND_CHAN_CAP: usize = 32;

pub async fn handle_upgrade(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, app))
}

async fn handle_socket(mut socket: WebSocket, app: AppState) {
    let state = app.state.clone();

    // Wait for the first message to classify role.
    let first = match socket.recv().await {
        Some(Ok(Message::Text(t))) => t,
        _ => return,
    };
    let msg: Result<ClientMessage, _> = serde_json::from_str(&first);
    let classified = match msg {
        Ok(m) => m,
        Err(e) => {
            send_error(&mut socket, ErrorCode::ProtocolError, &format!("bad first message: {e}")).await;
            return;
        }
    };

    match classified {
        ClientMessage::Register { host_id, pubkey_b64 } => {
            if state.hosts.contains_key(&host_id) {
                send_error(&mut socket, ErrorCode::HostAlreadyRegistered, "host_id already in use").await;
                return;
            }
            let (tx, rx) = mpsc::channel::<ServerMessage>(SEND_CHAN_CAP);
            state.hosts.insert(host_id.clone(), HostEntry {
                pubkey_b64,
                tx: tx.clone(),
                registered_at: Instant::now(),
            });
            info!(host_id = %host_id, "register");
            if send_message(&mut socket, &ServerMessage::Registered { host_id: host_id.clone() })
                .await.is_err()
            {
                state.hosts.remove(&host_id);
                return;
            }
            host_loop(socket, state, host_id, rx).await;
        }
        _other => {
            // Connect / other — implemented in later tasks.
            send_error(&mut socket, ErrorCode::ProtocolError, "not yet implemented").await;
        }
    }
}

async fn host_loop(
    mut socket: WebSocket,
    state: SharedState,
    host_id: String,
    mut rx: mpsc::Receiver<ServerMessage>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(_))) => {
                        // Candidate / Done handling lands in Tasks 7-8.
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore binary / ping etc
                    Some(Err(e)) => { warn!(error = %e, "host ws error"); break }
                }
            }
            outbound = rx.recv() => {
                let Some(m) = outbound else { break };
                if send_message(&mut socket, &m).await.is_err() { break; }
            }
        }
    }
    state.hosts.remove(&host_id);
    info!(host_id = %host_id, "host_disconnected");
}

pub(crate) async fn send_message(socket: &mut WebSocket, m: &ServerMessage) -> Result<(), ()> {
    let s = serde_json::to_string(m).map_err(|_| ())?;
    socket.send(Message::Text(s)).await.map_err(|_| ())
}

pub(crate) async fn send_error(socket: &mut WebSocket, code: ErrorCode, message: &str) {
    let _ = send_message(socket, &ServerMessage::Error { code, message: message.into() }).await;
}
