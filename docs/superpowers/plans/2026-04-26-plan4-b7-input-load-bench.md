# Plan 4 B7 Input-under-load Bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `prdt-input-load-bench` bin that measures one-way `InputEvent` send→recv lag while a concurrent synthetic video stream consumes the same `InProcTransport`. Sweeps `(input_rate_hz × video_rate_fps)` and writes a per-config CSV.

**Architecture:** Single-process, single-bin in `crates/latency-bench/src/bin/input-load-bench.rs`. Reuses `prdt_transport::InProcTransport` (FIFO, drop_ppm=0), `prdt_protocol::InputEvent::MouseMove` for the input message, synthetic `EncodedFrame` for the concurrent video load. `tokio::sync::mpsc::unbounded_channel<u64>` carries `sent_us` from sender to receiver since `InputEvent` has no timestamp field.

**Tech Stack:** Rust 2021, existing `clap` derive, `tokio` (rt-multi-thread + sync + time + macros), `tracing`, `humantime`, `tokio-util` (CancellationToken). No new workspace deps.

**Spec:** `docs/superpowers/specs/2026-04-26-plan4-b7-input-load-design.md`

---

## File Structure

**Created files:**

```
crates/latency-bench/src/bin/
  input-load-bench.rs          ← new prdt-input-load-bench bin
                                 CLI + matrix expand + per-config
                                 sender/receiver tasks + aggregate
                                 + summary CSV + 5 unit tests
docs/
  input-load-bench.md          ← usage + schema + sample interpretation
```

**Modified files:**

```
crates/latency-bench/Cargo.toml + [[bin]] entry for prdt-input-load-bench
                                + tokio-util dep already present? verify
```

---

## Verified API (from current code)

```rust
// prdt_transport
pub use loopback::{InProcTransport, LoopbackOptions};
pub use transport_trait::{ReceivedMessage, Transport};

#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackOptions {
    pub drop_ppm: u32,
    pub latency: Option<Duration>,
}

impl InProcTransport {
    pub fn pair(opts: LoopbackOptions) -> (Self, Self);
}

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError>;
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError>;
    async fn send_audio(&self, pkt: AudioPacket) -> Result<(), TransportError>;
    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<ReceivedMessage, TransportError>;
}

pub enum ReceivedMessage {
    Video(EncodedFrame),
    Audio(AudioPacket),
    Input(InputEvent),
    Control(ControlMessage),
}

// prdt_protocol
pub enum InputEvent {
    MouseMove { x: i32, y: i32, absolute: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    Key { scancode: u32, pressed: bool },
}
pub fn now_monotonic_us() -> u64;
pub struct EncodedFrame {
    pub seq: u64,
    pub timestamp_host_us: u64,
    pub is_keyframe: bool,
    pub nal_units: bytes::Bytes,
    pub width: u32,
    pub height: u32,
    pub codec: prdt_protocol::frame::Codec, // Codec::H265 variant exists
}
```

`Transport` is implemented for `InProcTransport`. To send from a spawned task we need `Arc<InProcTransport>` (or just clone — `InProcTransport` itself is `Clone`? — verify; if not, wrap in `Arc`).

**Verification check during Task 2** with: `grep -n "impl Clone for InProcTransport\|#\[derive.*Clone.*\] *pub struct InProcTransport" crates/transport/src/loopback.rs`. If `Clone` is implemented, use clone; otherwise wrap each side in `Arc<InProcTransport>` before spawning.

---

## Task 1: Bin scaffold + CLI + dry-run

**Files:**
- Modify: `crates/latency-bench/Cargo.toml`
- Create: `crates/latency-bench/src/bin/input-load-bench.rs`

- [ ] **Step 1: Update Cargo.toml**

In `crates/latency-bench/Cargo.toml`, after the existing `[[bin]] prdt-fec-bench` block, append:

```toml

[[bin]]
name = "prdt-input-load-bench"
path = "src/bin/input-load-bench.rs"
```

Verify these are already in `[dependencies]` (B1 + B6 added them):
- `prdt-transport`, `prdt-protocol`, `tokio`, `clap`, `tracing`, `tracing-subscriber`, `bytes`, `anyhow`, `humantime`

Add `tokio-util = { version = "0.7", features = ["rt"] }` if NOT present. Run:
```bash
grep -n "tokio-util" crates/latency-bench/Cargo.toml
```
If empty, add it under `[dependencies]`.

