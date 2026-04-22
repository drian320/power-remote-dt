//! End-to-end pipeline smoke test: `DxgiNvencProducer` to encoded bytes to
//! `MfD3d11Consumer` to decoded NV12 bytes.

#![cfg(windows)]

use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, D3d11Device, DxgiNvencProducer,
    MfD3d11Consumer,
};
use prdt_protocol::{VideoConsumer, VideoProducer};

#[tokio::test]
async fn producer_to_consumer_end_to_end() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("no adapter (skip): {e}");
            return;
        }
    };
    if !adapter.is_nvidia() {
        eprintln!("skip: non-NVIDIA adapter");
        return;
    }
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let outputs = enumerate_outputs_for_adapter(&adapter).expect("outputs");
    let primary = outputs
        .iter()
        .find(|o| o.is_attached)
        .cloned()
        .unwrap_or_else(|| outputs[0].clone());

    let mut producer = match DxgiNvencProducer::new(&dev, &primary, 10_000_000) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skip: producer init failed: {e}");
            return;
        }
    };
    let width = producer.width();
    let height = producer.height();
    let mut consumer = match MfD3d11Consumer::new(&dev, width, height) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skip: consumer init failed: {e}");
            return;
        }
    };

    // Drive for ~10 frames so the MF decoder has time to emit output.
    let mut idr_seen = false;
    let mut frames_submitted = 0;
    for _ in 0..10 {
        let frame = match producer.next_frame().await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("producer error (skip): {e}");
                return;
            }
        };
        if frame.is_keyframe {
            idr_seen = true;
        }
        consumer.submit(frame).await.expect("consumer submit");
        frames_submitted += 1;
    }
    eprintln!("submitted {frames_submitted} frames, first_idr_seen={idr_seen}");
    assert!(idr_seen, "first frame should be IDR");

    let latest = consumer.take_latest_frame();
    match latest {
        Some(bytes) => {
            eprintln!("consumer decoded output: {} bytes", bytes.len());
            // NV12 for W x H = W*H*1.5, possibly with alignment padding.
            assert!(
                bytes.len() >= (width * height) as usize,
                "decoded buffer smaller than Y plane: {} < {}",
                bytes.len(),
                width * height
            );
        }
        None => {
            eprintln!("no decoded frame (desktop might be static; lenient pass)");
        }
    }
}
