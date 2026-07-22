use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::*;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, instrument};

use prdt_signaling_proto::PRIORITY_RELAY;
use prdt_signaling_proto::PRIORITY_SRFLX;
use std::sync::Arc;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTERED_TIMEOUT: Duration = Duration::from_secs(5);

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn candidate_for(local: SocketAddr) -> Candidate {
    Candidate {
        typ: CandidateType::Host,
        ip: local.ip().to_string(),
        port: local.port(),
        priority: PRIORITY_HOST,
    }
}

/// Whether an interface IP is a usable Host ICE candidate.
///
/// Rejects loopback (127.0.0.0/8), link-local / APIPA (169.254.0.0/16),
/// unspecified (0.0.0.0), and the broadcast address — none of which a remote
/// peer can reach. IPv6 host candidates are intentionally not emitted for now
/// (the LANs in use are IPv4); revisit when IPv6 signaling is exercised.
fn is_usable_host_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified() && !v4.is_broadcast()
        }
        IpAddr::V6(_) => false,
    }
}

/// Decide which Host ICE candidates to advertise for a given UDP bind address.
///
/// Pure: takes the socket's bind addr plus a slice of already-enumerated local
/// interface IPs and returns the candidates to send. The interface enumeration
/// itself (a side effect) happens at the call site so this stays unit-testable.
///
/// - Concrete bind (e.g. `192.168.1.50:9000`): advertise exactly that address,
///   ignoring `local_ips` — this is the historical behavior and the peer must
///   reach precisely that IP.
/// - Wildcard bind (`0.0.0.0` / `[::]`, the default): the wildcard itself is
///   unroutable — a peer that probes `0.0.0.0` gets EADDRNOTAVAIL and the
///   handshake times out. So advertise one Host candidate per usable interface
///   IP from `local_ips`, each carrying the socket's real port. May be empty
///   (caller then applies a fallback).
fn host_candidates(local: SocketAddr, local_ips: &[IpAddr]) -> Vec<Candidate> {
    if !local.ip().is_unspecified() {
        return vec![candidate_for(local)];
    }
    local_ips
        .iter()
        .filter(|ip| is_usable_host_ip(ip))
        .map(|ip| Candidate {
            typ: CandidateType::Host,
            ip: ip.to_string(),
            port: local.port(),
            priority: PRIORITY_HOST,
        })
        .collect()
}

/// Enumerate the machine's local interface IPs for Host candidate gathering.
///
/// Primary source is `if_addrs::get_if_addrs()`. If that yields nothing usable
/// (enumeration failed, or every address is loopback/link-local), fall back to
/// the zero-dependency default-route trick via [`default_route_source_ip`].
fn gather_local_ips() -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = match if_addrs::get_if_addrs() {
        Ok(addrs) => addrs.into_iter().map(|a| a.ip()).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "if_addrs enumeration failed; falling back to default route");
            Vec::new()
        }
    };
    if !ips.iter().any(is_usable_host_ip) {
        if let Some(ip) = default_route_source_ip() {
            ips.push(ip);
        }
    }
    ips
}

/// Resolve the machine's primary source IP by asking the OS which local address
/// it would use to reach an arbitrary off-link destination.
///
/// No packets are sent: `connect` on a UDP socket only sets the default peer
/// and makes the kernel pick a source address, which `local_addr` then reports.
/// `192.0.2.1` is TEST-NET-1 (RFC 5737) — reserved for documentation and
/// guaranteed not to be a live host.
fn default_route_source_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("192.0.2.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    is_usable_host_ip(&ip).then_some(ip)
}

async fn ws_connect(url: &url::Url) -> Result<Ws, SignalingError> {
    let url = normalize_signaling_url(url);
    let (ws, _) = timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    .map_err(|_| SignalingError::Timeout { stage: "connect" })??;
    Ok(ws)
}

/// Auto-complete a signaling URL that omits the WebSocket path. The server
/// exposes the signaling endpoint at `/signal`, but the client connects to the
/// configured URL verbatim — so a user who enters just `ws://host:8080` would
/// hit `/` and get a 404. When the URL carries no path (or only `/`), default
/// it to `/signal`; an explicit path (e.g. a reverse-proxy prefix like
/// `/proxy/signal`) is left untouched.
fn normalize_signaling_url(url: &url::Url) -> url::Url {
    let mut u = url.clone();
    if u.path().is_empty() || u.path() == "/" {
        u.set_path("/signal");
    }
    u
}

