//! Lightweight online-probe request: asks the signaling server which of the
//! given host IDs are currently registered (online). Requires no active
//! session — the connection is made, used, and closed in one shot.

use crate::error::SignalingError;
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::{ClientMessage, ServerMessage};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Connect to `url`, send `ProbeHosts { host_ids }`, return the subset that
/// are currently online. The WebSocket connection is closed after the reply.
pub async fn probe_hosts(url: &Url, host_ids: Vec<String>) -> Result<Vec<String>, SignalingError> {
    let (mut ws, _) = timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    .map_err(|_| SignalingError::Timeout { stage: "connect" })??;

    let req = ClientMessage::ProbeHosts { host_ids };
    ws.send(Message::Text(serde_json::to_string(&req)?)).await?;

    let frame = timeout(REPLY_TIMEOUT, ws.next())
        .await
        .map_err(|_| SignalingError::Timeout {
            stage: "probe_result",
        })?
        .ok_or_else(|| SignalingError::Protocol("connection closed before ProbeResult".into()))??;

    let reply: ServerMessage = match frame {
        Message::Text(t) => serde_json::from_str(&t)?,
        other => {
            return Err(SignalingError::Protocol(format!(
                "non-text frame: {other:?}"
            )))
        }
    };

    let _ = ws.close(None).await;

    match reply {
        ServerMessage::ProbeResult { online } => Ok(online),
        other => Err(SignalingError::Protocol(format!(
            "expected ProbeResult, got {other:?}"
        ))),
    }
}
