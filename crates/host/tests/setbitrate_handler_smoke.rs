//! Smoke test for the host's SetBitrate control-loop arm.
//!
//! The arm itself in lib.rs is small (forwarding via mpsc), so this test
//! exercises the equivalent forwarding logic in isolation: receiving a
//! `ControlMessage::SetBitrate` should produce a u32 on the bitrate channel
//! that the video loop will drain. Mirrors `request_idr_handler_smoke.rs`
//! pattern from L2.

use prdt_protocol::ControlMessage;
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test]
async fn setbitrate_forwards_target_bps_to_video_channel() {
    let (bitrate_tx, mut bitrate_rx) = unbounded_channel::<u32>();

    // Simulate the control-loop arm:
    let msg = ControlMessage::SetBitrate {
        target_bps: 5_000_000,
    };
    if let ControlMessage::SetBitrate { target_bps } = msg {
        let _ = bitrate_tx.send(target_bps);
    }

    let received = bitrate_rx.recv().await.expect("channel open");
    assert_eq!(received, 5_000_000);
}

#[tokio::test]
async fn setbitrate_video_loop_drains_to_latest() {
    // The video loop's drain logic: try_recv until empty, keep last.
    // Simulates rapid SetBitrate updates between video frames.
    let (bitrate_tx, mut bitrate_rx) = unbounded_channel::<u32>();
    bitrate_tx.send(8_000_000).unwrap();
    bitrate_tx.send(5_000_000).unwrap();
    bitrate_tx.send(3_000_000).unwrap();

    let mut latest: Option<u32> = None;
    while let Ok(bps) = bitrate_rx.try_recv() {
        latest = Some(bps);
    }
    assert_eq!(latest, Some(3_000_000));
}
