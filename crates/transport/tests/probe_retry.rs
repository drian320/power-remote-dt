//! Verifies `CustomUdpTransport::probe_and_commit_peer` resends Probe packets
//! so a transient drop of the first N packets (typical of stateful firewalls
//! admitting the first outbound but dropping inbound until state is tracked)
//! is masked within the configured retry budget.

use prdt_protocol::control::ControlMessage;
use prdt_protocol::wire::{PacketHeader, PacketType, HEADER_LEN};
use prdt_transport::{
    CustomUdpTransport, UdpTransportConfig, PROBE_RETRY_COUNT, PROBE_RETRY_INTERVAL,
};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_retry_survives_first_packet_drops() {
    // Simulate a firewall that drops the first `DROP_COUNT` Probes, accepts
    // the (DROP_COUNT+1)th, and only then emits the ProbeAck. The client
    // should retry enough times to get past this.
    const DROP_COUNT: u32 = 2;
    assert!(
        DROP_COUNT < PROBE_RETRY_COUNT,
        "DROP_COUNT must be strictly less than PROBE_RETRY_COUNT"
    );

    let client = Arc::new(
        CustomUdpTransport::bind(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            UdpTransportConfig::default(),
        )
        .await
        .unwrap(),
    );

    let server = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server.local_addr().unwrap();

    let probe_count = Arc::new(AtomicU32::new(0));
    let probe_count_bg = Arc::clone(&probe_count);
    let server_bg = Arc::clone(&server);

    let server_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = match server_bg.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            let hdr = match PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(_) => continue,
            };
            if hdr.packet_type != PacketType::Control {
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                continue;
            }
            let msg = match prdt_protocol::decode_control(&buf[HEADER_LEN..body_end]) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let ControlMessage::Probe { nonce } = msg {
                let count = probe_count_bg.fetch_add(1, Ordering::SeqCst) + 1;
                if count > DROP_COUNT {
                    let ack = ControlMessage::ProbeAck { nonce };
                    let body = prdt_protocol::encode_control(&ack).unwrap();
                    let ack_hdr = PacketHeader {
                        packet_type: PacketType::Control,
                        flags: 0,
                        session_id: 0,
                        payload_len: body.len() as u32,
                    };
                    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
                    out.extend_from_slice(&ack_hdr.encode());
                    out.extend_from_slice(&body);
                    let _ = server_bg.send_to(&out, from).await;
                }
            }
        }
    });

    let start = std::time::Instant::now();
    let winner = client
        .probe_and_commit_peer(&[server_addr], Duration::from_secs(10))
        .await
        .expect("probe_and_commit_peer should succeed once retries land");
    let elapsed = start.elapsed();

    assert_eq!(winner, server_addr);
    let observed = probe_count.load(Ordering::SeqCst);
    assert!(
        observed > DROP_COUNT,
        "server should have observed at least {} probes, saw {observed}",
        DROP_COUNT + 1
    );
    // (DROP_COUNT+1) successful probe = initial + DROP_COUNT retries.
    // Allow 2× slack over the nominal DROP_COUNT × interval for CI timing.
    let generous = PROBE_RETRY_INTERVAL * (DROP_COUNT * 2 + 2);
    assert!(
        elapsed < generous,
        "probe_and_commit_peer took {elapsed:?}, expected < {generous:?} with retry",
    );
    server_task.abort();
}