#[cfg(test)]
mod url_norm_tests {
    use super::normalize_signaling_url;

    fn norm(s: &str) -> String {
        normalize_signaling_url(&url::Url::parse(s).unwrap()).to_string()
    }

    #[test]
    fn appends_signal_when_path_missing() {
        assert_eq!(norm("ws://host:8080"), "ws://host:8080/signal");
        assert_eq!(norm("ws://host:8080/"), "ws://host:8080/signal");
        assert_eq!(
            norm("ws://192.168.1.10:8080"),
            "ws://192.168.1.10:8080/signal"
        );
    }

    #[test]
    fn preserves_explicit_path() {
        assert_eq!(norm("ws://host:8080/signal"), "ws://host:8080/signal");
        assert_eq!(norm("wss://host/proxy/signal"), "wss://host/proxy/signal");
    }
}

#[cfg(test)]
mod host_candidate_tests {
    use super::host_candidates;
    use prdt_signaling_proto::{CandidateType, PRIORITY_HOST};
    use std::net::{IpAddr, SocketAddr};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn endpoints(cands: &[prdt_signaling_proto::Candidate]) -> Vec<String> {
        cands
            .iter()
            .map(|c| format!("{}:{}", c.ip, c.port))
            .collect()
    }

    #[test]
    fn unspecified_bind_enumerates_usable_ips_only() {
        // Loopback (127.x) and link-local (169.254.x) are filtered out; the two
        // routable LAN addresses survive, each keeping the socket's real port.
        let cands = host_candidates(
            addr("0.0.0.0:9000"),
            &[
                ip("127.0.0.1"),
                ip("169.254.1.2"),
                ip("192.168.100.134"),
                ip("10.0.0.5"),
            ],
        );
        assert_eq!(endpoints(&cands), ["192.168.100.134:9000", "10.0.0.5:9000"]);
        assert!(cands.iter().all(|c| c.typ == CandidateType::Host));
        assert!(cands.iter().all(|c| c.priority == PRIORITY_HOST));
        // The unroutable wildcard must never be advertised.
        assert!(!cands.iter().any(|c| c.ip == "0.0.0.0"));
    }

    #[test]
    fn specified_bind_is_advertised_verbatim() {
        // A concrete bind ignores the enumerated list and is sent as-is.
        let cands = host_candidates(
            addr("192.168.1.50:9000"),
            &[ip("10.0.0.5"), ip("172.16.0.9")],
        );
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].ip, "192.168.1.50");
        assert_eq!(cands[0].port, 9000);
        assert_eq!(cands[0].typ, CandidateType::Host);
    }

    #[test]
    fn unspecified_bind_with_only_loopback_is_empty() {
        // Nothing usable → empty, so the caller falls back at the call site.
        let cands = host_candidates(addr("0.0.0.0:9000"), &[ip("127.0.0.1")]);
        assert!(cands.is_empty());
    }
}

async fn send_msg(ws: &mut Ws, m: &ClientMessage) -> Result<(), SignalingError> {
    let s = serde_json::to_string(m)?;
    ws.send(Message::Text(s)).await?;
    Ok(())
}

async fn recv_msg(
    ws: &mut Ws,
    stage: &'static str,
    dur: Duration,
) -> Result<ServerMessage, SignalingError> {
    let frame = timeout(dur, ws.next())
        .await
        .map_err(|_| SignalingError::Timeout { stage })?;
    let frame = frame
        .ok_or_else(|| SignalingError::Protocol("connection closed".into()))?
        .map_err(SignalingError::from)?;
    match frame {
        Message::Text(t) => Ok(serde_json::from_str(&t)?),
        other => Err(SignalingError::Protocol(format!(
            "non-text frame: {other:?}"
        ))),
    }
}

