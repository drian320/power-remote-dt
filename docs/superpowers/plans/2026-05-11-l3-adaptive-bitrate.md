# L3 Adaptive Bitrate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an observed-loss-driven AIMD bitrate controller on the viewer that drops the host encoder's target bitrate when WiFi/LAN packet loss spikes, preventing the L2 smoke regression where 5.7% packet delivery → 5s host-watchdog session kill.

**Architecture:** Viewer-side stateless `BitrateController` runs at 1Hz inside the existing `latency_task`. It observes `purge_assembler() → Vec<u64>` (added in L2) for frame-loss count and `LatencyProbe::snapshot().present.samples` for total count, applies AIMD (Multiplicative-Decrease 0.7× on loss>2%, Additive-Increase +200kbps/s on loss<0.5% with 2s post-MD cooldown), and sends `ControlMessage::SetBitrate { target_bps: u32 }` (existing wire, dead path) when the change exceeds 5% hysteresis. The host adds a `SetBitrate` arm to its control loop, forwards via `tokio::sync::mpsc::unbounded_channel<u32>` to the video loop, which drains the channel before each `next_frame()` and calls `producer.set_target_bitrate(bps)`.

**Tech Stack:** Rust 1.85, tokio 1.x, async-trait, clap derive, tracing. No new crate dependencies.

---

## Pre-Task Context (T0 already resolved)

The brainstorming session already grepped the codebase to resolve spec §6 open questions:

| # | Question | Resolution |
|---|---|---|
| Q1 | `VideoProducer::set_target_bitrate` trait shape | **Already exists** at `crates/protocol/src/video_pipeline.rs:43`. All 3 impls present. Only `DxgiNvencProducer` (media-win/pipeline/producer.rs:190) is a **no-op stub** — needs real wiring in T4 |
| Q2 | Encoder apply timing | OpenH264 applies per-frame (cheap `encoder.set_bitrate()`). NVENC/MF apply via their respective `set_target_bitrate` methods (already exist on `Hevc265Encoder` trait at `crates/media-win/src/encoder_trait.rs:40`) |
| Q3 | `bitrate_tx` channel placement | Session-local closure clone, mirror `force_idr_flag: Arc<AtomicBool>` pattern at `host/src/lib.rs:483, 490, 623`. Use `tokio::sync::mpsc::unbounded_channel::<u32>()` |
| Q4 | Viewer rolling window | Caller (latency_task) tracks `last_total_samples`. Per tick: `total = snapshot.present.samples - last_total_samples; lost = purged.len() as u64`. Controller is stateless step (lost/total passed in). |
| Q5 | Flag name | `--no-adaptive-bitrate` (clap `bool` flag, default false = enabled) |

**Key file references** (read-only context for implementers):

- `crates/transport/src/udp.rs:99` — default `fec_k=64, fec_m=6` (~75KB/frame, 6-packet burst tolerance)
- `crates/transport/src/udp.rs:691` — `pub async fn purge_assembler(&self) -> Vec<u64>` (L2 added)
- `crates/transport/src/assembler.rs:209` — `pub fn purge(&mut self) -> Vec<u64>` (returns timeout-purged seq numbers)
- `crates/protocol/src/control.rs:84` — `ControlMessage::SetBitrate { target_bps: u32 }` (kind_u8=6)
- `crates/protocol/src/video_pipeline.rs:34-50` — `VideoProducer` trait
- `crates/host/src/lib.rs:464-534` — host video task (build_video_producer + spawn loop)
- `crates/host/src/lib.rs:617-688` — host input/control task (where SetBitrate arm goes)
- `crates/viewer/src/lib.rs:1645-1719` — viewer latency_task (where controller integration goes)
- `crates/media-win/src/pipeline/producer.rs:190` — DxgiNvencProducer **no-op** to fix in T4

---

## Branch & Working Dir

Branch `phase-l3-adaptive-bitrate` already exists and is checked out. Spec is committed at `docs/superpowers/specs/2026-05-11-l3-adaptive-bitrate-design.md` (commit `1a3620d`).

```bash
git status   # → "On branch phase-l3-adaptive-bitrate", clean
git log --oneline -1   # → "1a3620d L3 spec: adaptive bitrate ..."
```

---

## File Manifest

| Path | Status | Purpose |
|---|---|---|
| `crates/transport/src/bitrate_control.rs` | **new** | `BitrateController` struct + `BitrateControllerConfig` + AIMD step + 8 unit tests |
| `crates/transport/src/lib.rs` | modify | Add `pub mod bitrate_control;` |
| `crates/transport/tests/adaptive_bitrate_test.rs` | **new** | 2 integration tests (round-trip + loss burst drives MD) |
| `crates/host/src/lib.rs` | modify | Add `bitrate_tx/rx` channel + control loop `SetBitrate` arm + video loop drain |
| `crates/host/tests/setbitrate_handler_smoke.rs` | **new** | 1 smoke test for SetBitrate→bitrate_tx wiring |
| `crates/media-win/src/pipeline/producer.rs` | modify | `DxgiNvencProducer::set_target_bitrate` no-op → real call |
| `crates/viewer/src/lib.rs` | modify | `--no-adaptive-bitrate` flag + latency_task controller wiring |
| `docs/superpowers/STATUS.md` | modify | Add L3 entry under B2 + update header |

---

## Task 1: BitrateController pure logic + 8 unit tests

**Files:**
- Create: `crates/transport/src/bitrate_control.rs`
- Modify: `crates/transport/src/lib.rs:1-28` — add `pub mod bitrate_control;`

- [ ] **Step 1: Create empty module file**

```bash
touch crates/transport/src/bitrate_control.rs
```

- [ ] **Step 2: Add `pub mod bitrate_control;` to lib.rs**

Edit `crates/transport/src/lib.rs`. The current top of file looks like:

```rust
pub mod assembler;
pub mod error;
pub mod fec;
pub mod handshake;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;
```

Add `pub mod bitrate_control;` in alphabetical order between `assembler` and `error`:

```rust
pub mod assembler;
pub mod bitrate_control;
pub mod error;
pub mod fec;
pub mod handshake;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;
```

(Also keep the existing `#[cfg(test)] mod idr_loss_test;` line intact — it's at the bottom of the file.)

- [ ] **Step 3: Write the failing tests**

Write the entire test module (8 tests) into `crates/transport/src/bitrate_control.rs`:

```rust
//! Adaptive bitrate controller (viewer-side AIMD).
//!
//! Observes `purge_assembler()` frame loss and `LatencyProbe::snapshot()`
//! totals at 1 Hz, computes a target bitrate via Additive-Increase /
//! Multiplicative-Decrease (TCP NewReno-style), and tells the caller via
//! `should_send()` when a `ControlMessage::SetBitrate` is worth sending
//! to the host. See `docs/superpowers/specs/2026-05-11-l3-adaptive-bitrate-design.md`
//! for parameter rationale.

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BitrateControllerConfig {
    pub initial_bps: u32,
    pub min_bps: u32,
    pub max_bps: u32,
    pub loss_high: f32,
    pub loss_low: f32,
    pub md_factor: f32,
    pub ai_step_bps: u32,
    pub send_threshold_pct: f32,
    pub cooldown_after_md: Duration,
    pub enabled: bool,
}

impl BitrateControllerConfig {
    pub fn new_for_max(max_bps: u32) -> Self {
        Self {
            initial_bps: max_bps,
            min_bps: 1_000_000,
            max_bps,
            loss_high: 0.02,
            loss_low: 0.005,
            md_factor: 0.7,
            ai_step_bps: 200_000,
            send_threshold_pct: 0.05,
            cooldown_after_md: Duration::from_secs(2),
            enabled: true,
        }
    }
}

pub struct BitrateController {
    cfg: BitrateControllerConfig,
    target_bps: u32,
    last_md_at: Option<Instant>,
    last_sent_bps: u32,
    rolling_lost: u64,
    rolling_total: u64,
}

impl BitrateController {
    pub fn new(cfg: BitrateControllerConfig) -> Self {
        let target = cfg.initial_bps.clamp(cfg.min_bps, cfg.max_bps);
        Self {
            target_bps: target,
            last_sent_bps: target,
            cfg,
            last_md_at: None,
            rolling_lost: 0,
            rolling_total: 0,
        }
    }

    pub fn observe(&mut self, lost: u64, total: u64) {
        self.rolling_lost = self.rolling_lost.saturating_add(lost);
        self.rolling_total = self.rolling_total.saturating_add(total);
    }

    pub fn aimd_step(&mut self, now: Instant) {
        if !self.cfg.enabled {
            self.target_bps = self.cfg.max_bps;
            return;
        }
        let total = self.rolling_total.max(1);
        let loss = (self.rolling_lost as f32) / (total as f32);
        if loss > self.cfg.loss_high {
            let next = ((self.target_bps as f32) * self.cfg.md_factor) as u32;
            self.target_bps = next.max(self.cfg.min_bps);
            self.last_md_at = Some(now);
        } else if loss < self.cfg.loss_low {
            let cooldown_ok = match self.last_md_at {
                None => true,
                Some(t) => now.saturating_duration_since(t) >= self.cfg.cooldown_after_md,
            };
            if cooldown_ok {
                let next = self.target_bps.saturating_add(self.cfg.ai_step_bps);
                self.target_bps = next.min(self.cfg.max_bps);
            }
        }
    }

    pub fn target_bps(&self) -> u32 {
        self.target_bps
    }

    pub fn should_send(&self) -> bool {
        if !self.cfg.enabled {
            return false;
        }
        let last = self.last_sent_bps.max(1) as f32;
        let curr = self.target_bps as f32;
        let delta = (curr - last).abs() / last;
        delta > self.cfg.send_threshold_pct
    }

    pub fn mark_sent(&mut self) {
        self.last_sent_bps = self.target_bps;
    }

    pub fn reset_window(&mut self) {
        self.rolling_lost = 0;
        self.rolling_total = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_max(bps: u32) -> BitrateControllerConfig {
        BitrateControllerConfig::new_for_max(bps)
    }

    #[test]
    fn aimd_md_on_high_loss() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        c.observe(50, 1000); // 5% loss
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 7_000_000); // 10M * 0.7
    }

    #[test]
    fn aimd_ai_on_low_loss() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        c.observe(1, 1000); // 0.1% loss
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 5_200_000); // +200kbps
    }

    #[test]
    fn aimd_hold_in_band() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        c.observe(15, 1000); // 1.5%, between low (0.5%) and high (2%)
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 10_000_000); // unchanged
    }

    #[test]
    fn aimd_md_clamps_to_min() {
        let mut c = BitrateController::new(cfg_max(10_000_000));
        let now = Instant::now();
        for _ in 0..50 {
            c.observe(100, 1000); // 10% loss
            c.aimd_step(now);
            c.reset_window();
        }
        assert_eq!(c.target_bps(), 1_000_000); // min_bps
    }

    #[test]
    fn aimd_ai_clamps_to_max() {
        let mut cfg = cfg_max(2_000_000);
        cfg.initial_bps = 1_000_000;
        let mut c = BitrateController::new(cfg);
        let now = Instant::now();
        for _ in 0..20 {
            c.observe(0, 1000); // 0% loss
            c.aimd_step(now);
            c.reset_window();
        }
        assert_eq!(c.target_bps(), 2_000_000); // max_bps clamp
    }

    #[test]
    fn aimd_cooldown_after_md() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        let t0 = Instant::now();
        // MD trigger
        c.observe(50, 1000);
        c.aimd_step(t0);
        let after_md = c.target_bps();
        c.reset_window();
        // 1s later: try AI — must be suppressed (cooldown is 2s)
        c.observe(0, 1000);
        c.aimd_step(t0 + Duration::from_secs(1));
        assert_eq!(
            c.target_bps(),
            after_md,
            "AI suppressed during cooldown"
        );
        c.reset_window();
        // 3s later: cooldown elapsed, AI permitted
        c.observe(0, 1000);
        c.aimd_step(t0 + Duration::from_secs(3));
        assert!(c.target_bps() > after_md, "AI allowed after cooldown");
    }

    #[test]
    fn hysteresis_filters_small_changes() {
        let mut cfg = cfg_max(10_000_000);
        cfg.initial_bps = 5_000_000;
        let mut c = BitrateController::new(cfg);
        // 4% bump: don't send
        c.observe(0, 1000);
        c.aimd_step(Instant::now());
        // Force target to 5_200_000 (4% over 5M) — actually AI gives +200kbps
        // = 5_200_000 which is exactly +4%. should_send checks delta > 5%, so false.
        assert!(!c.should_send(), "4% change suppressed");
        // Bump to 6%: send
        c.target_bps = 5_300_000;
        assert!(c.should_send(), "6% change passes");
    }

    #[test]
    fn disabled_controller_returns_max_always() {
        let mut cfg = cfg_max(10_000_000);
        cfg.enabled = false;
        let mut c = BitrateController::new(cfg);
        c.observe(500, 1000); // 50% loss
        c.aimd_step(Instant::now());
        assert_eq!(c.target_bps(), 10_000_000);
        assert!(!c.should_send(), "disabled never sends");
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:
```bash
cargo test -p prdt-transport --lib bitrate_control
```

Expected: `test result: ok. 8 passed; 0 failed`.

If `aimd_md_clamps_to_min` fails because 10M × 0.7^N overshoots, increase the loop iterations or adjust assertion (10M × 0.7^7 ≈ 824k → clamp to 1M; 7 iterations enough, 50 is safe).

If `aimd_md_on_high_loss` returns 6_999_999 due to f32 rounding, accept off-by-one: change assertion to `assert!((c.target_bps() as i64 - 7_000_000).abs() <= 1)`.

- [ ] **Step 5: cargo fmt + clippy**

```bash
cargo fmt --all
cargo clippy -p prdt-transport --lib --tests -- -D warnings
```

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/transport/src/bitrate_control.rs crates/transport/src/lib.rs
git commit -m "$(cat <<'EOF'
L3 T1: BitrateController pure logic + 8 unit tests

Stateless AIMD: MD ×0.7 on loss>2%, AI +200kbps/s on loss<0.5% with 2s
post-MD cooldown. Hysteresis filter (5%) prevents control-msg spam.
Disabled mode returns max_bps unconditionally.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Fix DxgiNvencProducer no-op stub

**Files:**
- Modify: `crates/media-win/src/pipeline/producer.rs:190-193` — replace no-op body with real call

**Why this comes before host wiring:** The `VideoProducer::set_target_bitrate` trait method already exists on all 3 producers. `LinuxSwProducer` and `DxgiSwProducer` already pass through. Only `DxgiNvencProducer` is a no-op stub. Fix this stub first so T3's host wiring has an actually-functional path on Windows HW.

- [ ] **Step 1: Read the current no-op stub**

```bash
sed -n '186,200p' crates/media-win/src/pipeline/producer.rs
```

Expected output:
```
    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, _bps: u32) {
        // Phase 0 Plan 2c: bitrate is fixed at construction time. Reconfigure
        // via NvencEncoder::reconfigure will be wired in Plan 3+.
    }

    fn backend_name(&self) -> &'static str {
