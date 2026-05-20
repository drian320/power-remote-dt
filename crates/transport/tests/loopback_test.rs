use std::time::Duration;

use bytes::Bytes;
use prdt_protocol::{frame::Codec, EncodedFrame};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};

fn make_frame(seq: u64, size: usize) -> EncodedFrame {
    EncodedFrame {
        seq,
        timestamp_host_us: seq * 1000,
        is_keyframe: seq.is_multiple_of(60),
        nal_units: Bytes::from(vec![(seq as u8).wrapping_mul(7); size]),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    }
}

/// Smoke: 100 frames, no loss, all delivered in order.
#[tokio::test]
async fn loopback_100_frames_no_loss() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions::default());
    let handle = tokio::spawn(async move {
        for i in 0..100 {
            host.send_video(make_frame(i, 500)).await.unwrap();
        }
    });
    let mut received = 0u64;
    while received < 100 {
        let m = tokio::time::timeout(Duration::from_secs(2), viewer.recv())
            .await
            .unwrap()
            .unwrap();
        if let ReceivedMessage::Video(f) = m {
            assert_eq!(f.seq, received);
            received += 1;
        }
    }
    handle.await.unwrap();
}

/// With 10ms latency every message still arrives.
#[tokio::test]
async fn loopback_with_latency() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: 0,
        latency: Some(Duration::from_millis(10)),
    });
    let start = std::time::Instant::now();
    let sender = tokio::spawn(async move {
        for i in 0..20 {
            host.send_video(make_frame(i, 100)).await.unwrap();
        }
    });
    let mut count = 0;
    while count < 20 {
        if let ReceivedMessage::Video(_) =
            tokio::time::timeout(Duration::from_secs(5), viewer.recv())
                .await
                .unwrap()
                .unwrap()
        {
            count += 1;
        }
    }
    sender.await.unwrap();
    // Last frame must arrive strictly after >= 20 * 10ms = 200ms (since latency is per-message serial).
    assert!(start.elapsed() >= Duration::from_millis(200));
}

/// With 5% drop rate, we expect some losses but the pipeline must not panic
/// and at least half the frames should still arrive.
#[tokio::test]
async fn loopback_with_drops_survives() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: 50_000, // 5%
        latency: None,
    });
    let sender = tokio::spawn(async move {
        for i in 0..200 {
            let _ = host.send_video(make_frame(i, 100)).await;
        }
    });
    let mut received = 0;
    loop {
        match tokio::time::timeout(Duration::from_millis(100), viewer.recv()).await {
            Ok(Ok(ReceivedMessage::Video(_))) => received += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    sender.await.unwrap();
    assert!(received > 100, "too many losses: only {received}/200");
}