`tempfile` is already a dev-dep.

- [ ] **Step 2: Create the scaffold**

Create `crates/latency-bench/src/bin/input-load-bench.rs`:

```rust
//! Plan 4 B7 Input-under-load bench. Measures one-way send-to-recv
//! lag for InputEvent messages while a concurrent synthetic video
//! stream shares the same InProcTransport. Sweeps (input_rate_hz x
//! video_rate_fps) and writes a per-config CSV.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "prdt-input-load-bench",
    about = "Plan 4 B7 input-under-load: send-to-recv InputEvent lag with concurrent synthetic video"
)]
struct Args {
    /// Output dir. Will contain `summary.csv`. Created if missing;
    /// existing files are overwritten.
    #[arg(long)]
    out_dir: PathBuf,

    /// Comma-separated input rates in Hz.
    #[arg(long, value_delimiter = ',', default_values_t = vec![100u32, 500u32, 1000u32, 5000u32])]
    input_rates: Vec<u32>,

    /// Comma-separated concurrent video rates in fps. 0 = no video.
    #[arg(long, value_delimiter = ',', default_values_t = vec![0u32, 60u32, 120u32])]
    video_rates: Vec<u32>,

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
    input_rate_hz: u32,
    video_rate_fps: u32,
    video_frame_bytes: usize,
    duration: Duration,
}

fn config_id(cfg: &Cfg) -> String {
    format!("in{}hz-vid{}fps", cfg.input_rate_hz, cfg.video_rate_fps)
}

fn expand_matrix(args: &Args) -> Vec<Cfg> {
    let mut out = Vec::with_capacity(args.input_rates.len() * args.video_rates.len());
    for &input_rate_hz in &args.input_rates {
        for &video_rate_fps in &args.video_rates {
            out.push(Cfg {
                input_rate_hz,
                video_rate_fps,
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
            input_rate_hz: 100,
            video_rate_fps: 0,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        assert_eq!(config_id(&c), "in100hz-vid0fps");
        let c = Cfg {
            input_rate_hz: 5000,
            video_rate_fps: 120,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        assert_eq!(config_id(&c), "in5000hz-vid120fps");
    }

    #[test]
    fn expand_matrix_cartesian() {
        let args = Args {
            out_dir: PathBuf::from("/tmp/fake"),
            input_rates: vec![100, 1000],
            video_rates: vec![0, 60],
            video_frame_bytes: 50_000,
            duration: humantime::Duration::from(Duration::from_secs(5)),
            inter_config_delay_ms: 250,
            dry_run: true,
        };
        let cfgs = expand_matrix(&args);
        assert_eq!(cfgs.len(), 4); // 2 * 2
        // Order: input_rate outer, video_rate inner
        assert_eq!((cfgs[0].input_rate_hz, cfgs[0].video_rate_fps), (100, 0));
        assert_eq!((cfgs[1].input_rate_hz, cfgs[1].video_rate_fps), (100, 60));
        assert_eq!((cfgs[2].input_rate_hz, cfgs[2].video_rate_fps), (1000, 0));
        assert_eq!((cfgs[3].input_rate_hz, cfgs[3].video_rate_fps), (1000, 60));
    }
}
```

- [ ] **Step 3: Build + dry-run smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -3
cargo run -p prdt-latency-bench --bin prdt-input-load-bench -- --out-dir /tmp/dry --dry-run 2>/dev/null | wc -l
```

Expected: clean build; dry-run prints exactly **12** stdout lines (4 input × 3 video).

- [ ] **Step 4: Tests**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -10
cargo clippy -p prdt-latency-bench --bin prdt-input-load-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 2 tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/Cargo.toml \
        crates/latency-bench/src/bin/input-load-bench.rs
git commit -m "input-load-bench: scaffold bin with CLI + dry-run + matrix expansion"
```

---

## Task 2: Per-config trial runner (sender/receiver tasks)

**Files:**
- Modify: `crates/latency-bench/src/bin/input-load-bench.rs`