/// Register (or re-confirm) a host identity with the signaling server without
/// waiting for a viewer to connect.
///
/// Used by offline-first provisioning (AC-9): the device generates its key
/// locally, then asynchronously calls this to obtain / confirm its
/// server-allocated 9-digit ID. Sends `Register { host_id, pubkey_b64 }` and
/// returns the server's `Registered { host_id }`.
///
/// Idempotency: pass the persisted `host_id` (dashed or empty). The server
/// returns the same ID for a matching key. Passing an empty `host_id` asks the
/// server to allocate — with the server's reverse-lookup-by-pubkey, a device
/// that still holds its key recovers its existing ID even if its local record
/// was lost. A `HostIdPubkeyMismatch` error means the ID is registered to a
/// different key.
#[instrument(skip(pubkey_b64), fields(host_id = %host_id))]
pub async fn register_host(
    url: &url::Url,
    host_id: &str,
    pubkey_b64: &str,
    timeout: Duration,
) -> Result<String, SignalingError> {
    let mut ws = ws_connect(url).await?;

    send_msg(
        &mut ws,
        &ClientMessage::Register {
            host_id: host_id.to_string(),
            pubkey_b64: pubkey_b64.to_string(),
        },
    )
    .await?;

    let allocated = match recv_msg(&mut ws, "registered", timeout).await? {
        ServerMessage::Registered { host_id } => host_id,
        ServerMessage::Error { code, message } => {
            return Err(SignalingError::Server { code, message })
        }
        other => {
            return Err(SignalingError::Protocol(format!(
                "expected Registered, got {other:?}"
            )))
        }
    };

    // Close cleanly; provisioning only needs the durable ID, not a live
    // session. The real listener re-registers later to become discoverable.
    let _ = ws.close(None).await;
    Ok(allocated)
}

#[instrument(skip(cfg, identity), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_host(
    cfg: RendezvousConfig,
    identity: HostIdentity,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(
        &mut ws,
        &ClientMessage::Register {
            host_id: cfg.host_id.clone(),
            pubkey_b64: identity.pubkey_b64,
        },
    )
    .await?;

    let allocated_host_id = match recv_msg(&mut ws, "registered", REGISTERED_TIMEOUT).await? {
        ServerMessage::Registered { host_id } => host_id,
        ServerMessage::Error { code, message } => {
            return Err(SignalingError::Server { code, message })
        }
        other => {
            return Err(SignalingError::Protocol(format!(
                "expected Registered, got {other:?}"
            )))
        }
    };

    let session_id = match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
        ServerMessage::SessionStart {
            session_id,
            role: Role::Host,
            ..
        } => session_id,
        ServerMessage::Error { code, message } => {
            return Err(SignalingError::Server { code, message })
        }
        other => {
            return Err(SignalingError::Protocol(format!(
                "expected SessionStart, got {other:?}"
            )))
        }
    };
    info!(%session_id, "session_start");

    send_candidates(
        &mut ws,
        &session_id,
        local_udp_addr,
        cfg.stun_url.as_ref(),
        cfg.turn_url.as_ref(),
    )
    .await?;

    let peer_candidates =
        recv_peer_candidates(&mut ws, cfg.timeout, cfg.aggregation_window).await?;

    send_msg(
        &mut ws,
        &ClientMessage::Done {
            session_id: session_id.clone(),
            outcome: DoneOutcome::Connected,
        },
    )
    .await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome {
        session_id,
        peer_pubkey_b64: None,
        peer_candidates,
        allocated_host_id,
    })
}