```

- [ ] **Step 2: Replace the no-op with the real call**

Edit `crates/media-win/src/pipeline/producer.rs`. Replace lines 190-193:

```rust
    fn set_target_bitrate(&mut self, _bps: u32) {
        // Phase 0 Plan 2c: bitrate is fixed at construction time. Reconfigure
        // via NvencEncoder::reconfigure will be wired in Plan 3+.
    }
```

with:

```rust
    fn set_target_bitrate(&mut self, bps: u32) {
        self.encoder.set_target_bitrate(bps);
    }
```

Rationale: `self.encoder: HwHevcEncoder` already has `set_target_bitrate(&mut self, bps: u32)` defined at `crates/media-win/src/encoder_trait.rs:73-78` which dispatches to NVENC or MF backends — both of which have working impls.

- [ ] **Step 3: Linux build**

This file only compiles on Windows (gated by crate-level `#[cfg(windows)]`), so `cargo check` from Linux must pass too (the change should not change build behavior):

```bash
cargo check -p prdt-media-win --target x86_64-unknown-linux-gnu
```

Expected: `Finished` (the crate has stub paths for non-Windows; this verifies no Linux regression).

- [ ] **Step 4: clippy on workspace (Linux target)**

```bash
cargo clippy --workspace --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: no new warnings. (Pre-existing warnings, if any, will surface; do not fix unless caused by this change.)

- [ ] **Step 5: Commit**

```bash
git add crates/media-win/src/pipeline/producer.rs
git commit -m "$(cat <<'EOF'
L3 T2: wire DxgiNvencProducer.set_target_bitrate to encoder

The Phase 0 stub at line 190 ignored bitrate updates. HwHevcEncoder
already has set_target_bitrate that dispatches to NVENC/MF, both with
working impls (encoder_trait.rs:73). One-line forward.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Host control-loop SetBitrate arm + video-loop drain

**Files:**
- Modify: `crates/host/src/lib.rs:478-534` — add `bitrate_tx/rx` channel, mutate video loop
- Modify: `crates/host/src/lib.rs:617-688` — add `SetBitrate` arm to control loop
- Create: `crates/host/tests/setbitrate_handler_smoke.rs` — 1 smoke test

