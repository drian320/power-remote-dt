use prdt_signaling_client::{rendezvous_as_host, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

async fn start_server() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn host_rendezvous_completes_when_viewer_arrives() {
    let addr = start_server().await;
    let ws_url: Url = format!("ws://{addr}/signal").parse().unwrap();

    let local_udp: SocketAddr = "127.0.0.1:40001".parse().unwrap();
    let host_task = tokio::spawn(async move {
        rendezvous_as_host(
            RendezvousConfig {
                url: ws_url,
                host_id: "h1".into(),
                timeout: Duration::from_secs(5),
                stun_url: None,
                turn_url: None,
                aggregation_window: std::time::Duration::from_millis(100),
            },
            HostIdentity {
                pubkey_b64: "HOSTPK".into(),
            },
            local_udp,
        )
        .await
    });

    // Viewer side as raw WS mock.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ws_url_str = format!("ws://{addr}/signal");
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url_str).await.unwrap();
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    async fn send(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        m: ClientMessage,
    ) {
        let s = serde_json::to_string(&m).unwrap();
        SinkExt::send(ws, Message::Text(s)).await.unwrap();
    }
    async fn recv(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> prdt_signaling_proto::ServerMessage {
        let f = ws.next().await.unwrap().unwrap();
        let t = match f {
            Message::Text(s) => s,
            o => panic!("{o:?}"),
        };
        serde_json::from_str(&t).unwrap()
    }

    send(
        &mut viewer_ws,
        ClientMessage::Connect {
            host_id: "h1".into(),
        },
    )
    .await;
    let start = recv(&mut viewer_ws).await;
    let sid = match start {
        prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };

    // Receive host's candidate
    let pc = recv(&mut viewer_ws).await;
    match pc {
        prdt_signaling_proto::ServerMessage::PeerCandidate { candidate, .. } => {
            assert_eq!(candidate.port, 40001);
        }
        _ => unreachable!(),
    }

    // Viewer replies with its own candidate
    send(
        &mut viewer_ws,
        ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate {
                typ: CandidateType::Host,
                ip: "127.0.0.1".into(),
                port: 40002,
                priority: PRIORITY_HOST,
            },
        },
    )
    .await;

    let outcome = host_task.await.unwrap().unwrap();
    assert_eq!(outcome.session_id, sid);
    let peer_addr = outcome
        .peer_candidates
        .iter()
        .find(|c| c.typ == prdt_signaling_proto::CandidateType::Host)
        .and_then(|c| {
            format!("{}:{}", c.ip, c.port)
                .parse::<std::net::SocketAddr>()
                .ok()
        })
        .expect("no host candidate in peer_candidates");
    assert_eq!(peer_addr.port(), 40002);
    assert_eq!(peer_addr.ip().to_string(), "127.0.0.1");
    assert_eq!(outcome.peer_pubkey_b64, None);
}