#[instrument(skip(cfg), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_viewer(
    cfg: RendezvousConfig,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(
        &mut ws,
        &ClientMessage::Connect {
            host_id: cfg.host_id.clone(),
        },
    )
    .await?;

    let (session_id, peer_pubkey_b64) =
        match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
            ServerMessage::SessionStart {
                session_id,
                role: Role::Viewer,
                peer_pubkey_b64,
            } => (session_id, peer_pubkey_b64),
            ServerMessage::Error { code, message } => {
                return Err(SignalingError::Server { code, message })
            }
            other => {
                return Err(SignalingError::Protocol(format!(
                    "expected SessionStart, got {other:?}"
                )))
            }
        };
    info!(%session_id, "session_start");

    send_candidates(
        &mut ws,
        &session_id,
        local_udp_addr,
        cfg.stun_url.as_ref(),
        cfg.turn_url.as_ref(),
    )
    .await?;

    let peer_candidates =
        recv_peer_candidates(&mut ws, cfg.timeout, cfg.aggregation_window).await?;

    send_msg(
        &mut ws,
        &ClientMessage::Done {
            session_id: session_id.clone(),
            outcome: DoneOutcome::Connected,
        },
    )
    .await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome {
        session_id,
        peer_pubkey_b64,
        peer_candidates,
        allocated_host_id: String::new(),
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
        return Err(SignalingError::Timeout {
            stage: "peer_candidate",
        });
    }
    Ok(collected)
}

async fn send_candidates(
    ws: &mut Ws,
    session_id: &str,
    local_udp_addr: SocketAddr,
    stun_url: Option<&url::Url>,
    turn_url: Option<&url::Url>,
) -> Result<(), SignalingError> {
    // Host candidate(s). A wildcard bind (0.0.0.0 / [::], the default) is
    // unroutable, so when the socket is bound to an unspecified address we
    // enumerate the machine's real interface IPs (the side effect) and advertise
    // one Host candidate per usable one. A concrete bind is advertised verbatim.
    let local_ips: Vec<IpAddr> = if local_udp_addr.ip().is_unspecified() {
        gather_local_ips()
    } else {
        Vec::new()
    };
    let mut host_cands = host_candidates(local_udp_addr, &local_ips);
    if host_cands.is_empty() {
        // Unspecified bind with no reachable interface IP found. Advertising the
        // wildcard is no worse than the pre-fix behavior, so send it as a last
        // resort rather than emitting nothing.
        tracing::warn!(
            %local_udp_addr,
            "no usable local interface IP for host candidate; advertising bind address as last resort"
        );
        host_cands.push(candidate_for(local_udp_addr));
    }
    for candidate in host_cands {
        send_msg(
            ws,
            &ClientMessage::Candidate {
                session_id: session_id.to_string(),
                candidate,
            },
        )
        .await?;
    }

    if let Some(url) = stun_url {
        match resolve_and_learn_srflx(url).await {
            Ok(srflx) => {
                send_msg(
                    ws,
                    &ClientMessage::Candidate {
                        session_id: session_id.to_string(),
                        candidate: Candidate {
                            typ: CandidateType::Srflx,
                            ip: srflx.ip().to_string(),
                            port: srflx.port(),
                            priority: PRIORITY_SRFLX,
                        },
                    },
                )
                .await?;
                tracing::info!(%srflx, "srflx candidate sent");
            }
            Err(e) => {
                tracing::warn!(error = %e, "STUN failed; proceeding without srflx candidate");
            }
        }
    }

    if let Some(url) = turn_url {
        match prdt_nat_traversal::TurnConfig::from_url(url).await {
            Ok(cfg) => {
                let probe_socket = Arc::new(tokio::net::UdpSocket::bind("0.0.0.0:0").await?);
                match prdt_nat_traversal::TurnRelaySocket::allocate_with_socket(probe_socket, cfg)
                    .await
                {
                    Ok(relay) => {
                        let relayed = relay.relayed_addr();
                        send_msg(
                            ws,
                            &ClientMessage::Candidate {
                                session_id: session_id.to_string(),
                                candidate: Candidate {
                                    typ: CandidateType::Relay,
                                    ip: relayed.ip().to_string(),
                                    port: relayed.port(),
                                    priority: PRIORITY_RELAY,
                                },
                            },
                        )
                        .await?;
                        tracing::info!(%relayed, "relay candidate sent");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "TURN allocate failed; no relay candidate")
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "TURN URL parse failed"),
        }
    }
    Ok(())
}

async fn resolve_and_learn_srflx(stun_url: &url::Url) -> Result<SocketAddr, SignalingError> {
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
    let addr =
        prdt_nat_traversal::learn_public_addr(&probe, stun_addr, std::time::Duration::from_secs(3))
            .await
            .map_err(|e| SignalingError::Protocol(format!("stun: {e}")))?;
    Ok(addr)
}