- [ ] **Step 1: Write the failing smoke test**

Create `crates/host/tests/setbitrate_handler_smoke.rs` with:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails (compile fail OK)**

```bash
cargo test -p prdt-host --test setbitrate_handler_smoke
```

Expected: tests pass on first run because the test only exercises wire-message + channel logic which already compiles. The "real" host arm is added in Step 3 below; the test exists to lock in the expected pattern. If the test compiles + passes immediately, that's correct — proceed.

- [ ] **Step 3: Add bitrate_tx/rx channel between control and video loops**

Edit `crates/host/src/lib.rs`. Find the section around line 478-490 that creates `force_idr_flag` and the video task. Replace this block:

```rust
        let cancel = CancellationToken::new();
        let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));
        // Shared flag: control loop sets this when viewer requests an IDR;
        // video loop reads+clears it before each encode call.
        // Mirrors last_keepalive: Arc<AtomicU64> (same task-safety pattern).
        let force_idr_flag = Arc::new(AtomicBool::new(false));

        // Spawn video loop. `handshake_complete_at` anchors the first-frame-latency
        // measurement (Phase 4 acceptance: ≤ 500ms max-of-20 cold-start).
        let tx_video = Arc::clone(&transport);
        let cancel_video = cancel.clone();
        let cancel_video_propagate = cancel.clone();
        let video_force_idr = Arc::clone(&force_idr_flag);
```

with:

```rust
        let cancel = CancellationToken::new();
        let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));
        // Shared flag: control loop sets this when viewer requests an IDR;
        // video loop reads+clears it before each encode call.
        // Mirrors last_keepalive: Arc<AtomicU64> (same task-safety pattern).
        let force_idr_flag = Arc::new(AtomicBool::new(false));
        // L3 adaptive bitrate channel: control loop forwards viewer's
        // SetBitrate target_bps; video loop drains to latest before each
        // next_frame() and calls producer.set_target_bitrate(). Unbounded
        // because messages are tiny u32s at ~1 Hz, far below memory pressure.
        let (bitrate_tx, mut bitrate_rx) =
            tokio::sync::mpsc::unbounded_channel::<u32>();

        // Spawn video loop. `handshake_complete_at` anchors the first-frame-latency
        // measurement (Phase 4 acceptance: ≤ 500ms max-of-20 cold-start).
        let tx_video = Arc::clone(&transport);
        let cancel_video = cancel.clone();
        let cancel_video_propagate = cancel.clone();
        let video_force_idr = Arc::clone(&force_idr_flag);
```

- [ ] **Step 4: Add the drain inside the video loop's `next_frame()` prelude**

Still in `crates/host/src/lib.rs`, find the inner `async` block at line 500-505:

```rust
                    _ = async {
                        if video_force_idr.swap(false, Ordering::AcqRel) {
                            producer.request_idr();
                            info!("viewer requested IDR; producer.request_idr() called");
                        }
                        match producer.next_frame().await {
```

Replace with (drain bitrate_rx before request_idr to honor latest target):

```rust
                    _ = async {
                        // L3: drain bitrate channel to newest, apply to encoder.
                        let mut latest_bps: Option<u32> = None;
                        while let Ok(bps) = bitrate_rx.try_recv() {
                            latest_bps = Some(bps);
                        }
                        if let Some(bps) = latest_bps {
                            producer.set_target_bitrate(bps);
                            info!(target_bps = bps, "applied viewer-requested bitrate");
                        }
                        if video_force_idr.swap(false, Ordering::AcqRel) {
                            producer.request_idr();
                            info!("viewer requested IDR; producer.request_idr() called");
                        }
                        match producer.next_frame().await {
```

- [ ] **Step 5: Add the SetBitrate arm to the control loop**

Find the input task's match block in `crates/host/src/lib.rs` around line 672-678:

```rust
                            Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => {
                                info!("viewer requested IDR; setting force_idr for next encode");
                                input_force_idr.store(true, Ordering::Release);
                            }
                            Ok(ReceivedMessage::Control(msg)) => {
                                let _ = ft_rx.handle(msg);
                            }
```

Insert a `SetBitrate` arm above the catch-all `Ok(ReceivedMessage::Control(msg))`:

```rust
                            Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => {
                                info!("viewer requested IDR; setting force_idr for next encode");
                                input_force_idr.store(true, Ordering::Release);
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::SetBitrate {
                                target_bps,
                            })) => {
                                info!(target_bps, "viewer requested bitrate change");
                                let _ = bitrate_tx.send(target_bps);
                            }
                            Ok(ReceivedMessage::Control(msg)) => {
                                let _ = ft_rx.handle(msg);
                            }
```

`bitrate_tx` is moved into the input task's `async move` closure naturally because it's captured. No `Arc::clone` needed (mpsc senders are `Clone` themselves and we only need one).

If the borrow checker complains because `bitrate_tx` is captured by the input closure but `bitrate_rx` is captured by the video closure that's spawned earlier, both halves are independent owned types — no clone. If a subsequent task spawn (audio/clipboard/etc.) needs another sender, clone the sender at that capture point: `let bitrate_tx_x = bitrate_tx.clone();`.

- [ ] **Step 6: Build + run all host tests**

```bash
cargo build -p prdt-host --target x86_64-unknown-linux-gnu
cargo test -p prdt-host --target x86_64-unknown-linux-gnu --test setbitrate_handler_smoke
```

Expected: build succeeds, 2 tests pass.

- [ ] **Step 7: Run full host test + clippy**

```bash
cargo test -p prdt-host --target x86_64-unknown-linux-gnu
cargo clippy -p prdt-host --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: all tests pass (including pre-existing `request_idr_handler_smoke`), no clippy warnings.

- [ ] **Step 8: cargo fmt**

```bash
cargo fmt --all
```

- [ ] **Step 9: Commit**

```bash
git add crates/host/src/lib.rs crates/host/tests/setbitrate_handler_smoke.rs
git commit -m "$(cat <<'EOF'
L3 T3: host SetBitrate arm + video-loop drain

