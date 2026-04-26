# Plan 4 B4 Network Profile Bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `prdt-net-profile-bench` bin that sweeps `(latency_ms × drop_ppm)` profiles via `LoopbackOptions` and reports per-profile InputEvent + Video send→recv lag and message-level loss.

**Architecture:** Near-clone of B7's `prdt-input-load-bench` template. New bin in `crates/latency-bench/src/bin/net-profile-bench.rs`. Reuses `InProcTransport` 3-task pattern (input sender, video sender, receiver) with `LoopbackOptions { latency, drop_ppm }` populated per config. Adds video sent/received counters since drop_ppm now matters for video too.

**Tech Stack:** Rust 2021, `clap` derive, `tokio` (rt-multi-thread + sync + time), `tracing`, `humantime`, `tokio-util` (CancellationToken). All deps already in `latency-bench/Cargo.toml` from B7.

**Spec:** `docs/superpowers/specs/2026-04-26-plan4-b4-net-profile-design.md`

---

## File Structure

**Created files:**

```
crates/latency-bench/src/bin/
  net-profile-bench.rs           ← new prdt-net-profile-bench bin
                                   CLI + matrix expand + per-config
                                   sender/receiver tasks + aggregate
                                   + summary CSV + 5 unit tests
docs/
  net-profile-bench.md           ← usage + schema + profile presets
                                   + interpretation
```

**Modified files:**

```
crates/latency-bench/Cargo.toml + [[bin]] entry for prdt-net-profile-bench
                                  (no new deps; B7 added all required ones)
```

---

## Verified API (from current code, 2026-04-26)

```rust
// prdt_transport::loopback
pub struct LoopbackOptions {
    pub drop_ppm: u32,
    pub latency: Option<Duration>,
}
// Send path semantics (loopback.rs:64-75):
//   1. should_drop() check; on drop, silently return Ok(()) (no send)
//   2. if Some(d) = latency, sleep(d) BLOCKS sender
//   3. send via mpsc; receiver eventually receives
// Therefore:
//   - drop is silent: sender always sees Ok, receiver sees fewer messages
//   - latency blocks the sender; sender's effective rate == min(configured, 1/latency)

pub use loopback::{InProcTransport, LoopbackOptions};
pub use transport_trait::{ReceivedMessage, Transport};

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError>;
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<ReceivedMessage, TransportError>;
}

pub enum ReceivedMessage {
    Video(EncodedFrame),
    Audio(AudioPacket),
    Input(InputEvent),
    Control(ControlMessage),
}

// prdt_protocol
pub enum InputEvent { MouseMove { x: i32, y: i32, absolute: bool }, ... }
pub fn now_monotonic_us() -> u64;
pub struct EncodedFrame {
    pub seq: u64,
    pub timestamp_host_us: u64,
    pub is_keyframe: bool,
    pub nal_units: bytes::Bytes,
    pub width: u32,
    pub height: u32,
    pub codec: prdt_protocol::frame::Codec, // ::H265
}

// prdt_latency_bench (lib.rs)
pub fn percentiles(lags_us: &mut [u64]) -> (u64, u64, u64, u64, u64);
```

---

## Task 1: Bin scaffold + CLI + dry-run

**Files:**
- Modify: `crates/latency-bench/Cargo.toml`
- Create: `crates/latency-bench/src/bin/net-profile-bench.rs`

- [ ] **Step 1: Update Cargo.toml — add 5th `[[bin]]`**

In `crates/latency-bench/Cargo.toml`, after the existing `[[bin]] prdt-input-load-bench` block, append:

```toml

[[bin]]
name = "prdt-net-profile-bench"
path = "src/bin/net-profile-bench.rs"
```

No new deps; B7 added `tokio-util`. Verify with:
```bash
grep -n "tokio-util" crates/latency-bench/Cargo.toml
```
Expected: one match (already present).

- [ ] **Step 2: Create the scaffold file**

Create `crates/latency-bench/src/bin/net-profile-bench.rs`:

```rust
//! Plan 4 B4 Network Profile bench. Sweeps (latency_ms x drop_ppm)
//! profiles using `LoopbackOptions` to simulate one-way delay and
//! message-level drop on top of the existing `InProcTransport`.
//! Reports per-profile InputEvent + Video send-to-recv lag and
//! message-level loss.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "prdt-net-profile-bench",
    about = "Plan 4 B4 network-profile bench: (latency x drop) sweep over LoopbackOptions"
)]
struct Args {
    /// Output dir. Will contain `summary.csv`. Created if missing;
    /// existing files are overwritten.
    #[arg(long)]
    out_dir: PathBuf,

    /// Comma-separated one-way delays in milliseconds.
    #[arg(long, value_delimiter = ',', default_values_t = vec![0u32, 1u32, 10u32, 50u32, 200u32])]
    latencies_ms: Vec<u32>,

    /// Comma-separated per-message drop ppm.
    #[arg(long, value_delimiter = ',', default_values_t = vec![0u32, 1_000u32, 10_000u32, 50_000u32])]
    drops_ppm: Vec<u32>,

    /// Fixed input rate (Hz).
    #[arg(long, default_value_t = 1000u32)]
    input_rate_hz: u32,

    /// Fixed video rate (fps).
    #[arg(long, default_value_t = 60u32)]
    video_rate_fps: u32,

    /// Synthetic video frame size in bytes.
    #[arg(long, default_value_t = 50_000usize)]
    video_frame_bytes: usize,

    /// Per-config bench duration.
    #[arg(long, default_value = "5s")]
    duration: humantime::Duration,

    /// Spacing between configs (ms).
    #[arg(long, default_value_t = 250u64)]
    inter_config_delay_ms: u64,

    /// Print the matrix and exit.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // used in Tasks 2-4
struct Cfg {
    latency_ms: u32,
    drop_ppm: u32,
    input_rate_hz: u32,
    video_rate_fps: u32,
    video_frame_bytes: usize,
    duration: Duration,
}

fn config_id(cfg: &Cfg) -> String {
    format!("lat{}ms-drop{}ppm", cfg.latency_ms, cfg.drop_ppm)
}

fn expand_matrix(args: &Args) -> Vec<Cfg> {
    let mut out = Vec::with_capacity(args.latencies_ms.len() * args.drops_ppm.len());
    for &latency_ms in &args.latencies_ms {
        for &drop_ppm in &args.drops_ppm {
            out.push(Cfg {
                latency_ms,
                drop_ppm,
                input_rate_hz: args.input_rate_hz,
                video_rate_fps: args.video_rate_fps,
                video_frame_bytes: args.video_frame_bytes,
                duration: Duration::from(args.duration),
            });
        }
    }
    out
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let configs = expand_matrix(&args);
    info!(count = configs.len(), "matrix expanded");

    if args.dry_run {
        for (i, c) in configs.iter().enumerate() {
            println!("[{:>3}/{}] {}", i + 1, configs.len(), config_id(c));
        }
        return Ok(());
    }

    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("create out_dir {}", args.out_dir.display()))?;

    // Trial loop + summary CSV come in Tasks 2-4.
    anyhow::bail!("trial loop not yet implemented (Tasks 2-4)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_id_format_canonical() {
        let c = Cfg {
            latency_ms: 0,
            drop_ppm: 0,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        assert_eq!(config_id(&c), "lat0ms-drop0ppm");
        let c = Cfg {
            latency_ms: 200,
            drop_ppm: 50_000,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        assert_eq!(config_id(&c), "lat200ms-drop50000ppm");
    }

    #[test]
    fn expand_matrix_cartesian() {
        let args = Args {
            out_dir: PathBuf::from("/tmp/fake"),
            latencies_ms: vec![0, 10],
            drops_ppm: vec![0, 1000],
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: humantime::Duration::from(Duration::from_secs(5)),
            inter_config_delay_ms: 250,
            dry_run: true,
        };
        let cfgs = expand_matrix(&args);
        assert_eq!(cfgs.len(), 4); // 2 * 2
        // Order: latency outer, drop inner
        assert_eq!((cfgs[0].latency_ms, cfgs[0].drop_ppm), (0, 0));
        assert_eq!((cfgs[1].latency_ms, cfgs[1].drop_ppm), (0, 1000));
        assert_eq!((cfgs[2].latency_ms, cfgs[2].drop_ppm), (10, 0));
        assert_eq!((cfgs[3].latency_ms, cfgs[3].drop_ppm), (10, 1000));
    }
}
```

- [ ] **Step 3: Build + dry-run smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -3
cargo run -p prdt-latency-bench --bin prdt-net-profile-bench -- --out-dir /tmp/dry --dry-run 2>/dev/null | wc -l
```

Expected: clean build; dry-run prints exactly **20** stdout lines (5 latencies × 4 drops).

- [ ] **Step 4: Tests + clippy**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -10
cargo clippy -p prdt-latency-bench --bin prdt-net-profile-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 2 tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/Cargo.toml \
        crates/latency-bench/src/bin/net-profile-bench.rs
git commit -m "net-profile-bench: scaffold bin with CLI + dry-run + matrix expansion"
```

---

## Task 2: `run_one_config` (sender/receiver tasks)

**Files:**
- Modify: `crates/latency-bench/src/bin/net-profile-bench.rs`

