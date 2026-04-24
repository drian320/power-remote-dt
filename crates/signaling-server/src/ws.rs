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
        ClientMessage::Connect { host_id } => {
            let (viewer_tx, viewer_rx) = mpsc::channel::<ServerMessage>(SEND_CHAN_CAP);
            let (host_tx, pubkey_b64) = match state.hosts.get(&host_id) {
                Some(entry) => (entry.tx.clone(), entry.pubkey_b64.clone()),
                None => {
                    send_error(&mut socket, ErrorCode::HostNotFound, "no such host_id").await;
                    return;
                }
            };
            let session_id = uuid::Uuid::new_v4().to_string();
            state.sessions.insert(session_id.clone(), crate::state::SessionEntry {
                host_id: host_id.clone(),
                host_tx: host_tx.clone(),
                viewer_tx: viewer_tx.clone(),
                created_at: Instant::now(),
            });
            info!(host_id = %host_id, session_id = %session_id, "connect");

            let _ = host_tx.send(ServerMessage::SessionStart {
                session_id: session_id.clone(),
                role: prdt_signaling_proto::Role::Host,
                peer_pubkey_b64: None,
            }).await;
            let _ = viewer_tx.send(ServerMessage::SessionStart {
                session_id: session_id.clone(),
                role: prdt_signaling_proto::Role::Viewer,
                peer_pubkey_b64: Some(pubkey_b64),
            }).await;

            let timeout_state = state.clone();
            let timeout_sid = session_id.clone();
            let timeout_host_tx = host_tx.clone();
            let timeout_viewer_tx = viewer_tx.clone();
            let session_timeout = app.cfg.session_timeout;
            tokio::spawn(async move {
                tokio::time::sleep(session_timeout).await;
                if timeout_state.sessions.remove(&timeout_sid).is_some() {
                    let _ = timeout_host_tx.send(ServerMessage::Error {
                        code: ErrorCode::InternalError,
                        message: "session timeout".into(),
                    }).await;
                    let _ = timeout_viewer_tx.send(ServerMessage::Error {
                        code: ErrorCode::InternalError,
                        message: "session timeout".into(),
                    }).await;
                    tracing::info!(session_id = %timeout_sid, "session_timeout");
                }
            });

            viewer_loop(socket, state, session_id, viewer_rx).await;
        }
        _ => {
            send_error(&mut socket, ErrorCode::ProtocolError, "first message must be register or connect").await;
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
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<ClientMessage>(&t) {
                            Ok(ClientMessage::Candidate { session_id, candidate }) => {
                                // W4: Host, Srflx, and Relay all forwarded.
                                if let Some(sess) = state.sessions.get(&session_id) {
                                    let _ = sess.viewer_tx.send(ServerMessage::PeerCandidate {
                                        session_id: session_id.clone(),
                                        candidate,
                                    }).await;
                                }
                            }
                            Ok(ClientMessage::Done { session_id, .. }) => {
                                state.sessions.remove(&session_id);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                send_error(&mut socket, ErrorCode::ProtocolError, &format!("{e}")).await;
                                break;
                            }
                        }
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

async fn viewer_loop(
    mut socket: WebSocket,
    state: SharedState,
    session_id: String,
    mut rx: mpsc::Receiver<ServerMessage>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<ClientMessage>(&t) {
                            Ok(ClientMessage::Candidate { session_id: sid, candidate }) => {
                                // W4: Host, Srflx, and Relay all forwarded.
                                if let Some(sess) = state.sessions.get(&sid) {
                                    let _ = sess.host_tx.send(ServerMessage::PeerCandidate {
                                        session_id: sid.clone(),
                                        candidate,
                                    }).await;
                                }
                            }
                            Ok(ClientMessage::Done { .. }) => {
                                state.sessions.remove(&session_id);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                send_error(&mut socket, ErrorCode::ProtocolError, &format!("{e}")).await;
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore binary / ping etc
                    Some(Err(e)) => { warn!(error = %e, "viewer ws error"); break }
                }
            }
            outbound = rx.recv() => {
                let Some(m) = outbound else { break };
                if send_message(&mut socket, &m).await.is_err() { break; }
            }
        }
    }
    state.sessions.remove(&session_id);
    info!(session_id = %session_id, "viewer_disconnected");
}

pub(crate) async fn send_message(socket: &mut WebSocket, m: &ServerMessage) -> Result<(), ()> {
    let s = serde_json::to_string(m).map_err(|_| ())?;
    socket.send(Message::Text(s)).await.map_err(|_| ())
}

pub(crate) async fn send_error(socket: &mut WebSocket, code: ErrorCode, message: &str) {
    let _ = send_message(socket, &ServerMessage::Error { code, message: message.into() }).await;
}