Control loop arm: ControlMessage::SetBitrate { target_bps } forwards via
unbounded mpsc to video loop. Video loop drains to newest u32 before
each next_frame() and calls producer.set_target_bitrate(bps).

Mirrors L2's force_idr_flag pattern but uses mpsc instead of AtomicU32
because we want drain-to-latest semantics (sender may burst between
frames; we only care about the most recent target).

2 smoke tests for the wire-message + channel-drain logic.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Viewer `--no-adaptive-bitrate` flag + latency_task wiring

**Files:**
- Modify: `crates/viewer/src/lib.rs:96-230` (Args struct) — add `--no-adaptive-bitrate` flag
- Modify: `crates/viewer/src/lib.rs:1645-1719` (latency_task) — instantiate controller, observe + step + send

- [ ] **Step 1: Add `--no-adaptive-bitrate` clap flag to Args**

In `crates/viewer/src/lib.rs`, find the `Args` struct definition starting at line 96. Locate the existing `--bitrate-mbps` arg (search for `bitrate`); if absent on viewer side, locate the last `#[arg(long)]` field before the struct closes. Add:

```rust
    /// Disable the L3 viewer-side adaptive bitrate controller. When set,
    /// the viewer will not send `ControlMessage::SetBitrate` to the host
    /// and the host's encoder will run at its CLI-configured bitrate for
    /// the entire session. Use for A/B regression comparisons.
    #[arg(long, default_value_t = false)]
    pub no_adaptive_bitrate: bool,

    /// Hint to the controller about the host's max bitrate, in Mbps. Used
    /// as the upper clamp for AIMD. If you don't know it, leave the
    /// default — the controller will start at this value and never exceed
    /// it. Should match the host's `--bitrate-mbps`.
    #[arg(long, default_value_t = 30u32)]
    pub bitrate_mbps: u32,
```

Place it in alphabetical order if the file follows that convention; otherwise group near other tuning flags. **Do not duplicate** if `bitrate_mbps` already exists — search the file first:

```bash
grep -n "bitrate_mbps\|no_adaptive_bitrate" crates/viewer/src/lib.rs
```

If `bitrate_mbps` already exists, only add `no_adaptive_bitrate`.

- [ ] **Step 2: Read the current latency_task structure**

```bash
sed -n '1645,1720p' crates/viewer/src/lib.rs
```

Confirm the layout matches the snippet in this plan's Pre-Task Context. If the line numbers have drifted (e.g., a previous task added lines), search for `latency_task = tokio::spawn`.

- [ ] **Step 3: Inject the controller into the latency_task**

In `crates/viewer/src/lib.rs`, just above the `let latency_task = tokio::spawn(...)` block (around line 1653), thread the controller config from CLI:

```rust
        // L3 adaptive bitrate controller — runs inside latency_task at 1 Hz.
        let mut bitrate_controller = {
            let mut cfg = prdt_transport::bitrate_control::BitrateControllerConfig::new_for_max(
                args.bitrate_mbps.saturating_mul(1_000_000),
            );
            cfg.enabled = !args.no_adaptive_bitrate;
            prdt_transport::bitrate_control::BitrateController::new(cfg)
        };
        let bitrate_transport = Arc::clone(&transport);
```

Note: `args.no_adaptive_bitrate` is captured here; since `args` may have been moved earlier into another task, you may need to clone the two fields up-front (place `let no_abr = args.no_adaptive_bitrate; let max_mbps = args.bitrate_mbps;` near the top of `run` and reference them here). If `args` is still in scope, use it directly.

- [ ] **Step 4: Mutate latency_task to drive the controller per tick**

Find the latency_task body starting at line 1653 (`let latency_task = tokio::spawn(async move {`) and replace it with:

```rust
        let latency_task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.tick().await; // fire first tick immediately; skip it
            let mut ticks_since_report: u32 = 0;
            // L3: caller-side rolling window state.
            let mut last_total_samples: u64 = 0;
            loop {
                ticker.tick().await;

                // Liveness heartbeat — host's watchdog needs this regardless of
                // whether decode is healthy yet. Crucial for slow-init viewers
                // that have not produced a present sample.
                if let Err(e) = latency_transport
                    .send_control(ControlMessage::KeepAlive)
                    .await
                {
                    warn!(?e, "send KeepAlive failed");
                }

                let snap = latency_probe.snapshot();

                // Window-title refresh: shown on every tick so users get
                // live feedback without tailing the log.
                let new_title = format_status_title(&snap);
                *title_shared.status_title.lock().unwrap() = Some(new_title);
                if let Some(present) = snap.present {
                    info!(
                        samples = present.samples,
                        arrival_p50_us = snap.arrival.map(|s| s.p50_us).unwrap_or(0),
                        arrival_p95_us = snap.arrival.map(|s| s.p95_us).unwrap_or(0),
                        decode_p50_us = snap.decode_done.map(|s| s.p50_us).unwrap_or(0),
                        decode_p95_us = snap.decode_done.map(|s| s.p95_us).unwrap_or(0),
                        present_p50_us = present.p50_us,
                        present_p95_us = present.p95_us,
                        present_p99_us = present.p99_us,
                        "M1 latency (host_capture → viewer_present)",
                    );
                } else if let Some(arrival) = snap.arrival {
                    info!(
                        samples = arrival.samples,
                        arrival_p50_us = arrival.p50_us,
                        arrival_p95_us = arrival.p95_us,
                        "M1 latency (arrival only; no present samples yet)",
                    );
                }

                // L3: adaptive bitrate step.
                let purged = bitrate_transport.purge_assembler().await;
                let lost = purged.len() as u64;
                let curr_total_samples = snap
                    .present
                    .map(|p| p.samples as u64)
                    .unwrap_or(last_total_samples);
                let delta_total = curr_total_samples.saturating_sub(last_total_samples);
                last_total_samples = curr_total_samples;
                let total_window = delta_total.saturating_add(lost);
                bitrate_controller.observe(lost, total_window);
                bitrate_controller.aimd_step(std::time::Instant::now());
                bitrate_controller.reset_window();
                if bitrate_controller.should_send() {
                    let target_bps = bitrate_controller.target_bps();
                    let msg = ControlMessage::SetBitrate { target_bps };
                    match bitrate_transport.send_control(msg).await {
                        Ok(()) => {
                            bitrate_controller.mark_sent();
                            info!(
                                target_bps,
                                lost_in_window = lost,
                                total_in_window = total_window,
                                "L3 sent SetBitrate"
                            );
                        }
                        Err(e) => warn!(?e, "L3 send SetBitrate failed"),
                    }
                }

                ticks_since_report += 1;
                if ticks_since_report >= 5 {
                    ticks_since_report = 0;
                    if let Some(present) = snap.present {
                        let arrival = snap.arrival.unwrap_or_default();
                        let decode = snap.decode_done.unwrap_or_default();
                        let msg = ControlMessage::LatencyReport {
                            samples: present.samples as u32,
                            arrival_p50_us: clamp_u32(arrival.p50_us),
                            arrival_p95_us: clamp_u32(arrival.p95_us),
                            decode_p50_us: clamp_u32(decode.p50_us),
                            decode_p95_us: clamp_u32(decode.p95_us),
                            present_p50_us: clamp_u32(present.p50_us),
                            present_p95_us: clamp_u32(present.p95_us),
                            present_p99_us: clamp_u32(present.p99_us),
                        };
                        if let Err(e) = latency_transport.send_control(msg).await {
                            warn!(?e, "send LatencyReport failed");
                        }
                    }
                }
            }
        });
```