This task adds the heart of the bench: spawning input-sender, optional video-sender, and receiver tasks; collecting per-event lag; returning `RunStats`.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/latency-bench/src/bin/input-load-bench.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn run_one_config_no_video_sees_lag() {
        // 100 Hz for 200 ms = ~20 events.
        let cfg = Cfg {
            input_rate_hz: 100,
            video_rate_fps: 0,
            video_frame_bytes: 0,
            duration: Duration::from_millis(200),
        };
        let stats = run_one_config(&cfg).await;
        assert!(stats.input_sent >= 10, "expected ~20 events, got {}", stats.input_sent);
        assert!(stats.input_sent <= 30);
        assert_eq!(stats.input_received, stats.input_sent, "no drops at default LoopbackOptions");
        assert!(!stats.lags.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_one_config_with_video_still_delivers_input() {
        // 100 Hz input + 60 fps video for 200 ms.
        let cfg = Cfg {
            input_rate_hz: 100,
            video_rate_fps: 60,
            video_frame_bytes: 1024, // small to keep the test fast
            duration: Duration::from_millis(200),
        };
        let stats = run_one_config(&cfg).await;
        assert!(stats.input_sent >= 10);
        assert_eq!(stats.input_received, stats.input_sent, "no drops at default LoopbackOptions");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -10
```

Expected: compile errors — `cannot find function run_one_config`, `cannot find type RunStats`.

- [ ] **Step 3: Implement run_one_config + RunStats**

In `crates/latency-bench/src/bin/input-load-bench.rs`, between `expand_matrix` and `main`, add:

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
struct RunStats {
    input_sent: u64,
    input_received: u64,
    lags: Vec<u64>,
}

async fn run_one_config(cfg: &Cfg) -> RunStats {
    let (host_side, viewer_side) =
        InProcTransport::pair(LoopbackOptions::default());
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
                if cancel.is_cancelled() {
                    break;
                }
                tokio::select! {
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

    // ---- Video sender (optional) ----
    let send_video = if cfg.video_rate_fps > 0 {
        let viewer_side = Arc::clone(&viewer_side);
        let cancel = cancel.clone();
        let interval = Duration::from_secs_f64(1.0 / cfg.video_rate_fps as f64);
        let frame_bytes = cfg.video_frame_bytes;
        let handle = tokio::spawn(async move {
            let mut seq: u64 = 0;
            let mut next = Instant::now();
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                tokio::select! {
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
        });
        Some(handle)
    } else {
        None
    };

    // ---- Receiver ----
    let recv_task = {
        let host_side = Arc::clone(&host_side);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut received: u64 = 0;
            let mut lags: Vec<u64> = Vec::new();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    msg = host_side.recv() => {
                        match msg {
                            Ok(ReceivedMessage::Input(_)) => {
                                if let Ok(sent_us) = sent_ts_rx.try_recv() {
                                    let lag = now_monotonic_us().saturating_sub(sent_us);
                                    lags.push(lag);
                                }
                                received += 1;
                            }
                            Ok(_) => {} // discard video / audio / control
                            Err(_) => break,
                        }
                    }
                }
            }
            (received, lags)
        })
    };

    // ---- Wait for the configured duration, then cancel ----
    tokio::time::sleep(cfg.duration).await;
    cancel.cancel();

    // ---- Drain remaining in-flight messages briefly ----
    let drain_deadline = Instant::now() + Duration::from_millis(50);
    while Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(5), host_side.recv()).await {
            Ok(Ok(ReceivedMessage::Input(_))) => {
                // We already cancelled; recv_task may already have exited.
                // Best-effort: don't double-count here. Skip.
            }
            _ => break,
        }
    }

    let input_sent = send_input.await.unwrap_or(0);
    if let Some(h) = send_video {
        let _ = h.await;
    }
    let (input_received, lags) = recv_task.await.unwrap_or_default();

    RunStats {
        input_sent,
        input_received,
        lags,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -10
```

Expected: 4 tests pass total (2 prior + 2 new). The new tests run real tokio loops at 100 Hz for 200 ms, completing in well under 1 s each.

```bash
cargo clippy -p prdt-latency-bench --bin prdt-input-load-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/input-load-bench.rs
git commit -m "input-load-bench: implement run_one_config (input + video senders + receiver)"
```

---

## Task 3: Aggregate + CSV writer

**Files:**
- Modify: `crates/latency-bench/src/bin/input-load-bench.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn aggregate_collects_outcomes() {
        let cfg = Cfg {
            input_rate_hz: 100,
            video_rate_fps: 0,
            video_frame_bytes: 0,
            duration: Duration::from_millis(500),
        };
        let stats = RunStats {
            input_sent: 50,
            input_received: 49,
            lags: (1..=49u64).collect(),
        };
        let s = aggregate(&cfg, &stats);
        assert_eq!(s.input_sent, 50);
        assert_eq!(s.input_received, 49);
        // 1 lost out of 50 = 20000 ppm
        assert_eq!(s.input_loss_ppm, 20_000);
        // p50 of 1..=49 (49 elements) using round-style picking:
        // idx = round((49-1) * 0.5) = round(24.0) = 24 -> v[24] = 25
        assert_eq!(s.input_p50_us, 25);
        // p95: idx = round(48 * 0.95) = round(45.6) = 46 -> v[46] = 47
        assert_eq!(s.input_p95_us, 47);
        // p99: idx = round(48 * 0.99) = round(47.52) = 48 -> v[48] = 49
        assert_eq!(s.input_p99_us, 49);
    }

    #[test]
    fn aggregate_empty_lags_emits_zero_percentiles_but_keeps_counts() {
        let cfg = Cfg {
            input_rate_hz: 100,
            video_rate_fps: 0,
            video_frame_bytes: 0,
            duration: Duration::from_millis(500),
        };
        let stats = RunStats {
            input_sent: 0,
            input_received: 0,
            lags: vec![],
        };
        let s = aggregate(&cfg, &stats);
        assert_eq!(s.input_sent, 0);
        assert_eq!(s.input_received, 0);
        assert_eq!(s.input_loss_ppm, 0);
        assert_eq!(s.input_p50_us, 0);
        assert_eq!(s.input_p95_us, 0);
        assert_eq!(s.input_p99_us, 0);
    }

    #[test]
    fn summary_csv_writer_emits_header_and_one_row() {
        let cfg = Cfg {
            input_rate_hz: 1000,
            video_rate_fps: 60,
            video_frame_bytes: 50_000,
            duration: Duration::from_secs(5),
        };
        let s = ConfigStats {
            config_id: config_id(&cfg),
            input_rate_hz: cfg.input_rate_hz,
            video_rate_fps: cfg.video_rate_fps,
            duration_ms: cfg.duration.as_millis() as u64,
            input_sent: 5000,
            input_received: 4998,
            input_loss_ppm: 400,
            input_p50_us: 12,
            input_p95_us: 28,
            input_p99_us: 45,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.csv");
        write_summary_csv(&path, std::slice::from_ref(&s)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row");
        assert_eq!(
            lines[0],
            "config_id,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us"
        );
        assert_eq!(
            lines[1],
            "in1000hz-vid60fps,1000,60,5000,5000,4998,400,12,28,45"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -15
```

Expected: compile errors — `cannot find type ConfigStats`, `cannot find function aggregate`, `cannot find function write_summary_csv`.

- [ ] **Step 3: Implement ConfigStats + aggregate + write_summary_csv**

In `crates/latency-bench/src/bin/input-load-bench.rs`, between `run_one_config` and `main`, add:

```rust
struct ConfigStats {
    config_id: String,
    input_rate_hz: u32,
    video_rate_fps: u32,
    duration_ms: u64,
    input_sent: u64,
    input_received: u64,
    input_loss_ppm: u64,
    input_p50_us: u64,
    input_p95_us: u64,
    input_p99_us: u64,
}

fn aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats {
    let (input_p50_us, input_p95_us, input_p99_us) = if stats.lags.is_empty() {
        (0, 0, 0)
    } else {
        let mut lags = stats.lags.clone();
        let (p50, _, p95, p99, _) = prdt_latency_bench::percentiles(&mut lags);
        (p50, p95, p99)
    };
    let input_loss_ppm = if stats.input_sent > 0 {
        (stats.input_sent.saturating_sub(stats.input_received))
            * 1_000_000
            / stats.input_sent
    } else {
        0
    };
    ConfigStats {
        config_id: config_id(cfg),
        input_rate_hz: cfg.input_rate_hz,
        video_rate_fps: cfg.video_rate_fps,
        duration_ms: cfg.duration.as_millis() as u64,
        input_sent: stats.input_sent,
        input_received: stats.input_received,
        input_loss_ppm,
        input_p50_us,
        input_p95_us,
        input_p99_us,
    }
}

fn write_summary_csv(
    path: &std::path::Path,
    stats: &[ConfigStats],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut wtr = std::fs::File::create(path)?;
    writeln!(
        wtr,
        "config_id,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us"
    )?;
    for s in stats {
        writeln!(
            wtr,
            "{},{},{},{},{},{},{},{},{},{}",
            s.config_id,
            s.input_rate_hz,
            s.video_rate_fps,
            s.duration_ms,
            s.input_sent,
            s.input_received,
            s.input_loss_ppm,
            s.input_p50_us,
            s.input_p95_us,
            s.input_p99_us
        )?;
    }
    Ok(())
}
```

Add `#[allow(dead_code)]` on `ConfigStats` and the two functions if clippy `-D warnings` complains (they're called from main in Task 4, plus tests use them).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -15
cargo clippy -p prdt-latency-bench --bin prdt-input-load-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 7 tests pass total (2 + 2 + 3); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/input-load-bench.rs
git commit -m "input-load-bench: add ConfigStats + aggregate + write_summary_csv"
```

---

## Task 4: Wire main loop + smoke + docs + tag

**Files:**
- Modify: `crates/latency-bench/src/bin/input-load-bench.rs` (replace bail!)
- Create: `docs/input-load-bench.md`

- [ ] **Step 1: Replace bail!() with the real trial loop**

In `crates/latency-bench/src/bin/input-load-bench.rs`, find:

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
            "[{:>3}/{}] done    {} sent={} received={} input_p95_us={}",
            i + 1,
            configs.len(),
            id,
            stats.input_sent,
            stats.input_received,
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
grep -n "#\[allow(dead_code)\]" crates/latency-bench/src/bin/input-load-bench.rs
```

For every match, delete the line. After this command produces zero hits the file is clean.

- [ ] **Step 3: Build + tiny smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build --release -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | tail -3
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load-smoke/ \
    --input-rates 100,1000 --video-rates 0,60 \
    --duration 500ms 2>&1 | tail -15
cat bench-results/input-load-smoke/summary.csv
```

Expected:
- Clean build
- 5-line CSV (header + 4 rows: 100/0, 100/60, 1000/0, 1000/60)
- All `input_loss_ppm == 0` (or near zero)
- `input_p50_us` < 200 µs across all 4 configs (cheap on a modern x86)

- [ ] **Step 4: Default 12-config sweep**

```bash
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load-default/ 2>&1 | tail -25
wc -l bench-results/input-load-default/summary.csv
```

Expected: 13 lines (header + 12 configs). All loss zero or near-zero. p95 may rise into the ms range at `input_rates=5000 video_rates=120` — that's the signal the bench is designed to expose.

- [ ] **Step 5: Run unit tests + workspace clippy**

```bash
cargo test -p prdt-latency-bench --bin prdt-input-load-bench 2>&1 | grep "test result"
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 7 tests pass; workspace clippy clean.

- [ ] **Step 6: Format**

```bash
rustfmt crates/latency-bench/src/bin/input-load-bench.rs
git diff --stat
```

If non-empty, include in the upcoming commit.

- [ ] **Step 7: Create docs/input-load-bench.md**

```markdown
# Input-under-load bench (Plan 4 B7)

The `prdt-input-load-bench` bin measures one-way send-to-recv lag for
`InputEvent` messages while a concurrent synthetic video stream
shares the same `InProcTransport`. Used to spot regressions where
input queueing under video load suddenly explodes (e.g. an unbounded
channel filling up).

## Quick start

```bash
# Default 12-config sweep (4 input_rates x 3 video_rates), ~63 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load/

# Custom subset (only 1000 Hz at various video rates).
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load-1k/ \
    --input-rates 1000 --video-rates 0,60,120,240

# Dry-run.
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--input-rates <list>` | `100,500,1000,5000` | Hz, comma-separated. |
| `--video-rates <list>` | `0,60,120` | fps, 0 = no video. |
| `--video-frame-bytes <N>` | `50000` | Synthetic frame size. |
| `--duration <humantime>` | `5s` | Per-config bench length. |
| `--inter-config-delay-ms <N>` | `250` | Spacing between configs. |
| `--dry-run` | off | List configs and exit. |

## summary.csv schema

```
config_id,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us
```

`config_id` format: `in{rate}hz-vid{fps}fps`, e.g. `in100hz-vid0fps`,
`in5000hz-vid120fps`.

- `input_sent`, `input_received`: counts of `InputEvent::MouseMove`
  messages. Loss should normally be 0 (InProcTransport does not
  drop with `LoopbackOptions::default()`).
- `input_loss_ppm`: `(sent - received) * 1_000_000 / max(1, sent)`.
- `input_p50_us` / `_p95_us` / `_p99_us`: send-to-recv lag in
  microseconds, computed via `prdt_latency_bench::percentiles`
  (round-style picking). Zero when `input_received == 0`.

## What this measures (and what it does NOT)

This bench measures **only** the protocol/transport portion of input
event delivery: `Transport::send_input(...)` returning to the
matching `recv() -> ReceivedMessage::Input(_)` arriving on the host
side. Both sides share `prdt_protocol::now_monotonic_us`, so the
subtraction is exact.

It does NOT measure:
- Capture overhead (RawInput callback to `send_input`)
- Real network RTT (this is single-process)
- Host-side `SendInputInjector::inject` (the bench skips injection)
- Display refresh / driver latency (Plan 4 M3 territory)

To approximate two-way RTT, double the `input_p50_us` etc.

## Sample interpretation

```
in1000hz-vid60fps,1000,60,5000,5000,5000,0,12,28,45
```

means: 1000 Hz input + 60 fps video for 5 s, all 5000 InputEvents
delivered (loss 0), median lag 12 µs, p95 28 µs, p99 45 µs.

If a future change makes the receive task slower under video load,
the p95 / p99 values will balloon — the loss column is the
secondary signal (events queueing past their deadline, eventually
exceeding mpsc capacity).

## Limitations

- **Single-process only**: real network adds queueing delay,
  reorder, and loss not modelled here.
- **Synthetic video frame**: zero-filled bytes, no NVENC, no FEC.
  Transport layer just shuttles the EncodedFrame as-is.
- **Uniform input rate**: real users emit bursty inputs; this bench
  is a steady-state measurement.
- **MouseMove only**: other InputEvent variants are similar in size;
  the choice is for stability, not generality.
```

- [ ] **Step 8: Commit docs + final tests**

```bash
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
```

Expected: total ≥ 298 (was 291 + 7 new B7 tests).

```bash
git add crates/latency-bench/src/bin/input-load-bench.rs \
        docs/input-load-bench.md
git commit -m "input-load-bench: wire main loop + docs"
```

If `rustfmt` produced changes earlier (Step 6), bundle them with the same commit (the `git add` above already covers it).

- [ ] **Step 9: Tag**

```bash
git tag -a plan4-b7-input-load-bench-complete -m "$(cat <<'EOF'
Plan 4 B7 input-under-load bench complete

Adds prdt-input-load-bench bin to latency-bench crate. Single-process
sender/receiver pair on InProcTransport: viewer sends InputEvents at
configurable rate while an optional synthetic video stream shares the
same transport. Receiver computes one-way send-to-recv lag using a
side mpsc<u64> for sent_us (InputEvent has no timestamp field).

- Cfg + expand_matrix (input_rate outer / video_rate inner)
- run_one_config: 3 spawned tasks (input sender, optional video
  sender, receiver) + cancel-on-deadline + drain
- aggregate: input_loss_ppm + input_p50/p95/p99 us
- write_summary_csv: 10-column header per spec
- 7 unit tests (2 sync + 2 async runtime + 3 aggregate/csv)
- 250ms inter-config delay (matches B1)
- docs/input-load-bench.md with usage + schema + sample interpretation

Out of scope: real network, host-side echo (would change protocol),
real input injection, glass-to-glass display latency (Plan 4 M3).
EOF
)"
git tag | grep plan4-b
```

Expected: lists plan4-b1, plan4-b6, plan4-b7.

- [ ] **Step 10: Final summary report**

Report:
- Files added: `crates/latency-bench/src/bin/input-load-bench.rs`, `docs/input-load-bench.md`
- File modified: `crates/latency-bench/Cargo.toml` (+1 `[[bin]]` block, optionally +tokio-util dep)
- Workspace test count + delta from 291
- Clippy result
- Tag listing
- Sample summary.csv top rows from default sweep
- Manual smoke status

---

## Risks & Notes for Implementer

- **InProcTransport `Clone` vs `Arc`**: verify with `grep "impl Clone\|derive.*Clone.*InProcTransport" crates/transport/src/loopback.rs`. The plan uses `Arc<InProcTransport>` which always works. If `InProcTransport` derives `Clone` directly you can simplify by cloning instead, but `Arc` is universally safe.
- **`tokio::sync::mpsc::unbounded_channel`** for `sent_ts` is unbounded by design — at 5000 Hz × 5 s × 8 bytes = 200 KB, allocation cost is negligible.
- **`try_recv` on the receiver side**: when an `Input(_)` arrives but the corresponding `sent_us` hasn't been pushed yet (very unlikely race because the sender pushes BEFORE awaiting `send_input`), `try_recv` returns `Err(TryRecvError::Empty)`. Plan code drops the lag in that case. If this becomes common, switch to a small bounded channel that the sender writes to BEFORE the send so backpressure aligns.
- **`tokio::test(flavor = "multi_thread")` requires the `rt-multi-thread` feature**: workspace tokio already enables this. Verify with `grep "rt-multi-thread" Cargo.toml`. If absent, add `features = [..., "rt-multi-thread"]`.
- **Test duration 200 ms × 4 tests = ~1 s**: under the 5-minute test timeout. Cargo test runs them in parallel by default but each takes ~200 ms wall.
- **`Args::default` not derived**: the `expand_matrix_cartesian` test constructs `Args { ... }` literal, which is the cleanest way to avoid touching `Args`'s clap derive surface.
- **`PathBuf` import** in `tests` block: covered because the parent module already imports `PathBuf`.
- **`humantime::Duration::from(Duration::from_secs(5))`**: this conversion exists; `humantime::Duration` is a newtype wrapper.
- **Drain after cancel**: the brief 50 ms drain loop after cancel is best-effort; the receiver task may have already exited. The plan's drain is a noop in most cases — kept for symmetry with other benches.
- **`run_one_config` must return AFTER all spawned tasks finish**: the `.await` on `send_input`, `send_video`, and `recv_task` joins them. If any panics, the join unwraps to `Err(JoinError)` which the plan handles via `unwrap_or(0)` / `unwrap_or_default()`.

---

## Self-Review

**Spec coverage:**
- §Architecture (separate bin, single-process, InProcTransport, mpsc<u64>) → Tasks 1+2 ✓
- §CLI 7 flags → Task 1 Args struct ✓
- §Trial flow (3 tasks: input sender, video sender, receiver; cancel-on-deadline; drain) → Task 2 run_one_config ✓
- §Aggregation (counts + percentiles + loss_ppm + zero on empty) → Task 3 aggregate ✓
- §Output 10-column CSV → Task 3 write_summary_csv ✓
- §Tests (5 unit) → Tasks 1+2+3 totals 7 (2 + 2 + 3) ✓ — exceeds spec, acceptable
- §Error handling (sender break on transport close, recv error break, empty sent_ts skip) → Task 2 covers all ✓
- §Progress logging → Task 4 main loop info!() ✓
- §Risk: inter-config delay → Task 4 main loop sleep ✓
- §Reproducibility: clock-based, no RNG → no seed flag, but spec's `--seed` removal matches plan
- §Exit criteria 6 items → Tasks 1-4 cover all ✓

**Placeholder scan:** No "TBD", "implement later". The Task 1 main() ends with `bail!("trial loop not yet implemented (Tasks 2-4)")` which is intentional — Task 4 Step 1 explicitly replaces it.

**Type consistency:**
- `Cfg { input_rate_hz, video_rate_fps, video_frame_bytes, duration }` — Task 1 def, Tasks 2/3/4 use ✓
- `RunStats { input_sent, input_received, lags }` — Task 2 def, Task 3 uses ✓
- `ConfigStats { config_id, input_rate_hz, video_rate_fps, duration_ms, input_sent, input_received, input_loss_ppm, input_p50_us, input_p95_us, input_p99_us }` — Task 3 def, Task 4 uses ✓
- `run_one_config(cfg: &Cfg) -> RunStats` — Task 2 def, Task 4 uses ✓
- `aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats` — Task 3 def, Task 4 uses ✓
- `write_summary_csv(path: &Path, stats: &[ConfigStats]) -> io::Result<()>` — Task 3 def, Task 4 uses ✓
- `config_id(cfg: &Cfg) -> String` — Task 1 def, Tasks 2/4 use ✓

Spec waiver: B7's `--seed` flag mentioned in spec §Risks for "symmetry" was dropped in this plan because the bench has no RNG (clock-driven only). The plan is internally consistent on this.
