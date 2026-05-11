//! End-to-end: in-process signaling-server + rendezvous_as_host + rendezvous_as_viewer +
//! CustomUdpTransport Noise handshake + Hello/HelloAck. Must complete within 15s.
//!
//! Locks in the Phase 2 W1 exit criterion: same-machine LAN loopback works through signaling.
// viewer_handshake is deprecated for production use; kept for transport integration tests.
#![allow(deprecated)]

use prdt_crypto::KeyPair;
use prdt_protocol::control::PermissionSet;
use prdt_protocol::{frame::Codec, MonitorRect};
use prdt_signaling_client::{
    rendezvous_as_host, rendezvous_as_viewer, HostIdentity, RendezvousConfig,
};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use prdt_transport::{
    host_handshake, viewer_handshake, AuthDecision, AuthHook, CustomUdpTransport, HelloRequest,
    UdpTransportConfig, DEFAULT_HANDSHAKE_TIMEOUT,
};

struct GrantAllHook;
#[async_trait::async_trait]
impl AuthHook for GrantAllHook {
    async fn evaluate(&self, _hello: &prdt_protocol::ControlMessage, _peer: &str) -> AuthDecision {
        AuthDecision::Grant(PermissionSet::all())
    }
}
use std::net::SocketAddr;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w1_smoke_signaling_noise_hello_ack_completes() {
    let signaling_url = spawn_signaling().await;

    // One keypair for the "host"; the "viewer" will TOFU it via the signaling-returned pubkey.
    let host_kp = KeyPair::generate();
    let host_pub_b64 = host_kp.public.to_base64();
    let host_pub_for_viewer = host_kp.public; // Copy

    let host_url = signaling_url.clone();
    let host_fut = async move {
        let transport = Arc::new(
            CustomUdpTransport::bind(
                "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                UdpTransportConfig::default(),
            )
            .await
            .expect("host bind"),
        );
        let local = transport.local_addr().expect("host local_addr");

        let outcome = rendezvous_as_host(
            RendezvousConfig {
                url: host_url,
                host_id: "w1-smoke".into(),
                timeout: Duration::from_secs(5),
                stun_url: None,
                turn_url: None,
                aggregation_window: std::time::Duration::from_millis(100),
            },
            HostIdentity {
                pubkey_b64: host_pub_b64,
            },
            local,
        )
        .await
        .expect("host rendezvous");

        let cand_addrs: Vec<std::net::SocketAddr> = outcome
            .peer_candidates
            .iter()
            .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
            .collect();
        let _peer_addr = transport
            .probe_and_commit_peer(&cand_addrs, std::time::Duration::from_secs(5))
            .await
            .expect("probe winner");

        transport
            .handshake_as_server(&host_kp)
            .await
            .expect("host Noise handshake");

        // Hello/HelloAck
        let _req = host_handshake(
            &*transport,
            &GrantAllHook,
            "smoke-peer",
            0xABCD_EF01,
            42,
            10_000_000,
            MonitorRect::new(0, 0, 1920, 1080),
            MonitorRect::new(0, 0, 1920, 1080),
            &[Codec::H265],
            Duration::from_secs(5),
        )
        .await
        .expect("host Hello/HelloAck");
    };

    let viewer_url = signaling_url.clone();
    let viewer_fut = async move {
        // Head-start for host to finish Register before viewer Connects.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let transport = Arc::new(
            CustomUdpTransport::bind(
                "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                UdpTransportConfig::default(),
            )
            .await
            .expect("viewer bind"),
        );
        let local = transport.local_addr().expect("viewer local_addr");

        let outcome = rendezvous_as_viewer(
            RendezvousConfig {
                url: viewer_url,
                host_id: "w1-smoke".into(),
                timeout: Duration::from_secs(5),
                stun_url: None,
                turn_url: None,
                aggregation_window: std::time::Duration::from_millis(100),
            },
            local,
        )
        .await
        .expect("viewer rendezvous");
        assert!(
            outcome.peer_pubkey_b64.is_some(),
            "viewer should receive host pubkey"
        );

        let cand_addrs: Vec<std::net::SocketAddr> = outcome
            .peer_candidates
            .iter()
            .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
            .collect();
        let _peer_addr = transport
            .probe_and_commit_peer(&cand_addrs, std::time::Duration::from_secs(5))
            .await
            .expect("probe winner");

        let viewer_kp = KeyPair::generate();
        transport
            .handshake_as_client(&host_pub_for_viewer, &viewer_kp, DEFAULT_HANDSHAKE_TIMEOUT)
            .await
            .expect("viewer Noise handshake");

        // Hello exchange
        let ack = viewer_handshake(
            &*transport,
            &HelloRequest {
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
            },
            Duration::from_millis(500),
            5,
        )
        .await
        .expect("viewer Hello");
        assert_eq!(ack.session_id, 0xABCD_EF01);
        assert_eq!(ack.neg_width, 1920);
    };

    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(host_fut, viewer_fut)
    })
    .await
    .expect("W1 smoke must complete within 15s");
}