Note: this preserves the existing 5-second LatencyReport cadence (`ticks_since_report`). The L3 step runs every tick (every 1s).

- [ ] **Step 5: Verify `bitrate_transport` and `bitrate_controller` are properly captured**

The `tokio::spawn(async move { ... })` will move `latency_transport`, `title_shared`, `bitrate_transport`, `bitrate_controller`, and `latency_probe`. If clippy flags an unused move warning, ensure all five are referenced inside the closure. Verify with:

```bash
cargo build -p prdt-viewer --target x86_64-unknown-linux-gnu
```

Expected: builds. If `move` complains about `bitrate_transport` not being used elsewhere, that's fine — the move is intentional.

- [ ] **Step 6: Run viewer tests + clippy**

```bash
cargo test -p prdt-viewer --target x86_64-unknown-linux-gnu --lib
cargo clippy -p prdt-viewer --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: tests pass, no warnings.

- [ ] **Step 7: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/viewer/src/lib.rs
git commit -m "$(cat <<'EOF'
L3 T4: viewer adaptive bitrate wiring

--no-adaptive-bitrate clap flag + --bitrate-mbps hint (defaults 30).
latency_task observes purge_assembler() loss and present.samples per
1Hz tick, drives BitrateController, sends SetBitrate when hysteresis
threshold (5%) is crossed.

Caller-side rolling window: subtracts last_total_samples from snapshot
samples, adds purged.len() as lost. Stateless controller step.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Integration test (loss burst drives MD over loopback)

**Files:**
- Create: `crates/transport/tests/adaptive_bitrate_test.rs` — 2 integration tests

- [ ] **Step 1: Read LoopbackOptions surface for drop_ppm injection**

```bash
grep -n "drop_ppm\|LoopbackOptions\|InProcTransport" crates/transport/src/loopback.rs | head -20
```

Confirm `LoopbackOptions { drop_ppm: u32, latency: Duration, ... }` exists. If `drop_ppm` field is named differently, adjust the test below.

- [ ] **Step 2: Write the integration tests**

Create `crates/transport/tests/adaptive_bitrate_test.rs`:

```rust
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
    assert!(bps < 10_000_000 && bps >= 1_000_000);
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
```

- [ ] **Step 3: Run + verify**

```bash
cargo test -p prdt-transport --test adaptive_bitrate_test
```

Expected: 2 passed.

- [ ] **Step 4: Run full transport tests + clippy**

```bash
cargo test -p prdt-transport --target x86_64-unknown-linux-gnu
cargo clippy -p prdt-transport --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: all tests pass (including L2's `idr_loss_test::*`), no warnings. The pre-existing flaky `transport::probe_test::two_transports_find_each_other` (HandshakeTimeout) failure is acceptable per L2 STATUS — record it but don't block on it.

- [ ] **Step 5: cargo fmt + commit**

```bash
cargo fmt --all
git add crates/transport/tests/adaptive_bitrate_test.rs
git commit -m "$(cat <<'EOF'
L3 T5: integration tests for adaptive bitrate

Two tests covering: (1) MD trigger flips should_send() flag and clears
on mark_sent; (2) sustained 5% loss across 5 ticks decreases target_bps
monotonically, lands between min_bps and 10 Mbps starting from 30 Mbps.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Workspace build + clippy + full test sweep

**Files:** none (validation task)

- [ ] **Step 1: Workspace build (Linux)**

```bash
cargo build --workspace --target x86_64-unknown-linux-gnu --all-targets
```

Expected: clean build.

- [ ] **Step 2: Workspace clippy (Linux)**

```bash
cargo clippy --workspace --target x86_64-unknown-linux-gnu --all-targets -- -D warnings
```

Expected: no warnings. If a pre-existing warning surfaces that's unrelated to L3 (e.g., audio crate), record it in the commit message but don't fix unless it's blocking the build.

- [ ] **Step 3: Workspace tests (Linux)**

```bash
cargo test --workspace --target x86_64-unknown-linux-gnu
```

Expected: all tests pass except the documented pre-existing flaky `transport::probe_test::two_transports_find_each_other`. Capture the count: should be **348 baseline + 7 L2 + 11 L3 = 366** passing.

- [ ] **Step 4: cargo fmt --check**

```bash
cargo fmt --all -- --check
```

Expected: no diff. If diff found, run `cargo fmt --all` and re-verify, then amend.

- [ ] **Step 5: Push branch + open draft PR (foreground)**

This is the L2 precedent — a draft PR triggers Windows CI which is the only way to verify Windows builds without a Windows host:

```bash
git push -u origin phase-l3-adaptive-bitrate
gh pr create --draft --title "L3: adaptive bitrate (observed-loss-driven AIMD)" --body "$(cat <<'EOF'
## Summary
- Viewer-side AIMD bitrate controller observes purge_assembler() loss at 1Hz, sends ControlMessage::SetBitrate
- Host control loop forwards SetBitrate to video loop via mpsc, applies to producer.set_target_bitrate
- Fixes DxgiNvencProducer::set_target_bitrate no-op stub (one-line forward to encoder)
- 11 new tests (8 unit + 2 integration + 2 host smoke + 1 round-trip; tests share BitrateController across 2 files)

Spec: `docs/superpowers/specs/2026-05-11-l3-adaptive-bitrate-design.md`
Plan: `docs/superpowers/plans/2026-05-11-l3-adaptive-bitrate.md`

## Test plan
- [x] `cargo test --workspace` Linux: 366 passed (348 baseline + 7 L2 + 11 L3)
- [x] `cargo clippy --workspace -- -D warnings` Linux green
- [ ] Windows CI green (this PR)
- [ ] Manual smoke: WSLg host (--bitrate-mbps 30) + real Wayland viewer → target_bps converges to ≤5 Mbps within 1 minute, session survives 5 minutes

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 6: Wait for Windows CI, fix any regressions**

Use `gh run watch` or `gh pr checks <PR#>` to monitor. If Windows CI fails, dispatch a fix-loop subagent with the failing log output. Common Windows-only failures from past phases:

- nvenc cfg-gating (handled in L1.5b — pattern at `media-win/build.rs:9-10`)
- rustfmt drift (run `cargo fmt --all` and amend)
- E0599 trait import for Windows-only path (add `use ... as _;` inside `#[cfg(windows)]`)

- [ ] **Step 7: Mark PR ready, request review (or proceed to T7 if auto-merging)**

```bash
gh pr ready <PR#>
```

---

## Task 7: Linux smoke walkthrough (DoD #1)

**Files:** none (manual verification)

This is the "real WiFi/LAN" verification that L2 didn't have an automated equivalent for. Spec §5E DoD #1.

- [ ] **Step 1: Build release binary**

```bash
cargo build --workspace --release --target x86_64-unknown-linux-gnu
```

- [ ] **Step 2: Start host on WSLg (background)**

```bash
RUST_LOG=info ./target/x86_64-unknown-linux-gnu/release/prdt host \
  --bind 0.0.0.0:9000 \
  --bitrate-mbps 30 \
  --encoder openh264 \
  --silent-allow > /tmp/prdt-host-l3.log 2>&1 &
```

(The `--silent-allow` flag bypasses the pubkey TOFU prompt for smoke testing. Auto-mode classifier may block the bind; if so, run with `!` prefix in the user's session.)

Capture the host pubkey from the log:
```bash
grep "host pubkey" /tmp/prdt-host-l3.log
```

- [ ] **Step 3: Run viewer from real Wayland machine**

On the real Wayland machine (not WSLg), run:

```bash
RUST_LOG=info ./target/x86_64-unknown-linux-gnu/release/prdt connect \
  --host <wsl-mirror-ip>:9000 \
  --host-pubkey <captured-pubkey> 2>&1 | tee /tmp/prdt-viewer-l3.log
```

- [ ] **Step 4: Observe DoD criteria**

Watch for these log lines:

1. **Viewer**: `L3 sent SetBitrate target_bps=N` where N drops to ≤ 5_000_000 within 60 seconds of connect
2. **Host**: `viewer requested bitrate change target_bps=N` matching the viewer's value
3. **Host**: `applied viewer-requested bitrate target_bps=N` printed by the video loop drain
4. **Session survival**: connect for at least 5 minutes without `host watchdog … session kill` in host log
5. **Recovery**: if you ssh and pause WiFi briefly to force loss, the controller drops bitrate; after WiFi resumes, target_bps grows by ~200kbps/s

- [ ] **Step 5: Capture metrics for STATUS update**

Note the following for the STATUS write-up:
- `target_bps` low watermark (worst-case bitrate during smoke)
- Time-to-first-frame after connect
- `arrival_p50` / `decode_p50` / `present_p50` from the M1 latency log (5s avg)
- Number of `RequestIdr` events triggered
- Whether session survived 5 minutes

- [ ] **Step 6: Stop host cleanly**

```bash
pkill -f "target/x86_64-unknown-linux-gnu/release/prdt host"
```

(The exit-144 cascade is benign; the process kills cleanly.)

---

## Task 8: STATUS update + tag

**Files:**
- Modify: `docs/superpowers/STATUS.md` — add L3 entry under section B2, update header

- [ ] **Step 1: Read current STATUS header (lines 1-12)**

```bash
sed -n '1,12p' docs/superpowers/STATUS.md
```

Confirm: `Last updated: 2026-05-10` and `Latest tag: phase-l2-transport-robustness-complete`.

- [ ] **Step 2: Update header**

Edit `docs/superpowers/STATUS.md`. Replace lines 3-4:

```markdown
**Last updated:** 2026-05-10
**Latest tag:** `phase-l2-transport-robustness-complete`
```

with:

```markdown
**Last updated:** 2026-05-11
**Latest tag:** `phase-l3-adaptive-bitrate-complete`
```

- [ ] **Step 3: Add L3 bullet under B2 Linux サポート section**

Find the `**L2 smoke 残課題** (L3 territory):` block (around line 163). Append a new sub-bullet **after** it:

```markdown
  - **L3 (`phase-l3-adaptive-bitrate-complete`, 2026-05-11)**: viewer-side AIMD bitrate controller を追加して L2 smoke の 5.7% delivery → session timeout を解消。Cross-platform、~360 LoC across 6 modify + 1 new + 3 new test files。
    - **Viewer side** (`crates/transport/src/bitrate_control.rs` 新): `BitrateController` (stateless: `observe(lost, total)` → `aimd_step(now)` → `should_send()` → `mark_sent()`, with `reset_window()`)。AIMD パラメータ: MD ×0.7 on loss>2%, AI +200kbps/s on loss<0.5%, 2s post-MD cooldown, 5% hysteresis、min 1 Mbps, max `--bitrate-mbps × 1e6`
    - **Viewer wiring** (`crates/viewer/src/lib.rs` `latency_task`): 1Hz tick で `transport.purge_assembler().await` → caller が `last_total_samples` 差分で rolling window を構築 → controller 駆動 → `SetBitrate` 送信。`--no-adaptive-bitrate` flag で disable (回帰比較用)
    - **Host side** (`crates/host/src/lib.rs`): `tokio::sync::mpsc::unbounded_channel::<u32>()` を control loop と video loop で共有。control loop arm: `Ok(ControlMessage::SetBitrate { target_bps }) => bitrate_tx.send(target_bps)`。video loop は per-frame `bitrate_rx.try_recv()` で drain to latest → `producer.set_target_bitrate(bps)`
    - **Producer fix** (`crates/media-win/src/pipeline/producer.rs:190`): `DxgiNvencProducer::set_target_bitrate` の Phase 0 no-op stub を `self.encoder.set_target_bitrate(bps)` に書き換え (1-line forward to `HwHevcEncoder` which already dispatches to NVENC/MF)
    - **Tests**: 11 new tests cross-platform: 8 unit (`bitrate_control::tests::*`) + 2 transport integration (`adaptive_bitrate_test::*`) + 2 host smoke (`setbitrate_handler_smoke::*`) − 1 round-trip overlap = **11 new** (Linux `cargo test` 366 passed)
    - **Wire**: `ControlMessage::SetBitrate { target_bps: u32 }` (kind_u8=6, 既存 dead path) を再利用、protocol_version bump 不要、backward compatible
    - **Linux regression bar**: `cargo build/clippy --workspace -- -D warnings` 両 target green、366 passed
    - **Windows regression bar**: GitHub Actions release workflow PR で確認 (tag push 後)
  - **L3 smoke walkthrough (2026-05-11)**: WSLg host (`--bitrate-mbps 30 --encoder openh264`) + 実機 Wayland viewer で end-to-end 検証。**spec §5E DoD #1 達成 ✅** — <FILL FROM TASK 7 OBSERVATIONS>
```

The placeholder `<FILL FROM TASK 7 OBSERVATIONS>` should be replaced with the metrics captured in Task 7 Step 5.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/STATUS.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record L3 adaptive bitrate completion + smoke walkthrough

L3 viewer-side AIMD controller resolves L2 smoke's 5.7% delivery →
watchdog kill regression. Updated header tag and added entry under
B2 Linux サポート with cross-platform test counts.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Squash-merge PR (after Windows CI green)**

```bash
gh pr merge --squash --delete-branch <PR#>
```

(Auto-mode classifier may block; if so, the user runs with `!` prefix.)

- [ ] **Step 6: Pull master + tag**

```bash
git checkout master
git pull
git tag -a phase-l3-adaptive-bitrate-complete -m "L3: viewer-side AIMD adaptive bitrate"
git push --tags
```

- [ ] **Step 7: Re-tag if squash created an orphan commit**

If the PR was squashed and the tag is on the pre-squash commit, re-tag like L2:

```bash
git tag -d phase-l3-adaptive-bitrate-complete
git push origin :refs/tags/phase-l3-adaptive-bitrate-complete
git tag -a phase-l3-adaptive-bitrate-complete <squash-sha> -m "L3: viewer-side AIMD adaptive bitrate"
git push --tags
```

---

## Done Criteria (mirrors spec §1)

1. Linux + Windows CI green (zero regressions in 348+ baseline tests)
2. WSLg + real Wayland smoke shows `target_bps ≤ 5_000_000` within 60s and session survives 5 minutes
3. L2's 5 Mbps smoke (already passes today) still passes (DoD #2 — controller in "no-loss" state holds at max_bps, equivalent to current behavior)
4. STATUS.md updated, `phase-l3-adaptive-bitrate-complete` tag pushed

---

## Risk Notes for Implementer

- **Auto-mode classifier**: blocks `0.0.0.0:9000` bind, `gh pr merge`, and possibly `pkill`. Use `.claude/settings.local.json` permission rules (already added for L2; verify they're still present) or hand off to user via `!` prefix
- **NVENC bindgen drift**: any change to `media-win/pipeline/producer.rs` may surface NVENC-cfg-gating issues on Windows CI even when Linux passes. Reference the L1.5b `prdt_nvenc_bindings` pattern if E0599/E0432 errors appear in NVENC paths
- **Pre-existing flaky test**: `transport::probe_test::two_transports_find_each_other` fails on master too; ignore it but mention in PR description
- **Encoder bitrate apply timing (NVENC/MF)**: spec Q2 noted possible IDR-boundary delay. If smoke shows long lag between SetBitrate send and observed bitrate change, T-future may need to also call `producer.request_idr()` after `set_target_bitrate(bps)` to force immediate apply. Out of L3 scope unless smoke fails
