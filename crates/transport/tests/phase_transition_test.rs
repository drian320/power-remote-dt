//! Integration tests for the probe → Noise phase transition, covering the
//! staggered-timing deadlock observed in production: two peers whose phases
//! are offset by network/timing would strand each other because the single
//! socket reader in each phase dropped messages belonging to the other phase.
//!
//! Real UDP sockets on 127.0.0.1:0, matching the style of `probe_test.rs` and
//! `encrypted_test.rs`. Timeouts are kept generous (>=3s) to avoid CI flakes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use prdt_crypto::KeyPair;
use prdt_protocol::control::ControlMessage;
use prdt_protocol::wire::{PacketHeader, PacketType, HEADER_LEN};
use prdt_transport::{CustomUdpTransport, TransportError, UdpTransportConfig};
use tokio::net::UdpSocket;

/// The full production deadlock, end to end: peer A (viewer) probe-commits to
/// B and immediately starts its client Noise handshake, sending NoiseE1. Peer
/// B (host) starts probing only *after* that NoiseE1 is already in flight, and
/// probes a candidate that will never answer — so B can only commit via the
/// NoiseE1-triggered path inside `probe_and_commit_peer`, then serve the
/// stashed E1 from `handshake_as_server`. Before the fix, B's probe loop
/// dropped the NoiseE1 (`_ => continue`) and both sides timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn noise_e1_during_probe_commits_and_completes() {
    let cfg = UdpTransportConfig::default();
    let a = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let b = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();

    // B is the host: it owns the static server keypair. A is the viewer.
    let server_kp = KeyPair::generate();
    let server_pub = server_kp.public;
    let client_kp = KeyPair::generate();
    let client_pub = client_kp.public;

    // A: probe-commit toward B, then run the client handshake.
    let a_clone = Arc::clone(&a);
    let task_a = tokio::spawn(async move {
        let winner = a_clone
            .probe_and_commit_peer(&[b_addr], Duration::from_secs(5))
            .await?;
        assert_eq!(winner, b_addr, "A should commit to B");
        a_clone
            .handshake_as_client(&server_pub, &client_kp, Duration::from_secs(5))
            .await
    });

    // B: start probing ~300ms late (A's NoiseE1 is already queued on B's
    // socket by then), toward an unreachable candidate so B never commits via
    // the ordinary probe/probe-ack path — it must commit via the NoiseE1
    // trigger and then consume the stashed E1 in handshake_as_server.
    let b_clone = Arc::clone(&b);
    let task_b = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let unreachable: SocketAddr = "240.0.0.1:1".parse().unwrap();
        let winner = b_clone
            .probe_and_commit_peer(&[unreachable], Duration::from_secs(5))
            .await?;
        assert_eq!(
            winner, a_addr,
            "B should commit to A via the NoiseE1-triggered path"
        );
        let peer_pub = b_clone.handshake_as_server(&server_kp).await?;
        Ok::<_, TransportError>(peer_pub)
    });

    let (ra, rb) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(task_a, task_b)
    })
    .await
    .expect("deadlock scenario did not resolve within 15s");

    ra.expect("task A panicked")
        .expect("A's client handshake should complete");
    let peer_pub = rb
        .expect("task B panicked")
        .expect("B's server handshake should complete via stashed E1");
    assert_eq!(
        peer_pub, client_pub,
        "B should recover A's authenticated static pubkey"
    );
}

/// While A is parked in `handshake_as_client` waiting for NoiseE2, a raw Probe
/// from a third socket must be answered with a ProbeAck (so a late host can
/// probe-commit to us instead of stranding), and A must still complete the
/// handshake once the real NoiseE2 arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_noise_wait_answers_probes() {
    let cfg = UdpTransportConfig::default();
    let a = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let b = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap(),
    );
    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a_addr).await;

    let server_kp = KeyPair::generate();
    let server_pub = server_kp.public;
    let client_kp = KeyPair::generate();

    // A: client handshake. Sends NoiseE1 to B (which is not yet reading it, so
    // it queues on B's socket), then parks in its wait loop for NoiseE2.
    let a_clone = Arc::clone(&a);
    let task_a = tokio::spawn(async move {
        a_clone
            .handshake_as_client(&server_pub, &client_kp, Duration::from_secs(5))
            .await
    });

    // A raw prober socket: pokes A with a Probe while it waits, and must get a
    // ProbeAck back to this very socket.
    let prober = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Let A send its NoiseE1 and settle into the wait loop.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nonce = [0x5Au8; 16];
    let probe_body = prdt_protocol::encode_control(&ControlMessage::Probe { nonce }).unwrap();
    let hdr = PacketHeader {
        packet_type: PacketType::Control,
        flags: 0,
        session_id: cfg.session_id,
        payload_len: probe_body.len() as u32,
    };
    let mut pkt = Vec::with_capacity(HEADER_LEN + probe_body.len());
    pkt.extend_from_slice(&hdr.encode());
    pkt.extend_from_slice(&probe_body);
    prober.send_to(&pkt, a_addr).await.unwrap();

    // Expect a ProbeAck for our nonce back on the prober socket.
    let mut buf = vec![0u8; 4096];
    let (n, _from) = tokio::time::timeout(Duration::from_secs(3), prober.recv_from(&mut buf))
        .await
        .expect("no ProbeAck arrived on the prober socket within 3s")
        .unwrap();
    let ack_hdr = PacketHeader::decode(&buf[..n]).expect("prober got malformed reply");
    let ack_end = HEADER_LEN + ack_hdr.payload_len as usize;
    let ack = prdt_protocol::decode_control(&buf[HEADER_LEN..ack_end]).expect("bad control reply");
    assert_eq!(
        ack,
        ControlMessage::ProbeAck { nonce },
        "A must answer a Probe with a matching ProbeAck while waiting for NoiseE2"
    );

    // Now let B respond to the queued NoiseE1, which should unblock A's wait
    // loop and complete the handshake.
    b.handshake_as_server(&server_kp)
        .await
        .expect("B server handshake should complete");

    let a_res = tokio::time::timeout(Duration::from_secs(5), task_a)
        .await
        .expect("A did not finish within 5s")
        .expect("task A panicked");
    a_res.expect("A should complete the handshake after answering the probe");
}
