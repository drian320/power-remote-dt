//! L3 integration tests: SetBitrate round-trip + loss-burst drives MD.

use std::time::{Duration, Instant};

use prdt_transport::bitrate_control::{BitrateController, BitrateControllerConfig};

#[test]
fn setbitrate_round_trip_via_controller() {
    // Seed the controller into a state where MD has already fired, then
    // verify should_send() flips and target_bps reports the post-MD value.
    let mut cfg = BitrateControllerConfig::new_for_max(10_000_000);
    cfg.initial_bps = 10_000_000;
    let mut c = BitrateController::new(cfg);
    c.observe(50, 1000); // 5% loss
    c.aimd_step(Instant::now());
    assert!(c.should_send(), "5% loss → MD → should_send() true");
    let bps = c.target_bps();
    assert!((1_000_000..10_000_000).contains(&bps));
    c.mark_sent();
    assert!(!c.should_send(), "after mark_sent, should_send() false");
}

#[test]
fn loss_burst_drives_md_monotonically() {
    // Simulated 5-second window with sustained 5% loss. Assert that the
    // controller's target_bps decreases monotonically across at least two
    // 1 Hz ticks, and approaches min_bps (1 Mbps) within 5 ticks.
    let mut cfg = BitrateControllerConfig::new_for_max(30_000_000);
    cfg.initial_bps = 30_000_000;
    cfg.cooldown_after_md = Duration::from_millis(0); // simulate steady loss
    let mut c = BitrateController::new(cfg);
    let mut prev = c.target_bps();
    let now = Instant::now();
    let mut history = vec![prev];
    for tick in 0..5 {
        c.observe(50, 1000); // 5% loss each tick
        c.aimd_step(now + Duration::from_secs(tick));
        c.reset_window();
        let curr = c.target_bps();
        assert!(curr <= prev, "tick {tick}: {curr} should be <= prev {prev}");
        history.push(curr);
        prev = curr;
    }
    // After 5 multiplicative-decreases of 0.7×: 30M × 0.7^5 ≈ 5.04M.
    // Should still be above min_bps but well below max_bps.
    assert!(
        history.last().copied().unwrap() < 10_000_000,
        "5 ticks of 5% loss should drop to <10 Mbps; history: {history:?}"
    );
    assert!(
        history.last().copied().unwrap() >= 1_000_000,
        "should not undershoot min_bps; history: {history:?}"
    );
}
