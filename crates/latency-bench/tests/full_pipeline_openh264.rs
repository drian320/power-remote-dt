//! Plan §Phase 5 integration test: 60 frames of 1080p I420 through
//! OpenH264 encode → InProcTransport → OpenH264 decode. Asserts
//! received==sent (within OpenH264's 1-frame warm-up tolerance) and
//! decode_p95_us < 30_000.
//!
//! Pure-CPU; no D3D11, no GPU. Runs on any platform that builds
//! `prdt-media-sw` (currently any windows/linux x86_64 with a working
//! C toolchain for the vendored OpenH264 source).
// full_pipeline mod is gated to windows in lib.rs; pre-existing before L1.5a.
#![cfg(windows)]

use std::time::Duration;

use prdt_latency_bench::{
    full_pipeline::{run_for_matrix_openh264, ConsumerBackend, EncoderBackend, FullPipelineConfig},
    percentiles,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_pipeline_openh264_loopback() {
    let cfg = FullPipelineConfig {
        width: 1920,
        height: 1080,
        // 60 fps × 1s = 60 frames target. The bench loop drives encode
        // off wall-clock, so a 1-second duration sends ~60 frames.
        fps: 60,
        duration: Duration::from_secs(1),
        bitrate_bps: 30_000_000,
        drop_ppm: 0,
        latency_ms: 0,
        csv: None,
        consumer: ConsumerBackend::Openh264,
        encoder: EncoderBackend::Openh264,
    };

    let stats = run_for_matrix_openh264(&cfg)
        .await
        .expect("openh264 full-pipeline must succeed");

    // OpenH264 typically emits the first decoded frame after 1-2 inputs
    // because the decoder needs SPS/PPS before it can produce output.
    // Allow a 2-frame warm-up gap.
    assert!(
        stats.sent >= 30,
        "expected ≥30 sent frames in 1s @60fps, got {}",
        stats.sent
    );
    assert!(
        stats.received + 2 >= stats.sent,
        "expected received≈sent (allowing 2-frame warm-up), got {}/{}",
        stats.received,
        stats.sent,
    );
    assert!(
        stats.received > 0,
        "decoder produced 0 frames; OpenH264 backend appears broken"
    );

    let mut decode_us: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.decode_done_us.saturating_sub(s.recv_us))
        .collect();
    let (_, _, p95, _, _) = percentiles(&mut decode_us);
    assert!(
        p95 < 30_000,
        "openh264 decode p95 = {p95}µs ≥ 30000µs budget"
    );
}