This task adds the trial runner. Compared to B7's run_one_config, it:
- Reads `cfg.latency_ms` + `cfg.drop_ppm` and builds `LoopbackOptions` accordingly
- Counts video sent/received in addition to input

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn run_one_config_no_latency_no_drop_baseline() {
        // 1000 Hz input + 60 fps video for 200 ms, no latency, no drop.
        let cfg = Cfg {
            latency_ms: 0,
            drop_ppm: 0,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 1024,
            duration: Duration::from_millis(200),
        };
        let stats = run_one_config(&cfg).await;
        assert!(stats.input_sent >= 100, "expected ~200, got {}", stats.input_sent);
        assert_eq!(stats.input_received, stats.input_sent, "no drops at drop_ppm=0");
        assert!(stats.video_sent >= 5);
        assert_eq!(stats.video_received, stats.video_sent, "no drops at drop_ppm=0");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_one_config_drop_ppm_loses_messages() {
        // 200_000 ppm = 20% drop. Over 100+ messages we should observe a
        // measurable loss > 0.
        let cfg = Cfg {
            latency_ms: 0,
            drop_ppm: 200_000,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 1024,
            duration: Duration::from_millis(200),
        };
        let stats = run_one_config(&cfg).await;
        assert!(stats.input_sent >= 100);
        assert!(
            stats.input_received < stats.input_sent,
            "expected some drops at 20% drop_ppm, got received={} sent={}",
            stats.input_received,
            stats.input_sent
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -10
```

Expected: compile errors — `cannot find function run_one_config`, `cannot find type RunStats`.

- [ ] **Step 3: Implement run_one_config + RunStats**

In `crates/latency-bench/src/bin/net-profile-bench.rs`, between the `expand_matrix` function and the `main` function, add:

```rust
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use prdt_protocol::{frame::Codec, now_monotonic_us, EncodedFrame, InputEvent};
use prdt_transport::{
    loopback::{InProcTransport, LoopbackOptions},
    ReceivedMessage, Transport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
#[allow(dead_code)] // used in Task 3
struct RunStats {
    input_sent: u64,
    input_received: u64,
    input_lags: Vec<u64>,
    video_sent: u64,
    video_received: u64,
}

#[allow(dead_code)] // wired in Task 4
async fn run_one_config(cfg: &Cfg) -> RunStats {
    let opts = LoopbackOptions {
        drop_ppm: cfg.drop_ppm,
        latency: if cfg.latency_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(cfg.latency_ms as u64))
        },
    };
    let (host_side, viewer_side) = InProcTransport::pair(opts);
    let host_side = Arc::new(host_side);
    let viewer_side = Arc::new(viewer_side);

    let (sent_ts_tx, mut sent_ts_rx) = mpsc::unbounded_channel::<u64>();
    let cancel = CancellationToken::new();

    // ---- Input sender ----
    let send_input = {
        let viewer_side = Arc::clone(&viewer_side);
        let cancel = cancel.clone();
        let interval = Duration::from_secs_f64(1.0 / cfg.input_rate_hz as f64);
        tokio::spawn(async move {
            let mut count: u64 = 0;
            let mut next = Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep_until(next.into()) => {}
                }
                let now = now_monotonic_us();
                if sent_ts_tx.send(now).is_err() {
                    break;
                }
                let ev = InputEvent::MouseMove { x: 0, y: 0, absolute: false };
                if viewer_side.send_input(ev).await.is_err() {
                    break;
                }
                count += 1;
                next += interval;
            }
            count
        })
    };

    // ---- Video sender ----
    let send_video = {
        let viewer_side = Arc::clone(&viewer_side);
        let cancel = cancel.clone();
        let interval = Duration::from_secs_f64(1.0 / cfg.video_rate_fps as f64);
        let frame_bytes = cfg.video_frame_bytes;
        tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut next = Instant::now();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep_until(next.into()) => {}
                }
                let frame = EncodedFrame {
                    seq,
                    timestamp_host_us: now_monotonic_us(),
                    is_keyframe: seq % 30 == 0,
                    nal_units: Bytes::from(vec![0u8; frame_bytes]),
                    width: 1920,
                    height: 1080,
                    codec: Codec::H265,
                };
                if viewer_side.send_video(frame).await.is_err() {
                    break;
                }
                seq += 1;
                next += interval;
            }
            seq
        })
    };

    // ---- Receiver ----
    let recv_task = {
        let host_side = Arc::clone(&host_side);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut input_received: u64 = 0;
            let mut video_received: u64 = 0;
            let mut input_lags: Vec<u64> = Vec::new();
            // ---- Phase 1: until cancel ----
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    msg = host_side.recv() => {
                        match msg {
                            Ok(ReceivedMessage::Input(_)) => {
                                if let Ok(sent_us) = sent_ts_rx.try_recv() {
                                    let lag = now_monotonic_us().saturating_sub(sent_us);
                                    input_lags.push(lag);
                                }
                                input_received += 1;
                            }
                            Ok(ReceivedMessage::Video(_)) => {
                                video_received += 1;
                            }
                            Ok(_) => {} // discard audio / control
                            Err(_) => break,
                        }
                    }
                }
            }
            // ---- Phase 2: drain in-flight (50 ms cap) ----
            let drain_until = Instant::now() + Duration::from_millis(50);
            loop {
                let remaining = drain_until.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, host_side.recv()).await {
                    Ok(Ok(ReceivedMessage::Input(_))) => {
                        if let Ok(sent_us) = sent_ts_rx.try_recv() {
                            let lag = now_monotonic_us().saturating_sub(sent_us);
                            input_lags.push(lag);
                        }
                        input_received += 1;
                    }
                    Ok(Ok(ReceivedMessage::Video(_))) => {
                        video_received += 1;
                    }
                    Ok(Ok(_)) => {}
                    _ => break,
                }
            }
            (input_received, video_received, input_lags)
        })
    };

    // ---- Wait, then cancel ----
    tokio::time::sleep(cfg.duration).await;
    cancel.cancel();

    let input_sent = send_input.await.unwrap_or(0);
    let video_sent = send_video.await.unwrap_or(0);
    let (input_received, video_received, input_lags) = recv_task.await.unwrap_or_default();

    RunStats {
        input_sent,
        input_received,
        input_lags,
        video_sent,
        video_received,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -10
cargo clippy -p prdt-latency-bench --bin prdt-net-profile-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 4 tests pass total (2 prior sync + 2 new async); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/net-profile-bench.rs
git commit -m "net-profile-bench: implement run_one_config (input + video senders + receiver)"
```

---

## Task 3: Aggregate + CSV writer

**Files:**
- Modify: `crates/latency-bench/src/bin/net-profile-bench.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn aggregate_with_video_loss() {
        let cfg = Cfg {
            latency_ms: 50,
            drop_ppm: 10_000,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        let stats = RunStats {
            input_sent: 5000,
            input_received: 4950,
            input_lags: (50_000..=50_049u64).collect(), // 50 entries, each 50ms-ish
            video_sent: 300,
            video_received: 297,
        };
        let s = aggregate(&cfg, &stats);
        assert_eq!(s.config_id, "lat50ms-drop10000ppm");
        assert_eq!(s.input_sent, 5000);
        assert_eq!(s.input_received, 4950);
        // 50 lost out of 5000 = 10000 ppm
        assert_eq!(s.input_loss_ppm, 10_000);
        assert_eq!(s.video_sent, 300);
        assert_eq!(s.video_received, 297);
        // 3 lost out of 300 = 10000 ppm
        assert_eq!(s.video_loss_ppm, 10_000);
        // p50 of 50 lags 50_000..=50_049 (round picking, 50 elements):
        // idx = round((50-1) * 0.5) = round(24.5) = 25 -> v[25] = 50_025
        assert_eq!(s.input_p50_us, 50_025);
    }

    #[test]
    fn aggregate_empty_input_lags_emits_zero_percentiles() {
        let cfg = Cfg {
            latency_ms: 0,
            drop_ppm: 0,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        let stats = RunStats {
            input_sent: 0,
            input_received: 0,
            input_lags: vec![],
            video_sent: 0,
            video_received: 0,
        };
        let s = aggregate(&cfg, &stats);
        assert_eq!(s.input_loss_ppm, 0);
        assert_eq!(s.video_loss_ppm, 0);
        assert_eq!(s.input_p50_us, 0);
        assert_eq!(s.input_p95_us, 0);
        assert_eq!(s.input_p99_us, 0);
    }

    #[test]
    fn summary_csv_writer_emits_header_and_one_row() {
        let cfg = Cfg {
            latency_ms: 10,
            drop_ppm: 1000,
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        let s = ConfigStats {
            config_id: config_id(&cfg),
            latency_ms: cfg.latency_ms,
            drop_ppm: cfg.drop_ppm,
            input_rate_hz: cfg.input_rate_hz,
            video_rate_fps: cfg.video_rate_fps,
            duration_ms: cfg.duration.as_millis() as u64,
            input_sent: 5000,
            input_received: 4995,
            input_loss_ppm: 1000,
            input_p50_us: 10_005,
            input_p95_us: 10_028,
            input_p99_us: 10_054,
            video_sent: 300,
            video_received: 300,
            video_loss_ppm: 0,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.csv");
        write_summary_csv(&path, std::slice::from_ref(&s)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row");
        assert_eq!(
            lines[0],
            "config_id,latency_ms,drop_ppm,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us,video_sent,video_received,video_loss_ppm"
        );
        assert_eq!(
            lines[1],
            "lat10ms-drop1000ppm,10,1000,1000,60,5000,5000,4995,1000,10005,10028,10054,300,300,0"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -15
```

Expected: compile errors — `cannot find type ConfigStats`, `cannot find function aggregate`, `cannot find function write_summary_csv`.

- [ ] **Step 3: Implement ConfigStats + aggregate + write_summary_csv**

In `crates/latency-bench/src/bin/net-profile-bench.rs`, between the `run_one_config` function and the `main` function, add:

```rust
#[allow(dead_code)] // wired in Task 4
struct ConfigStats {
    config_id: String,
    latency_ms: u32,
    drop_ppm: u32,
    input_rate_hz: u32,
    video_rate_fps: u32,
    duration_ms: u64,
    input_sent: u64,
    input_received: u64,
    input_loss_ppm: u64,
    input_p50_us: u64,
    input_p95_us: u64,
    input_p99_us: u64,
    video_sent: u64,
    video_received: u64,
    video_loss_ppm: u64,
}

#[allow(dead_code)] // wired in Task 4
fn aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats {
    let (input_p50_us, input_p95_us, input_p99_us) = if stats.input_lags.is_empty() {
        (0, 0, 0)
    } else {
        let mut lags = stats.input_lags.clone();
        let (p50, _, p95, p99, _) = prdt_latency_bench::percentiles(&mut lags);
        (p50, p95, p99)
    };
    let input_loss_ppm = if stats.input_sent > 0 {
        stats.input_sent.saturating_sub(stats.input_received) * 1_000_000
            / stats.input_sent
    } else {
        0
    };
    let video_loss_ppm = if stats.video_sent > 0 {
        stats.video_sent.saturating_sub(stats.video_received) * 1_000_000
            / stats.video_sent
    } else {
        0
    };
    ConfigStats {
        config_id: config_id(cfg),
        latency_ms: cfg.latency_ms,
        drop_ppm: cfg.drop_ppm,
        input_rate_hz: cfg.input_rate_hz,
        video_rate_fps: cfg.video_rate_fps,
        duration_ms: cfg.duration.as_millis() as u64,
        input_sent: stats.input_sent,
        input_received: stats.input_received,
        input_loss_ppm,
        input_p50_us,
        input_p95_us,
        input_p99_us,
        video_sent: stats.video_sent,
        video_received: stats.video_received,
        video_loss_ppm,
    }
}

#[allow(dead_code)] // wired in Task 4
fn write_summary_csv(
    path: &std::path::Path,
    stats: &[ConfigStats],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut wtr = std::fs::File::create(path)?;
    writeln!(
        wtr,
        "config_id,latency_ms,drop_ppm,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us,video_sent,video_received,video_loss_ppm"
    )?;
    for s in stats {
        writeln!(
            wtr,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            s.config_id,
            s.latency_ms,
            s.drop_ppm,
            s.input_rate_hz,
            s.video_rate_fps,
            s.duration_ms,
            s.input_sent,
            s.input_received,
            s.input_loss_ppm,
            s.input_p50_us,
            s.input_p95_us,
            s.input_p99_us,
            s.video_sent,
            s.video_received,
            s.video_loss_ppm
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -15
cargo clippy -p prdt-latency-bench --bin prdt-net-profile-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 7 tests pass total (2 + 2 + 3); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/net-profile-bench.rs
git commit -m "net-profile-bench: add ConfigStats + aggregate + write_summary_csv"
```

---

## Task 4: Wire main loop + smoke + docs + tag

**Files:**
- Modify: `crates/latency-bench/src/bin/net-profile-bench.rs` (replace `bail!`, remove `#[allow(dead_code)]`)
- Create: `docs/net-profile-bench.md`

- [ ] **Step 1: Replace bail!() with the trial loop**

In `crates/latency-bench/src/bin/net-profile-bench.rs`, find:

```rust
    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("create out_dir {}", args.out_dir.display()))?;

    // Trial loop + summary CSV come in Tasks 2-4.
    anyhow::bail!("trial loop not yet implemented (Tasks 2-4)");
}
```

Replace with:

```rust
    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("create out_dir {}", args.out_dir.display()))?;

    let inter_delay = Duration::from_millis(args.inter_config_delay_ms);
    let mut all_stats: Vec<ConfigStats> = Vec::with_capacity(configs.len());
    for (i, cfg) in configs.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(inter_delay).await;
        }
        let id = config_id(cfg);
        info!(
            "[{:>3}/{}] running {} duration={:?}",
            i + 1,
            configs.len(),
            id,
            cfg.duration
        );
        let run = run_one_config(cfg).await;
        let stats = aggregate(cfg, &run);
        info!(
            "[{:>3}/{}] done    {} input={}/{} video={}/{} input_p95_us={}",
            i + 1,
            configs.len(),
            id,
            stats.input_received,
            stats.input_sent,
            stats.video_received,
            stats.video_sent,
            stats.input_p95_us
        );
        all_stats.push(stats);
    }

    let summary_path = args.out_dir.join("summary.csv");
    write_summary_csv(&summary_path, &all_stats)
        .with_context(|| format!("write {}", summary_path.display()))?;
    info!(
        path = %summary_path.display(),
        rows = all_stats.len(),
        "summary written"
    );
    Ok(())
}
```

- [ ] **Step 2: Remove all `#[allow(dead_code)]` attributes**

```bash
grep -n "#\[allow(dead_code)\]" crates/latency-bench/src/bin/net-profile-bench.rs
```

For every match, delete that line. There are 5 of these from Tasks 1-3 (`Cfg`, `RunStats`, `run_one_config`, `ConfigStats`, `aggregate`, `write_summary_csv` — actually 5 attribute lines covering 6 items where some share via the struct attribute). After deletion the grep should return zero matches.

- [ ] **Step 3: Build + tiny smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build --release -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -3
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile-smoke/ \
    --latencies-ms 0,10 --drops-ppm 0,10000 \
    --duration 500ms 2>&1 | tail -15
cat bench-results/net-profile-smoke/summary.csv
```

Expected:
- Clean build
- 5-line CSV (header + 4 rows: lat0/drop0, lat0/drop10000, lat10/drop0, lat10/drop10000)
- Row `lat0ms-drop0ppm`: `input_loss_ppm=0`, `input_p50_us` < 200
- Row `lat10ms-drop0ppm`: `input_p50_us` ≈ 10_000 (10ms)
- Row `lat0ms-drop10000ppm`: `input_loss_ppm` ≈ 10_000 (1%)
- Row `lat10ms-drop10000ppm`: latency + loss both visible

- [ ] **Step 4: Default 20-config sweep**

```bash
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile-default/ 2>&1 | tail -25
wc -l bench-results/net-profile-default/summary.csv
```

Expected: 21 lines (header + 20 configs). At default `--duration 5s` total wall time ≈ 105 s.

Per the spec's expected pattern:
- `lat0ms-drop0ppm`: p50_us < 100, loss = 0
- `lat200ms-drop50000ppm`: p50_us ≈ 200_000, input_loss_ppm ≈ 50_000

- [ ] **Step 5: Run unit tests + workspace clippy**

```bash
cargo test -p prdt-latency-bench --bin prdt-net-profile-bench 2>&1 | tail -10
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 7 tests pass; workspace clippy clean.

- [ ] **Step 6: rustfmt**

```bash
rustfmt crates/latency-bench/src/bin/net-profile-bench.rs
git diff --stat
```

If non-empty, include in upcoming commit.

- [ ] **Step 7: Create docs/net-profile-bench.md**

```markdown
# Network profile bench (Plan 4 B4)

The `prdt-net-profile-bench` bin sweeps `(latency_ms × drop_ppm)`
profiles using `LoopbackOptions` to inject one-way delay and
message-level drop on top of `InProcTransport`. It reports per-
profile InputEvent + Video send-to-recv lag and loss.

## Quick start

```bash
# Default 20-config sweep (5 latencies x 4 drops), ~105 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile/

# Custom subset (only LAN/metro latencies, no drop sweep).
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile-low/ \
    --latencies-ms 1,5,10 --drops-ppm 0

# Dry-run (list configs).
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--latencies-ms <list>` | `0,1,10,50,200` | One-way delay in milliseconds. |
| `--drops-ppm <list>` | `0,1000,10000,50000` | Per-message drop ppm. |
| `--input-rate-hz <N>` | `1000` | Fixed input rate (CLI override only). |
| `--video-rate-fps <N>` | `60` | Fixed video rate (CLI override only). |
| `--video-frame-bytes <N>` | `50000` | Synthetic frame size. |
| `--duration <humantime>` | `5s` | Per-config bench length. |
| `--inter-config-delay-ms <N>` | `250` | Spacing between configs. |
| `--dry-run` | off | List configs and exit. |

## Suggested profile presets

| Profile | `--latencies-ms` | `--drops-ppm` |
|---|---|---|
| localhost | `0` | `0` |
| LAN | `1` | `0` |
| metro | `10` | `1000` (0.1%) |
| WAN | `50` | `10000` (1%) |
| satellite | `600` | `50000` (5%) |
| lossy WiFi | `10` | `100000` (10%) |

To run a single profile, specify both axes with one value each.

## summary.csv schema

```
config_id,latency_ms,drop_ppm,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us,video_sent,video_received,video_loss_ppm
```

`config_id` format: `lat{latency_ms}ms-drop{drop_ppm}ppm`, e.g.
`lat0ms-drop0ppm`, `lat200ms-drop50000ppm`.

- `input_sent` / `input_received` / `video_sent` / `video_received`:
  message counts. Loss is the difference (silent drop in the
  transport layer).
- `*_loss_ppm`: `(sent - received) * 1_000_000 / sent`.
- `input_p50_us` / `_p95_us` / `_p99_us`: send-to-recv lag in
  microseconds for `Input` events only. Round-style percentile
  picking. Zero when no input lags were captured.

## Sample interpretation

```
lat50ms-drop10000ppm,50,10000,1000,60,5000,500,495,10000,50012,50034,50061,300,297,10000
```

means: 50 ms latency + 1% drop, 1000 Hz input + 60 fps video for
5 s. The 50 ms latency blocks the sender, so 5000 events would
take 250 s — only ~500 events fit in 5 s wall time. 1% of those
were dropped (`input_loss_ppm = 10000`). Input p50/p95/p99 sit
around 50 ms (the injected delay), with a few hundred µs of
overhead. Video at 60 fps × 5 s = 300 frames, 1% lost = 297
received.

## What this measures (and what it does NOT)

This bench measures how the **application sees** simulated network
profiles via `LoopbackOptions::latency` and `LoopbackOptions::drop_ppm`.

It does NOT measure:
- **Packet-level loss + FEC interaction**: `InProcTransport` ships
  whole `EncodedFrame` messages; FEC is not exercised. For FEC under
  loss see B6 (`prdt-fec-bench`).
- **Real UDP / network stack overhead**: no `CustomUdpTransport`,
  no socket layer.
- **Jitter / reorder / duplicate packets**: latency is a single
  fixed delay per message, not a distribution.
- **Bandwidth limit**: messages deliver in full byte size with
  no rate cap.
- **TURN-relay overhead**: external TURN server required.
- **Real glass-to-glass display latency** (Plan 4 M3 territory).

## Caveats

- **`latency` blocks the sender**: each `send_*` call sleeps for
  `latency` before completing, capping per-task throughput at
  `1 / latency`. A 1000 Hz input sender at 200 ms latency produces
  ~5 events per second. Counters reflect this; high-latency rows
  will have small `input_sent` and noisy percentiles.
- **`drop_ppm` is per-message, not per-packet**: dropping a video
  frame means the entire frame is missing from the receiver. There
  is no FEC opportunity in this bench.
- **`Bytes::from(vec![0u8; N])` allocations** per video frame: at
  60 fps × 50_000 bytes × 5 s × 20 configs = 600 MB of allocation
  churn over a full sweep. Cheap on modern hardware.
```

- [ ] **Step 8: Commit**

```bash
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
```

Expected: total ≥ 305 (was 298 + 7 new B4 tests).

```bash
git add crates/latency-bench/src/bin/net-profile-bench.rs \
        docs/net-profile-bench.md
git commit -m "net-profile-bench: wire main loop + docs"
```

- [ ] **Step 9: Tag**

```bash
git tag -a plan4-b4-net-profile-bench-complete -m "$(cat <<'EOF'
Plan 4 B4 network-profile bench complete

Adds prdt-net-profile-bench bin to latency-bench crate. Sweeps
(latency_ms x drop_ppm) profiles via LoopbackOptions on top of
InProcTransport. Reports per-profile InputEvent + Video send-to-recv
lag and message-level loss. Default 5 latencies x 4 drops = 20 configs.

- Cfg + expand_matrix (latency outer / drop inner)
- run_one_config: 3 spawned tasks (input + video sender + receiver)
  + cancel-on-deadline + drain, parameterized LoopbackOptions
- aggregate: input_loss_ppm + video_loss_ppm + input p50/p95/p99 us
- write_summary_csv: 15-column header per spec
- 7 unit tests (2 sync + 2 async + 3 aggregate/csv)
- 250 ms inter-config delay
- docs/net-profile-bench.md with usage + suggested profile presets
  + schema + sample interpretation + clear "what this does NOT
  measure" section (FEC interaction, real UDP, jitter, TURN, M3)

Out of scope: real network, packet-level loss + FEC interaction
(see B6), TURN relay (external server required), glass-to-glass
display latency (Plan 4 M3).
EOF
)"
git tag | grep plan4-b
```

Expected: lists `plan4-b1-bench-matrix-complete`, `plan4-b4-net-profile-bench-complete`, `plan4-b6-fec-bench-complete`, `plan4-b7-input-load-bench-complete`.

- [ ] **Step 10: Final summary report**

Report:
- Files added: `crates/latency-bench/src/bin/net-profile-bench.rs`, `docs/net-profile-bench.md`
- File modified: `crates/latency-bench/Cargo.toml` (+1 `[[bin]]` block)
- All `#[allow(dead_code)]` removed (verify count = 0)
- Smoke (4 configs): pass with expected latency / loss patterns
- Default 20-config sweep: 20 rows in summary.csv, ~105 s wall time
- Workspace test count + delta from 298
- Workspace clippy clean
- Tag listing
- Sample summary.csv top rows

## Self-review checklist

- [ ] `bail!()` removed
- [ ] Real trial loop with 250 ms inter-config delay
- [ ] All `#[allow(dead_code)]` removed (zero remaining)
- [ ] Build clean
- [ ] Smoke (4 configs) shows expected latency/loss patterns
- [ ] Default 20-config sweep produces 20 rows
- [ ] 7 unit tests pass
- [ ] Workspace clippy clean
- [ ] docs/net-profile-bench.md present
- [ ] tag `plan4-b4-net-profile-bench-complete` created

---

## Risks & Notes for Implementer

- **`InProcTransport::pair` lacks `Clone`**: per B7 verification, wrap each side in `Arc<>`. Same pattern here.
- **`LoopbackOptions` is `Copy`**: `let opts = LoopbackOptions { ... };` then pass it to `pair(opts)` directly.
- **`tokio_util::sync::CancellationToken`**: already in deps from B7 (`tokio-util` = "0.7"). No new deps.
- **`#[tokio::test(flavor = "multi_thread")]`**: works because workspace tokio enables `rt-multi-thread`.
- **High-latency tests are slow**: the `run_one_config_drop_ppm_loses_messages` test uses 0 ms latency, so it completes in 200 ms. Don't add high-latency unit tests; manual smoke (Step 3) covers that path.
- **`saturating_sub` for loss**: defensive; received should never exceed sent, but defensive math is cheap.
- **`use` statements placed mid-file**: same as B7 (after `expand_matrix`, before `run_one_config`). Acceptable interim layout; rustfmt won't move them.

---

## Self-Review

**Spec coverage:**
- §Architecture (B7-clone bin, separate file, no library API change) → Tasks 1+2 ✓
- §CLI 9 flags → Task 1 Args ✓
- §Trial flow (3 tasks: input, video, receiver; cancel + drain; LoopbackOptions populated per cfg) → Task 2 ✓
- §RunStats with video_sent/received → Task 2 ✓
- §Aggregation (input_loss_ppm + video_loss_ppm + percentiles) → Task 3 ✓
- §Output 15-column CSV → Task 3 write_summary_csv ✓
- §Tests (5 unit) → 7 (2 sync + 2 async + 3 aggregate/csv); exceeds spec, acceptable
- §Error handling (orphan ts handled by skipping pop on missing send, drop is silent so loss appears as receiver-side count gap) → Task 2 ✓
- §Progress logging → Task 4 main loop ✓
- §Risk: latency-induced send blocking → documented in `docs/net-profile-bench.md` ✓
- §Risk: high-latency configs sparse → documented ✓
- §Profile preset table → docs ✓
- §Exit criteria 6 items → Tasks 1-4 cover all ✓

**Placeholder scan:** No "TBD", "implement later". Task 1 main() ends with `bail!("trial loop not yet implemented (Tasks 2-4)")` which is intentional and explicitly replaced in Task 4.

**Type consistency:**
- `Cfg { latency_ms, drop_ppm, input_rate_hz, video_rate_fps, video_frame_bytes, duration }` — Task 1 def, Tasks 2/3/4 use ✓
- `RunStats { input_sent, input_received, input_lags, video_sent, video_received }` — Task 2 def, Task 3 use ✓
- `ConfigStats` 15 fields — Task 3 def, Task 4 use ✓
- `run_one_config(cfg: &Cfg) -> RunStats` — Task 2 def, Task 4 use ✓
- `aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats` — Task 3 def, Task 4 use ✓
- `write_summary_csv(path: &Path, stats: &[ConfigStats]) -> io::Result<()>` — Task 3 def, Task 4 use ✓
- `config_id(cfg: &Cfg) -> String` — Task 1 def, Tasks 2/3/4 use ✓
