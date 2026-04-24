use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::*;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, instrument};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTERED_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_CANDIDATE_TIMEOUT: Duration = Duration::from_secs(5);

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

    send_msg(&mut ws, &ClientMessage::Candidate {
        session_id: session_id.clone(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    let peer = match recv_msg(&mut ws, "peer_candidate", PEER_CANDIDATE_TIMEOUT).await? {
        ServerMessage::PeerCandidate { candidate, .. } => {
            if candidate.typ != CandidateType::Host {
                return Err(SignalingError::BadCandidate(format!("unsupported typ {:?}", candidate.typ)));
            }
            let s = format!("{}:{}", candidate.ip, candidate.port);
            s.parse::<SocketAddr>()
                .map_err(|e| SignalingError::BadCandidate(format!("{e}: {s}")))?
        }
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected PeerCandidate, got {other:?}"))),
    };

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome { session_id, peer_addr: peer, peer_pubkey_b64: None, peer_candidates: vec![] })
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

    send_msg(&mut ws, &ClientMessage::Candidate {
        session_id: session_id.clone(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    let peer = match recv_msg(&mut ws, "peer_candidate", PEER_CANDIDATE_TIMEOUT).await? {
        ServerMessage::PeerCandidate { candidate, .. } => {
            if candidate.typ != CandidateType::Host {
                return Err(SignalingError::BadCandidate(format!("unsupported typ {:?}", candidate.typ)));
            }
            let s = format!("{}:{}", candidate.ip, candidate.port);
            s.parse::<SocketAddr>()
                .map_err(|e| SignalingError::BadCandidate(format!("{e}: {s}")))?
        }
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected PeerCandidate, got {other:?}"))),
    };

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome { session_id, peer_addr: peer, peer_pubkey_b64, peer_candidates: vec![] })
}
