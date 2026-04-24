use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::*;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, instrument};

use prdt_signaling_proto::PRIORITY_SRFLX;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTERED_TIMEOUT: Duration = Duration::from_secs(5);

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn candidate_for(local: SocketAddr) -> Candidate {
    Candidate {
        typ: CandidateType::Host,
        ip: local.ip().to_string(),
        port: local.port(),
        priority: PRIORITY_HOST,
    }
}

async fn ws_connect(url: &url::Url) -> Result<Ws, SignalingError> {
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()))
        .await
        .map_err(|_| SignalingError::Timeout { stage: "connect" })??;
    Ok(ws)
}

async fn send_msg(ws: &mut Ws, m: &ClientMessage) -> Result<(), SignalingError> {
    let s = serde_json::to_string(m)?;
    ws.send(Message::Text(s)).await?;
    Ok(())
}

async fn recv_msg(ws: &mut Ws, stage: &'static str, dur: Duration) -> Result<ServerMessage, SignalingError> {
    let frame = timeout(dur, ws.next())
        .await
        .map_err(|_| SignalingError::Timeout { stage })?;
    let frame = frame
        .ok_or_else(|| SignalingError::Protocol("connection closed".into()))?
        .map_err(SignalingError::from)?;
    match frame {
        Message::Text(t) => Ok(serde_json::from_str(&t)?),
        other => Err(SignalingError::Protocol(format!("non-text frame: {other:?}"))),
    }
}

#[instrument(skip(cfg, identity), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_host(
    cfg: RendezvousConfig,
    identity: HostIdentity,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(&mut ws, &ClientMessage::Register {
        host_id: cfg.host_id.clone(),
        pubkey_b64: identity.pubkey_b64,
    }).await?;

    match recv_msg(&mut ws, "registered", REGISTERED_TIMEOUT).await? {
        ServerMessage::Registered { .. } => {}
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected Registered, got {other:?}"))),
    }

    let session_id = match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
        ServerMessage::SessionStart { session_id, role: Role::Host, .. } => session_id,
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected SessionStart, got {other:?}"))),
    };
    info!(%session_id, "session_start");

    send_candidates(&mut ws, &session_id, local_udp_addr, cfg.stun_url.as_ref()).await?;

    let peer_candidates = recv_peer_candidates(&mut ws, cfg.timeout, cfg.aggregation_window).await?;

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome {
        session_id,
        peer_pubkey_b64: None,
        peer_candidates,
    })
}

#[instrument(skip(cfg), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_viewer(
    cfg: RendezvousConfig,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(&mut ws, &ClientMessage::Connect { host_id: cfg.host_id.clone() }).await?;

    let (session_id, peer_pubkey_b64) = match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
        ServerMessage::SessionStart { session_id, role: Role::Viewer, peer_pubkey_b64 } => (session_id, peer_pubkey_b64),
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected SessionStart, got {other:?}"))),
    };
    info!(%session_id, "session_start");

    send_candidates(&mut ws, &session_id, local_udp_addr, cfg.stun_url.as_ref()).await?;

    let peer_candidates = recv_peer_candidates(&mut ws, cfg.timeout, cfg.aggregation_window).await?;

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome {
        session_id,
        peer_pubkey_b64,
        peer_candidates,
    })
}

async fn recv_peer_candidates(
    ws: &mut Ws,
    total_timeout: Duration,
    aggregation_window: Duration,
) -> Result<Vec<Candidate>, SignalingError> {
    let total_deadline = tokio::time::Instant::now() + total_timeout;
    let mut collected: Vec<Candidate> = Vec::new();
    let mut first_seen: Option<tokio::time::Instant> = None;
    loop {
        let effective_deadline = match first_seen {
            None => total_deadline,
            Some(t) => total_deadline.min(t + aggregation_window),
        };
        let remaining = effective_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match recv_msg(ws, "peer_candidate", remaining).await {
            Ok(ServerMessage::PeerCandidate { candidate, .. }) => {
                if first_seen.is_none() {
                    first_seen = Some(tokio::time::Instant::now());
                }
                collected.push(candidate);
            }
            Ok(ServerMessage::Error { code, message }) => {
                return Err(SignalingError::Server { code, message });
            }
            Ok(other) => {
                return Err(SignalingError::Protocol(format!(
                    "expected PeerCandidate, got {other:?}"
                )));
            }
            Err(SignalingError::Timeout { .. }) => break,
            Err(e) => return Err(e),
        }
    }
    if collected.is_empty() {
        return Err(SignalingError::Timeout { stage: "peer_candidate" });
    }
    Ok(collected)
}

async fn send_candidates(
    ws: &mut Ws,
    session_id: &str,
    local_udp_addr: SocketAddr,
    stun_url: Option<&url::Url>,
) -> Result<(), SignalingError> {
    send_msg(ws, &ClientMessage::Candidate {
        session_id: session_id.to_string(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    if let Some(url) = stun_url {
        match resolve_and_learn_srflx(url).await {
            Ok(srflx) => {
                send_msg(ws, &ClientMessage::Candidate {
                    session_id: session_id.to_string(),
                    candidate: Candidate {
                        typ: CandidateType::Srflx,
                        ip: srflx.ip().to_string(),
                        port: srflx.port(),
                        priority: PRIORITY_SRFLX,
                    },
                }).await?;
                tracing::info!(%srflx, "srflx candidate sent");
            }
            Err(e) => {
                tracing::warn!(error = %e, "STUN failed; proceeding without srflx candidate");
            }
        }
    }
    Ok(())
}

async fn resolve_and_learn_srflx(
    stun_url: &url::Url,
) -> Result<SocketAddr, SignalingError> {
    if stun_url.scheme() != "stun" {
        return Err(SignalingError::Protocol(format!(
            "unsupported stun URL scheme: {}",
            stun_url.scheme()
        )));
    }
    let host = stun_url
        .host_str()
        .ok_or_else(|| SignalingError::Protocol("stun URL missing host".into()))?;
    let port = stun_url.port().unwrap_or(3478);
    let stun_addr = tokio::net::lookup_host(format!("{host}:{port}"))
        .await
        .map_err(|e| SignalingError::Protocol(format!("resolve stun: {e}")))?
        .next()
        .ok_or_else(|| SignalingError::Protocol("no addrs for stun host".into()))?;

    // Separate UDP socket for STUN (W2 limitation — see spec Open Questions).
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let addr = prdt_nat_traversal::learn_public_addr(
        &probe,
        stun_addr,
        std::time::Duration::from_secs(3),
    )
    .await
    .map_err(|e| SignalingError::Protocol(format!("stun: {e}")))?;
    Ok(addr)
}
