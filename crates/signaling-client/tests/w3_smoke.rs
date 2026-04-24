//! W3 end-to-end: mock STUN (reports loopback back as "public") + in-process
//! signaling-server + probe_and_commit_peer + Noise + Hello/HelloAck.
//! Proves the full pipeline wires up correctly and the probe selects a
//! reachable candidate.

use bytecodec::{DecodeExt, EncodeExt};
use prdt_crypto::KeyPair;
use prdt_protocol::{frame::Codec, MonitorRect};
use prdt_signaling_client::{rendezvous_as_host, rendezvous_as_viewer, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::CandidateType;
use prdt_signaling_server::{router, ServerConfig, ServerState};
use prdt_transport::{
    host_handshake, viewer_handshake, CustomUdpTransport, HelloRequest, UdpTransportConfig,
    DEFAULT_HANDSHAKE_TIMEOUT,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder,
};
use tokio::net::UdpSocket;
use url::Url;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_stun_reporting(report: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut dec = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = dec.decode_from_bytes(&buf[..n]) else { continue };
            if req.class() != MessageClass::Request || req.method() != BINDING { continue; }
            let mut resp = Message::new(MessageClass::SuccessResponse, BINDING, req.transaction_id());
            resp.add_attribute(Attribute::from(XorMappedAddress::new(report)));
            let mut enc = MessageEncoder::<Attribute>::new();
            let bytes = enc.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    addr
}

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
async fn w3_smoke_probe_plus_noise_plus_hello() {
    let signaling_url = spawn_signaling().await;

    // Pre-bind both transports so we know their real loopback ports.
    let host_transport = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let viewer_transport = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let host_real = host_transport.local_addr().unwrap();
    let viewer_real = viewer_transport.local_addr().unwrap();

    // Mock STUN reports the REAL loopback addr as "public" — so the Srflx
    // candidate carries a reachable addr. In practice, both Host and Srflx
    // candidates here point to the same loopback port; the probe first-to-ack
    // race picks whichever answers first. The assertion is "probe succeeded +
    // Noise established" — specific winner typ is not asserted.
    let host_stun = spawn_stun_reporting(host_real).await;
    let viewer_stun = spawn_stun_reporting(viewer_real).await;

    let host_kp = KeyPair::generate();
    let host_pub_b64 = host_kp.public.to_base64();
    let host_pub_copy = host_kp.public;

    let sig_a = signaling_url.clone();
    let host_stun_url: Url = format!("stun://{host_stun}").parse().unwrap();
    let ht = Arc::clone(&host_transport);
    let host_fut = async move {
        let outcome = rendezvous_as_host(
            RendezvousConfig {
                url: sig_a,
                host_id: "w3".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(host_stun_url),
                turn_url: None,
                aggregation_window: Duration::from_millis(300),
            },
            HostIdentity { pubkey_b64: host_pub_b64 },
            host_real,
        ).await.expect("host rendezvous");

        let cand_addrs: Vec<SocketAddr> = outcome.peer_candidates.iter()
            .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
            .collect();
        let peer_addr = ht.probe_and_commit_peer(&cand_addrs, Duration::from_secs(5)).await.expect("host probe");
        eprintln!("w3_smoke host probe winner: {peer_addr}");

        ht.handshake_as_server(&host_kp).await.expect("host Noise");
        let _req = host_handshake(
            &*ht,
            0xDEAD_BEEF,
            0,
            10_000_000,
            MonitorRect::new(0, 0, 1920, 1080),
            MonitorRect::new(0, 0, 1920, 1080),
            Duration::from_secs(5),
        ).await.expect("host Hello");
    };

    let sig_b = signaling_url.clone();
    let viewer_stun_url: Url = format!("stun://{viewer_stun}").parse().unwrap();
    let vt = Arc::clone(&viewer_transport);
    let viewer_fut = async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let outcome = rendezvous_as_viewer(
            RendezvousConfig {
                url: sig_b,
                host_id: "w3".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(viewer_stun_url),
                turn_url: None,
                aggregation_window: Duration::from_millis(300),
            },
            viewer_real,
        ).await.expect("viewer rendezvous");
        assert!(outcome.peer_pubkey_b64.is_some());
        assert!(outcome.peer_candidates.iter().any(|c| c.typ == CandidateType::Host),
            "viewer missing peer Host; got {:?}", outcome.peer_candidates);

        let cand_addrs: Vec<SocketAddr> = outcome.peer_candidates.iter()
            .filter_map(|c| format!("{}:{}", c.ip, c.port).parse().ok())
            .collect();
        let peer_addr = vt.probe_and_commit_peer(&cand_addrs, Duration::from_secs(5)).await.expect("viewer probe");
        eprintln!("w3_smoke viewer probe winner: {peer_addr}");

        vt.handshake_as_client(&host_pub_copy, DEFAULT_HANDSHAKE_TIMEOUT).await.expect("viewer Noise");
        let ack = viewer_handshake(
            &*vt,
            &HelloRequest { req_width: 1920, req_height: 1080, req_fps: 60, codec: Codec::H265 },
            Duration::from_millis(500),
            5,
        ).await.expect("viewer Hello");
        assert_eq!(ack.session_id, 0xDEAD_BEEF);
    };

    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(host_fut, viewer_fut)
    }).await.expect("W3 smoke must complete within 20s");
}
