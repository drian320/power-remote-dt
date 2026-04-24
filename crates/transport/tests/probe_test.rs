use prdt_transport::{CustomUdpTransport, UdpTransportConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_transports_find_each_other() {
    let a = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let b = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();

    let a_clone = Arc::clone(&a);
    let task_a = tokio::spawn(async move {
        a_clone.probe_and_commit_peer(&[b_addr], Duration::from_secs(3)).await
    });
    let b_clone = Arc::clone(&b);
    let task_b = tokio::spawn(async move {
        b_clone.probe_and_commit_peer(&[a_addr], Duration::from_secs(3)).await
    });

    let (ra, rb) = tokio::join!(task_a, task_b);
    let winner_a = ra.unwrap().unwrap();
    let winner_b = rb.unwrap().unwrap();

    assert_eq!(winner_a, b_addr, "a should pick b");
    assert_eq!(winner_b, a_addr, "b should pick a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_candidate_is_skipped() {
    let a = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let b = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();

    let a_clone = Arc::clone(&a);
    let task_a = tokio::spawn(async move {
        let candidates = vec!["240.0.0.1:1".parse::<SocketAddr>().unwrap(), b_addr];
        a_clone.probe_and_commit_peer(&candidates, Duration::from_secs(3)).await
    });
    let b_clone = Arc::clone(&b);
    let task_b = tokio::spawn(async move {
        b_clone.probe_and_commit_peer(&[a_addr], Duration::from_secs(3)).await
    });

    let (ra, rb) = tokio::join!(task_a, task_b);
    let winner_a = ra.unwrap().unwrap();
    let _ = rb.unwrap().unwrap();
    assert_eq!(winner_a, b_addr, "a should pick the reachable candidate");
}

#[tokio::test]
async fn all_unreachable_times_out() {
    let t = Arc::new(
        CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
            .await.unwrap(),
    );
    let err = t.probe_and_commit_peer(
        &["240.0.0.1:1".parse::<SocketAddr>().unwrap(), "240.0.0.2:1".parse::<SocketAddr>().unwrap()],
        Duration::from_millis(500),
    ).await.unwrap_err();
    assert!(matches!(err, prdt_transport::TransportError::HandshakeTimeout), "got: {err:?}");
}
