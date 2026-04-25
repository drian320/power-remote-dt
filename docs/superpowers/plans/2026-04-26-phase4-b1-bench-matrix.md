# Plan 4 B1 Bench Matrix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `prdt-bench-matrix` bin to `latency-bench` that sweeps 60 configs (3 resolutions × 5 bitrates × 2 decoders × 2 fps) and writes `summary.csv` + per-frame raw CSVs.

**Architecture:** Convert `latency-bench` from binary-only to lib + 2 bins. Promote `full_pipeline` to `pub mod`, split `run` into `run_for_matrix(cfg) -> RunStats` (returns stats, no I/O) + thin wrapper that writes CSV. New `bench-matrix` bin owns matrix expansion, sequential execution, per-frame + summary CSV writers, and progress logging. CLI accepts comma-separated axes with sane defaults.

**Tech Stack:** Rust 2021, existing `clap` derive, `tokio`, `tracing`, NVENC + MF/NVDEC via `prdt-media-win`, `humantime`. No new workspace deps.

**Spec:** `docs/superpowers/specs/2026-04-25-phase4-b1-bench-matrix-design.md`

---

## File Structure

**Modified files:**

```
crates/latency-bench/Cargo.toml         + [lib] + [[bin]] for prdt-bench-matrix
crates/latency-bench/src/main.rs        - move percentiles() to lib.rs, import from there
crates/latency-bench/src/full_pipeline.rs  refactor run() to call run_for_matrix();
                                           expose RunStats + run_for_matrix as pub
```

**Created files:**

```
crates/latency-bench/src/lib.rs         pub mod full_pipeline + percentiles + new types
                                        (MatrixAxes, ConfigStats, expand_matrix, aggregate,
                                        config_id, write_per_frame_csv, write_summary_csv)
crates/latency-bench/src/bin/bench-matrix.rs  new prdt-bench-matrix binary
docs/bench-matrix.md                    usage guide + sample CSV interpretation
```

**Public API surface added on `prdt_latency_bench::*`:**

- `pub fn percentiles(lags_us: &mut [u64]) -> (u64, u64, u64, u64, u64)` (moved from main.rs)
- `pub use full_pipeline::{ConsumerBackend, FullPipelineConfig, StageTimes}`
- `pub struct RunStats { sent, received, frames }`
- `pub async fn run_for_matrix(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats>` (in full_pipeline.rs, re-exported)
- `pub struct MatrixAxes { resolutions, bitrates_mbps, decoders, fps, duration }`
- `pub struct ConfigStats { ... 18 fields per spec ... }`
- `pub fn config_id(resolution, fps, bitrate_mbps, decoder) -> String`
- `pub fn expand_matrix(axes: &MatrixAxes) -> Vec<FullPipelineConfig>`
- `pub fn aggregate(cfg: &FullPipelineConfig, run: &RunStats) -> ConfigStats`
- `pub fn write_per_frame_csv(path: &Path, frames: &[StageTimes]) -> std::io::Result<()>`
- `pub fn write_summary_csv(path: &Path, stats: &[ConfigStats]) -> std::io::Result<()>`

---

## Task 1: Convert `latency-bench` into lib + bin layout

**Files:**
- Modify: `crates/latency-bench/Cargo.toml`
- Create: `crates/latency-bench/src/lib.rs`
- Modify: `crates/latency-bench/src/main.rs` (move `percentiles` out)

- [ ] **Step 1: Update Cargo.toml**

Replace `crates/latency-bench/Cargo.toml` contents with:

```toml
[package]
name = "prdt-latency-bench"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lib]
name = "prdt_latency_bench"
path = "src/lib.rs"

[[bin]]
name = "prdt-latency-bench"
path = "src/main.rs"

[[bin]]
name = "prdt-bench-matrix"
path = "src/bin/bench-matrix.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
bytes.workspace = true
anyhow = "1"
humantime = "2"

[target.'cfg(windows)'.dependencies]
prdt-media-win = { path = "../media-win" }

[dev-dependencies]
tempfile = "3"
```

(`tempfile` is needed by Task 4's CSV writer tests. The `[[bin]]` for `bench-matrix` points at a file that doesn't exist yet — Task 5 creates it. Cargo only complains when building that bin, so Task 1's `cargo build -p prdt-latency-bench --lib --bin prdt-latency-bench` works fine.)

- [ ] **Step 2: Create src/lib.rs**

Create `crates/latency-bench/src/lib.rs`:

```rust
//! Plan 4 latency bench library — shared between the single-config bin
//! (`prdt-latency-bench`) and the matrix bin (`prdt-bench-matrix`).

#[cfg(windows)]
pub mod full_pipeline;

/// Compute (p50, p90, p95, p99, p100) by sorting in place. Sorts the input.
pub fn percentiles(lags_us: &mut [u64]) -> (u64, u64, u64, u64, u64) {
    lags_us.sort_unstable();
    let pick = |p: f64| -> u64 {
        let idx = ((lags_us.len() as f64 - 1.0) * p).round() as usize;
        lags_us[idx]
    };
    (
        pick(0.50),
        pick(0.90),
        pick(0.95),
        pick(0.99),
        *lags_us.last().unwrap_or(&0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_monotonic() {
        let mut v: Vec<u64> = (1..=100).collect();
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert!(p50 <= p90);
        assert!(p90 <= p95);
        assert!(p95 <= p99);
        assert!(p99 <= p100);
        assert_eq!(p100, 100);
    }

    #[test]
    fn percentiles_single_sample() {
        let mut v = vec![42u64];
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert_eq!((p50, p90, p95, p99, p100), (42, 42, 42, 42, 42));
    }
}
```

- [ ] **Step 3: Remove percentiles + percentile tests from main.rs**

In `crates/latency-bench/src/main.rs`:

- Delete the `pub(crate) fn percentiles(...)` function (currently lines 89-102).
- Delete the two existing tests `percentiles_monotonic` and `percentiles_single_sample` from the `#[cfg(test)] mod tests` block (currently lines 259-275). Keep `parse_res_*` tests.
- Add `use prdt_latency_bench::percentiles;` near the existing `use bytes::Bytes;` import.

After this main.rs's `mod tests` should retain only `parse_res_accepts_wxh` and `parse_res_rejects_garbage`.

The `mod full_pipeline;` declaration on line 23 of main.rs becomes redundant because the lib already declares it. Replace:

```rust
#[cfg(windows)]
mod full_pipeline;
```

with:

```rust
#[cfg(windows)]
use prdt_latency_bench::full_pipeline;
```

- [ ] **Step 4: Build + test**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-latency-bench --lib --bin prdt-latency-bench
cargo test -p prdt-latency-bench --lib --bin prdt-latency-bench
```

Expected: clean build (note: `--bin prdt-bench-matrix` excluded because that file doesn't exist yet); the lib + main bin tests pass (4 tests: 2 percentiles + 2 parse_res).

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/Cargo.toml \
        crates/latency-bench/src/lib.rs \
        crates/latency-bench/src/main.rs
git commit -m "latency-bench: split into lib + bin to share infra with matrix bin"
```

---

## Task 2: Extract `run_for_matrix` and `RunStats`

**Files:**
- Modify: `crates/latency-bench/src/full_pipeline.rs`
- Modify: `crates/latency-bench/src/lib.rs` (re-export RunStats)

- [ ] **Step 1: Add RunStats type to full_pipeline.rs**

In `crates/latency-bench/src/full_pipeline.rs`, after the existing `pub struct StageTimes { ... }` definition (currently lines 80-86), add:

```rust
/// Result of a single bench config run. `frames` is the per-frame raw
/// data; `sent` is the sender's seq counter; `received` is the count
/// of frames that made it through both transport and decode.
pub struct RunStats {
    pub sent: u64,
    pub received: u64,
    pub frames: Vec<StageTimes>,
}
```

Also change `struct StageTimes` to `pub struct StageTimes` so callers outside the file can read its fields (it's currently `struct` — line 80).

- [ ] **Step 2: Refactor run() to call run_for_matrix**

In the same file, replace the existing `pub async fn run(cfg: FullPipelineConfig) -> anyhow::Result<()>` with:

```rust
pub async fn run(cfg: FullPipelineConfig) -> anyhow::Result<()> {
    let csv_path = cfg.csv.clone();
    let stats = run_for_matrix(&cfg).await?;

    if stats.frames.is_empty() {
        info!(sent = stats.sent, decoded = stats.received, "bench done but decoded 0 frames");
        return Ok(());
    }

    // Per-stage latency arrays (computed from frames).
    let mut encode: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.encode_done_us.saturating_sub(s.capture_us))
        .collect();
    let mut transport: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.recv_us.saturating_sub(s.encode_done_us))
        .collect();
    let mut decode: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.decode_done_us.saturating_sub(s.recv_us))
        .collect();
    let mut e2e: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.decode_done_us.saturating_sub(s.capture_us))
        .collect();

    let (e50, _, e95, e99, _) = crate::percentiles(&mut encode);
    let (t50, _, t95, t99, _) = crate::percentiles(&mut transport);
    let (d50, _, d95, d99, _) = crate::percentiles(&mut decode);
    let (w50, _, w95, w99, wmax) = crate::percentiles(&mut e2e);

    info!(
        sent = stats.sent,
        decoded = stats.received,
        encode_p50_us = e50,
        encode_p95_us = e95,
        encode_p99_us = e99,
        transport_p50_us = t50,
        transport_p95_us = t95,
        transport_p99_us = t99,
        decode_p50_us = d50,
        decode_p95_us = d95,
        decode_p99_us = d99,
        e2e_p50_us = w50,
        e2e_p95_us = w95,
        e2e_p99_us = w99,
        e2e_max_us = wmax,
        "full-pipeline bench done",
    );

    if let Some(path) = csv_path {
        use std::io::Write;
        let mut wtr = std::fs::File::create(&path)?;
        writeln!(wtr, "seq,capture_us,encode_done_us,recv_us,decode_done_us,e2e_us")?;
        for s in &stats.frames {
            let e = s.decode_done_us.saturating_sub(s.capture_us);
            writeln!(
                wtr,
                "{},{},{},{},{},{}",
                s.seq, s.capture_us, s.encode_done_us, s.recv_us, s.decode_done_us, e
            )?;
        }
        info!(path = %path.display(), "wrote CSV");
    }

    Ok(())
}
```

The CSV format here (single-config bin) is preserved from the existing implementation for backward compat. The matrix bin in Task 5 writes a different richer CSV.

- [ ] **Step 3: Implement run_for_matrix in full_pipeline.rs**

Append to `crates/latency-bench/src/full_pipeline.rs` (after the new `run()` function from Step 2):

```rust
/// Core bench loop without any I/O. Returns the raw per-frame samples
/// and counters; the caller decides how to log/aggregate/write CSV.
///
/// Used by both the single-config `run()` (which logs + writes one CSV)
/// and the matrix bin (which writes per-frame + summary CSVs).
pub async fn run_for_matrix(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats> {
    let adapter = pick_default_adapter().map_err(|e| anyhow::anyhow!("no GPU adapter: {e}"))?;
    if !adapter.is_nvidia() {
        anyhow::bail!(
            "full-pipeline mode requires an NVIDIA adapter; got {}",
            adapter.name
        );
    }
    let dev = D3d11Device::create(&adapter).map_err(|e| anyhow::anyhow!("D3D11 device: {e}"))?;

    let enc_cfg = NvencEncoderConfig {
        width: cfg.width,
        height: cfg.height,
        fps_numerator: cfg.fps,
        fps_denominator: 1,
        bitrate_bps: cfg.bitrate_bps,
        gop_length: cfg.fps * 2,
    };
    let encoder = NvencEncoder::new(&dev, &enc_cfg)
        .map_err(|e| anyhow::anyhow!("NvencEncoder::new: {e}"))?;

    let mut consumer = match cfg.consumer {
        ConsumerBackend::Mf => BenchConsumer::Mf(
            MfD3d11Consumer::new(&dev, cfg.width, cfg.height)
                .map_err(|e| anyhow::anyhow!("MfD3d11Consumer::new: {e}"))?,
        ),
        ConsumerBackend::Nvdec => BenchConsumer::Nvdec(
            NvdecD3d11Consumer::new(&dev, cfg.width, cfg.height)
                .map_err(|e| anyhow::anyhow!("NvdecD3d11Consumer::new: {e}"))?,
        ),
    };

    let (host_side, viewer_side) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: cfg.drop_ppm,
        latency: if cfg.latency_ms > 0 {
            Some(Duration::from_millis(cfg.latency_ms))
        } else {
            None
        },
    });

    let frame_interval = Duration::from_secs_f64(1.0 / cfg.fps as f64);
    let deadline = Instant::now() + cfg.duration;
    let mut samples: Vec<StageTimes> = Vec::new();
    let mut next_tick = Instant::now();
    let mut seq: u64 = 0;
    let mut decoded: u64 = 0;

    while Instant::now() < deadline {
        let capture_us = now_monotonic_us();
        let tex = make_counter_texture(&dev, cfg.width, cfg.height, seq as u32)
            .map_err(|e| anyhow::anyhow!("synthetic texture: {e}"))?;
        let force_idr = seq == 0;
        let encoded = encoder
            .encode(&tex, force_idr, capture_us)
            .map_err(|e| anyhow::anyhow!("encode: {e}"))?;
        let encode_done_us = now_monotonic_us();

        let frame = EncodedFrame::new_h265(
            seq,
            capture_us,
            encoded.is_keyframe,
            bytes::Bytes::from(encoded.nal_bytes),
            cfg.width,
            cfg.height,
        );
        if let Err(e) = host_side.send_video(frame).await {
            warn!(?e, seq, "send_video failed; stopping");
            break;
        }

        loop {
            match tokio::time::timeout(Duration::from_millis(1), viewer_side.recv()).await {
                Ok(Ok(ReceivedMessage::Video(rx_frame))) => {
                    let recv_us = now_monotonic_us();
                    let rx_seq = rx_frame.seq;
                    let rx_capture_us = rx_frame.timestamp_host_us;
                    match consumer.submit(rx_frame).await {
                        Ok(()) => {}
                        Err(ConsumerError::Decode(msg)) => {
                            warn!(seq = rx_seq, msg, "decode error");
                            continue;
                        }
                        Err(e) => {
                            warn!(?e, "consumer error");
                            continue;
                        }
                    }
                    if consumer.take_latest_texture() {
                        let decode_done_us = now_monotonic_us();
                        decoded += 1;
                        samples.push(StageTimes {
                            seq: rx_seq,
                            capture_us: rx_capture_us,
                            encode_done_us,
                            recv_us,
                            decode_done_us,
                        });
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        seq += 1;
        next_tick += frame_interval;
        let sleep_until = next_tick;
        let now = Instant::now();
        if sleep_until > now {
            tokio::time::sleep(sleep_until - now).await;
        }
    }

    // Drain remaining decoded frames.
    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(50), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(rx_frame))) => {
                let recv_us = now_monotonic_us();
                let rx_seq = rx_frame.seq;
                let rx_capture_us = rx_frame.timestamp_host_us;
                let _ = consumer.submit(rx_frame).await;
                if consumer.take_latest_texture() {
                    let decode_done_us = now_monotonic_us();
                    decoded += 1;
                    samples.push(StageTimes {
                        seq: rx_seq,
                        capture_us: rx_capture_us,
                        encode_done_us: recv_us,
                        recv_us,
                        decode_done_us,
                    });
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    Ok(RunStats {
        sent: seq,
        received: decoded,
        frames: samples,
    })
}
```

This is the existing `run()` body minus the trailing log + CSV write block.

**Important**: Now delete the old body of the previously-existing `run()` (everything between its old fn signature and the trailing `Ok(())`). The new `run()` from Step 2 replaces it entirely. Since both Step 2 and Step 3 produced new code in the same file, ensure the file ends up with exactly 2 functions: `run()` (thin wrapper) + `run_for_matrix()` (core).

- [ ] **Step 4: Re-export RunStats + run_for_matrix from lib.rs**

Edit `crates/latency-bench/src/lib.rs`. Below the `pub mod full_pipeline;` line, add:

```rust
#[cfg(windows)]
pub use full_pipeline::{ConsumerBackend, FullPipelineConfig, RunStats, StageTimes};
```

- [ ] **Step 5: Build + test**

```bash
cargo build -p prdt-latency-bench --lib --bin prdt-latency-bench
cargo test -p prdt-latency-bench --lib --bin prdt-latency-bench
```

Expected: clean build + 4 tests pass. The single-config bin still has identical user-facing behaviour (CSV format unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/latency-bench/src/full_pipeline.rs \
        crates/latency-bench/src/lib.rs
git commit -m "latency-bench: extract run_for_matrix from run() (no behaviour change)"
```

---

## Task 3: MatrixAxes + expand_matrix + ConfigStats + aggregate

**Files:**
- Modify: `crates/latency-bench/src/lib.rs` (add new types + tests)

- [ ] **Step 1: Write the failing tests**

Edit `crates/latency-bench/src/lib.rs`. At the bottom, replace the existing `#[cfg(test)] mod tests { ... }` block with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_monotonic() {
        let mut v: Vec<u64> = (1..=100).collect();
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert!(p50 <= p90);
        assert!(p90 <= p95);
        assert!(p95 <= p99);
        assert!(p99 <= p100);
        assert_eq!(p100, 100);
    }

    #[test]
    fn percentiles_single_sample() {
        let mut v = vec![42u64];
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert_eq!((p50, p90, p95, p99, p100), (42, 42, 42, 42, 42));
    }

    #[cfg(windows)]
    #[test]
    fn config_id_format_canonical() {
        let id = config_id((1920, 1080), 60, 30, ConsumerBackend::Mf);
        assert_eq!(id, "1080p60-30mbps-mf");

        let id = config_id((3840, 2160), 120, 50, ConsumerBackend::Nvdec);
        assert_eq!(id, "2160p120-50mbps-nvdec");
    }

    #[cfg(windows)]
    #[test]
    fn expand_matrix_produces_cartesian_product() {
        let axes = MatrixAxes {
            resolutions: vec![(1920, 1080), (2560, 1440)],
            bitrates_mbps: vec![10, 30],
            decoders: vec![ConsumerBackend::Mf],
            fps: vec![60],
            duration: std::time::Duration::from_secs(10),
        };
        let configs = expand_matrix(&axes);
        // 2 * 2 * 1 * 1 = 4 configs
        assert_eq!(configs.len(), 4);
        // Order: outermost = resolution, then bitrate, then decoder, then fps
        assert_eq!((configs[0].width, configs[0].height), (1920, 1080));
        assert_eq!(configs[0].bitrate_bps, 10_000_000);
        assert_eq!((configs[1].width, configs[1].height), (1920, 1080));
        assert_eq!(configs[1].bitrate_bps, 30_000_000);
        assert_eq!((configs[2].width, configs[2].height), (2560, 1440));
        assert_eq!(configs[2].bitrate_bps, 10_000_000);
        assert_eq!((configs[3].width, configs[3].height), (2560, 1440));
        assert_eq!(configs[3].bitrate_bps, 30_000_000);
    }

    #[cfg(windows)]
    #[test]
    fn aggregate_empty_run_emits_skip_row() {
        let cfg = FullPipelineConfig {
            width: 1920, height: 1080, fps: 60,
            duration: std::time::Duration::from_secs(10),
            bitrate_bps: 30_000_000, drop_ppm: 0, latency_ms: 0,
            csv: None, consumer: ConsumerBackend::Mf,
        };
        let run = RunStats { sent: 0, received: 0, frames: vec![] };
        let stats = aggregate(&cfg, &run);
        assert_eq!(stats.config_id, "1080p60-30mbps-mf");
        assert_eq!(stats.loss_ppm, 1_000_000);
        assert_eq!(stats.arrival_p50_us, 0);
        assert_eq!(stats.e2e_p99_us, 0);
    }

    #[cfg(windows)]
    #[test]
    fn aggregate_full_run_computes_percentiles() {
        let cfg = FullPipelineConfig {
            width: 1920, height: 1080, fps: 60,
            duration: std::time::Duration::from_secs(10),
            bitrate_bps: 30_000_000, drop_ppm: 0, latency_ms: 0,
            csv: None, consumer: ConsumerBackend::Mf,
        };
        // 100 frames with arrival_lag_us = i, decode_lag_us = 2*i, e2e = 3*i.
        let frames: Vec<StageTimes> = (1..=100u64)
            .map(|i| StageTimes {
                seq: i,
                capture_us: 0,
                encode_done_us: 0,
                recv_us: i,
                decode_done_us: 3 * i,
            })
            .collect();
        let run = RunStats { sent: 100, received: 100, frames };
        let stats = aggregate(&cfg, &run);
        assert_eq!(stats.sent, 100);
        assert_eq!(stats.received, 100);
        assert_eq!(stats.loss_ppm, 0);
        // arrival_lag = recv - capture = i; p50 of 1..=100 = 51 (round)
        assert_eq!(stats.arrival_p50_us, 51);
        assert_eq!(stats.arrival_p95_us, 96);
        assert_eq!(stats.arrival_p99_us, 100);
        // e2e_lag = decode_done - capture = 3i; p50 = 153, p95 = 288, p99 = 300
        assert_eq!(stats.e2e_p50_us, 153);
        assert_eq!(stats.e2e_p99_us, 300);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --lib 2>&1 | tail -10
```

Expected: compile errors — `cannot find value/type config_id`, `cannot find type MatrixAxes`, `cannot find function expand_matrix`, `cannot find function aggregate`, `no field config_id on type ConfigStats`, etc.

- [ ] **Step 3: Implement the new types in lib.rs**

In `crates/latency-bench/src/lib.rs`, between the `pub use full_pipeline::...` line and the `#[cfg(test)] mod tests` block, add:

```rust
#[cfg(windows)]
mod matrix {
    use super::{percentiles, ConsumerBackend, FullPipelineConfig, RunStats, StageTimes};

    /// CLI-supplied axes for the matrix bin.
    pub struct MatrixAxes {
        pub resolutions: Vec<(u32, u32)>,
        pub bitrates_mbps: Vec<u32>,
        pub decoders: Vec<ConsumerBackend>,
        pub fps: Vec<u32>,
        pub duration: std::time::Duration,
    }

    /// One row of summary.csv.
    pub struct ConfigStats {
        pub config_id: String,
        pub resolution: (u32, u32),
        pub bitrate_mbps: u32,
        pub decoder: ConsumerBackend,
        pub fps: u32,
        pub sent: u64,
        pub received: u64,
        pub loss_ppm: u64,
        pub arrival_p50_us: u64,
        pub arrival_p95_us: u64,
        pub arrival_p99_us: u64,
        pub decode_p50_us: u64,
        pub decode_p95_us: u64,
        pub decode_p99_us: u64,
        pub e2e_p50_us: u64,
        pub e2e_p95_us: u64,
        pub e2e_p99_us: u64,
    }

    /// Stable, filesystem-safe identifier for a config: `{height}p{fps}-{bitrate}mbps-{decoder}`.
    pub fn config_id(
        resolution: (u32, u32),
        fps: u32,
        bitrate_mbps: u32,
        decoder: ConsumerBackend,
    ) -> String {
        let dec = match decoder {
            ConsumerBackend::Mf => "mf",
            ConsumerBackend::Nvdec => "nvdec",
        };
        format!("{}p{}-{}mbps-{}", resolution.1, fps, bitrate_mbps, dec)
    }

    /// Expand axes into a `Vec<FullPipelineConfig>`. Order:
    /// resolution outer → bitrate → decoder → fps inner.
    pub fn expand_matrix(axes: &MatrixAxes) -> Vec<FullPipelineConfig> {
        let mut out = Vec::with_capacity(
            axes.resolutions.len()
                * axes.bitrates_mbps.len()
                * axes.decoders.len()
                * axes.fps.len(),
        );
        for &res in &axes.resolutions {
            for &bitrate_mbps in &axes.bitrates_mbps {
                for &decoder in &axes.decoders {
                    for &fps in &axes.fps {
                        out.push(FullPipelineConfig {
                            width: res.0,
                            height: res.1,
                            fps,
                            duration: axes.duration,
                            bitrate_bps: bitrate_mbps.saturating_mul(1_000_000),
                            drop_ppm: 0,
                            latency_ms: 0,
                            csv: None,
                            consumer: decoder,
                        });
                    }
                }
            }
        }
        out
    }

    /// Aggregate per-frame raw into the summary row. Empty `frames` produces
    /// a "skip row" with `loss_ppm = 1_000_000` and all percentiles = 0.
    pub fn aggregate(cfg: &FullPipelineConfig, run: &RunStats) -> ConfigStats {
        let id = config_id(
            (cfg.width, cfg.height),
            cfg.fps,
            (cfg.bitrate_bps / 1_000_000) as u32,
            cfg.consumer,
        );
        if run.frames.is_empty() {
            return ConfigStats {
                config_id: id,
                resolution: (cfg.width, cfg.height),
                bitrate_mbps: (cfg.bitrate_bps / 1_000_000) as u32,
                decoder: cfg.consumer,
                fps: cfg.fps,
                sent: run.sent,
                received: run.received,
                loss_ppm: 1_000_000,
                arrival_p50_us: 0,
                arrival_p95_us: 0,
                arrival_p99_us: 0,
                decode_p50_us: 0,
                decode_p95_us: 0,
                decode_p99_us: 0,
                e2e_p50_us: 0,
                e2e_p95_us: 0,
                e2e_p99_us: 0,
            };
        }
        let mut arrival: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.recv_us.saturating_sub(s.capture_us))
            .collect();
        let mut decode: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.decode_done_us.saturating_sub(s.recv_us))
            .collect();
        let mut e2e: Vec<u64> = run
            .frames
            .iter()
            .map(|s| s.decode_done_us.saturating_sub(s.capture_us))
            .collect();
        let (a50, _, a95, a99, _) = percentiles(&mut arrival);
        let (d50, _, d95, d99, _) = percentiles(&mut decode);
        let (e50, _, e95, e99, _) = percentiles(&mut e2e);
        let loss_ppm = if run.sent > 0 {
            ((run.sent.saturating_sub(run.received)) as f64 / run.sent as f64
                * 1_000_000.0) as u64
        } else {
            0
        };
        ConfigStats {
            config_id: id,
            resolution: (cfg.width, cfg.height),
            bitrate_mbps: (cfg.bitrate_bps / 1_000_000) as u32,
            decoder: cfg.consumer,
            fps: cfg.fps,
            sent: run.sent,
            received: run.received,
            loss_ppm,
            arrival_p50_us: a50,
            arrival_p95_us: a95,
            arrival_p99_us: a99,
            decode_p50_us: d50,
            decode_p95_us: d95,
            decode_p99_us: d99,
            e2e_p50_us: e50,
            e2e_p95_us: e95,
            e2e_p99_us: e99,
        }
    }

    // The per-frame and summary CSV writers live in this module too, in
    // Task 4. (Type stubs go here so this module compiles standalone.)
}

#[cfg(windows)]
pub use matrix::{aggregate, config_id, expand_matrix, ConfigStats, MatrixAxes};
```

Note: Task 4 will append `write_per_frame_csv` + `write_summary_csv` to the same `mod matrix` block.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --lib 2>&1 | tail -10
```

Expected: 7 tests pass (2 percentiles + 5 new matrix tests).

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/lib.rs
git commit -m "latency-bench: add MatrixAxes + expand_matrix + ConfigStats + aggregate"
```

---

## Task 4: CSV writers (per-frame + summary)

**Files:**
- Modify: `crates/latency-bench/src/lib.rs` (extend `mod matrix` with writers + 1 new test)

- [ ] **Step 1: Write the failing test**

In `crates/latency-bench/src/lib.rs`'s test block, append:

```rust
    #[cfg(windows)]
    #[test]
    fn summary_csv_writer_emits_header_and_one_row() {
        let cfg = FullPipelineConfig {
            width: 1920, height: 1080, fps: 60,
            duration: std::time::Duration::from_secs(10),
            bitrate_bps: 30_000_000, drop_ppm: 0, latency_ms: 0,
            csv: None, consumer: ConsumerBackend::Mf,
        };
        let run = RunStats { sent: 600, received: 598, frames: vec![
            StageTimes { seq: 0, capture_us: 0, encode_done_us: 100, recv_us: 200, decode_done_us: 300 },
        ]};
        let s = aggregate(&cfg, &run);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.csv");
        write_summary_csv(&path, std::slice::from_ref(&s)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row");
        assert!(
            lines[0].starts_with("config_id,resolution,bitrate_mbps,decoder,fps,"),
            "unexpected header: {}", lines[0]
        );
        assert!(lines[1].starts_with("1080p60-30mbps-mf,1920x1080,30,mf,60,600,598,"),
            "unexpected row: {}", lines[1]);
    }

    #[cfg(windows)]
    #[test]
    fn per_frame_csv_writer_round_trips() {
        let frames = vec![
            StageTimes { seq: 0, capture_us: 0, encode_done_us: 100, recv_us: 200, decode_done_us: 300 },
            StageTimes { seq: 1, capture_us: 16_667, encode_done_us: 16_770, recv_us: 16_870, decode_done_us: 16_970 },
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames.csv");
        write_per_frame_csv(&path, &frames).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows");
        assert_eq!(
            lines[0],
            "seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us"
        );
        // Row 0: arrival = 200-0 = 200, decode_lag = 300-200 = 100, e2e = 300-0 = 300
        assert!(lines[1].ends_with(",200,100,300"), "got: {}", lines[1]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --lib 2>&1 | tail -10
```

Expected: compile errors — `cannot find function write_summary_csv`, `cannot find function write_per_frame_csv`.

- [ ] **Step 3: Implement writers**

Inside `mod matrix` (in `crates/latency-bench/src/lib.rs`), append before the closing `}` of the `mod matrix { ... }` block:

```rust
    use std::io::Write;
    use std::path::Path;

    /// Write per-frame raw CSV. Header:
    /// `seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us`.
    pub fn write_per_frame_csv(path: &Path, frames: &[StageTimes]) -> std::io::Result<()> {
        let mut wtr = std::fs::File::create(path)?;
        writeln!(
            wtr,
            "seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us"
        )?;
        for s in frames {
            let arrival = s.recv_us.saturating_sub(s.capture_us);
            let decode = s.decode_done_us.saturating_sub(s.recv_us);
            let e2e = s.decode_done_us.saturating_sub(s.capture_us);
            writeln!(
                wtr,
                "{},{},{},{},{},{},{},{}",
                s.seq, s.capture_us, s.encode_done_us, s.recv_us, s.decode_done_us,
                arrival, decode, e2e
            )?;
        }
        Ok(())
    }

    /// Write summary.csv across all configs. Header per spec.
    pub fn write_summary_csv(path: &Path, stats: &[ConfigStats]) -> std::io::Result<()> {
        let mut wtr = std::fs::File::create(path)?;
        writeln!(
            wtr,
            "config_id,resolution,bitrate_mbps,decoder,fps,sent,received,loss_ppm,\
             arrival_p50_us,arrival_p95_us,arrival_p99_us,\
             decode_p50_us,decode_p95_us,decode_p99_us,\
             e2e_p50_us,e2e_p95_us,e2e_p99_us"
        )?;
        for s in stats {
            let dec = match s.decoder {
                ConsumerBackend::Mf => "mf",
                ConsumerBackend::Nvdec => "nvdec",
            };
            writeln!(
                wtr,
                "{},{}x{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                s.config_id,
                s.resolution.0, s.resolution.1,
                s.bitrate_mbps, dec, s.fps,
                s.sent, s.received, s.loss_ppm,
                s.arrival_p50_us, s.arrival_p95_us, s.arrival_p99_us,
                s.decode_p50_us, s.decode_p95_us, s.decode_p99_us,
                s.e2e_p50_us, s.e2e_p95_us, s.e2e_p99_us
            )?;
        }
        Ok(())
    }
```

Then update the public re-export to include the writers. Edit the existing `pub use matrix::...` line to:

```rust
#[cfg(windows)]
pub use matrix::{
    aggregate, config_id, expand_matrix, write_per_frame_csv, write_summary_csv,
    ConfigStats, MatrixAxes,
};
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --lib 2>&1 | tail -15
```

Expected: 9 tests pass (2 percentiles + 5 matrix + 2 csv writers).

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/lib.rs
git commit -m "latency-bench: add per-frame and summary CSV writers"
```

---

## Task 5: `prdt-bench-matrix` bin

**Files:**
- Create: `crates/latency-bench/src/bin/bench-matrix.rs`

- [ ] **Step 1: Verify the bin path exists**

```bash
mkdir -p crates/latency-bench/src/bin
ls crates/latency-bench/src/bin/
```

Expected: empty directory.

- [ ] **Step 2: Create the bin**

Create `crates/latency-bench/src/bin/bench-matrix.rs`:

```rust
//! Plan 4 B1 bench matrix bin. Sweeps the cartesian product of
//! resolutions × bitrates × decoders × fps and writes per-frame raw
//! CSVs + a summary CSV.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use prdt_latency_bench::{
    aggregate, config_id, expand_matrix, full_pipeline, write_per_frame_csv,
    write_summary_csv, ConfigStats, ConsumerBackend, MatrixAxes,
};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "prdt-bench-matrix",
    about = "Plan 4 B1 bench matrix: sweep resolutions × bitrates × decoders × fps"
)]
struct Args {
    /// Output dir. Will contain `summary.csv` and `per-frame/<config_id>.csv`.
    /// Created if missing; existing files are overwritten.
    #[arg(long)]
    out_dir: PathBuf,

    /// Comma-separated heights (16:9 width auto-derived).
    #[arg(long, value_delimiter = ',', default_values_t = vec![1080u32, 1440u32, 2160u32])]
    resolutions: Vec<u32>,

    /// Comma-separated bitrates in Mbps.
    #[arg(long, value_delimiter = ',', default_values_t = vec![5u32, 10u32, 20u32, 30u32, 50u32])]
    bitrates: Vec<u32>,

    /// Comma-separated decoders. Choices: mf, nvdec.
    #[arg(long, value_delimiter = ',', default_values_t = vec!["mf".to_string(), "nvdec".to_string()])]
    decoders: Vec<String>,

    /// Comma-separated fps.
    #[arg(long, value_delimiter = ',', default_values_t = vec![60u32, 120u32])]
    fps: Vec<u32>,

    /// Per-config bench duration.
    #[arg(long, default_value = "10s")]
    duration: humantime::Duration,

    /// Print the matrix and exit without running.
    #[arg(long)]
    dry_run: bool,
}

fn parse_decoders(strs: &[String]) -> anyhow::Result<Vec<ConsumerBackend>> {
    strs.iter()
        .map(|s| match s.as_str() {
            "mf" => Ok(ConsumerBackend::Mf),
            "nvdec" => Ok(ConsumerBackend::Nvdec),
            other => Err(anyhow::anyhow!("unknown decoder {other:?} (options: mf, nvdec)")),
        })
        .collect()
}

fn heights_to_resolutions(heights: &[u32]) -> Vec<(u32, u32)> {
    heights.iter().map(|h| (h * 16 / 9, *h)).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let decoders = parse_decoders(&args.decoders)?;
    let resolutions = heights_to_resolutions(&args.resolutions);
    let axes = MatrixAxes {
        resolutions,
        bitrates_mbps: args.bitrates,
        decoders,
        fps: args.fps,
        duration: Duration::from(args.duration),
    };
    let configs = expand_matrix(&axes);
    info!(count = configs.len(), "matrix expanded");

    if args.dry_run {
        for (i, c) in configs.iter().enumerate() {
            let id = config_id(
                (c.width, c.height),
                c.fps,
                (c.bitrate_bps / 1_000_000) as u32,
                c.consumer,
            );
            println!("[{:>3}/{}] {}", i + 1, configs.len(), id);
        }
        return Ok(());
    }

    std::fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("create out_dir {}", args.out_dir.display()))?;
    let per_frame_dir = args.out_dir.join("per-frame");
    std::fs::create_dir_all(&per_frame_dir)
        .with_context(|| format!("create {}", per_frame_dir.display()))?;

    let mut all_stats: Vec<ConfigStats> = Vec::with_capacity(configs.len());
    let mut skipped: u64 = 0;
    for (i, cfg) in configs.iter().enumerate() {
        let id = config_id(
            (cfg.width, cfg.height),
            cfg.fps,
            (cfg.bitrate_bps / 1_000_000) as u32,
            cfg.consumer,
        );
        info!(
            "[{:>3}/{}] running {} duration={:?}",
            i + 1,
            configs.len(),
            id,
            cfg.duration
        );
        match full_pipeline::run_for_matrix(cfg).await {
            Ok(run) => {
                let frame_path = per_frame_dir.join(format!("{id}.csv"));
                if let Err(e) = write_per_frame_csv(&frame_path, &run.frames) {
                    warn!(?e, path = %frame_path.display(), "per-frame CSV write failed");
                }
                let stats = aggregate(cfg, &run);
                info!(
                    "[{:>3}/{}] done    {} received={}/{} e2e_p95_us={}",
                    i + 1,
                    configs.len(),
                    id,
                    stats.received,
                    stats.sent,
                    stats.e2e_p95_us
                );
                all_stats.push(stats);
            }
            Err(e) => {
                warn!(?e, config_id = %id, "config failed; skip row will be emitted");
                let empty = prdt_latency_bench::RunStats {
                    sent: 0,
                    received: 0,
                    frames: vec![],
                };
                all_stats.push(aggregate(cfg, &empty));
                skipped += 1;
            }
        }
    }

    let summary_path = args.out_dir.join("summary.csv");
    write_summary_csv(&summary_path, &all_stats)
        .with_context(|| format!("write {}", summary_path.display()))?;
    info!(
        path = %summary_path.display(),
        rows = all_stats.len(),
        skipped,
        "summary written"
    );
    Ok(())
}
```

- [ ] **Step 3: Build + smoke (dry-run)**

```bash
cargo build --release -p prdt-latency-bench --bin prdt-bench-matrix
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir /tmp/dry --dry-run 2>&1 | tail -10
```

Expected: build succeeds. Dry-run prints exactly 60 lines (default axes) like:

```
[  1/60] 1080p60-5mbps-mf
[  2/60] 1080p60-5mbps-nvdec
...
[ 60/60] 2160p120-50mbps-nvdec
```

The `out-dir` is required by clap but `--dry-run` exits before creating it, so `/tmp/dry` doesn't actually need to exist.

- [ ] **Step 4: Smoke a tiny matrix end-to-end**

To exercise the actual run path without burning 15 minutes:

```bash
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/smoke/ \
    --resolutions 1080 \
    --bitrates 30 \
    --decoders mf \
    --fps 60 \
    --duration 3s 2>&1 | tail -10

ls bench-results/smoke/
ls bench-results/smoke/per-frame/
head -2 bench-results/smoke/summary.csv
```

Expected:
- `bench-results/smoke/summary.csv` exists with 1 header + 1 data row
- `bench-results/smoke/per-frame/1080p60-30mbps-mf.csv` exists with 1 header + ~180 frame rows (3s × 60fps)
- The summary.csv data row starts with `1080p60-30mbps-mf,1920x1080,30,mf,60,...`

If NVENC isn't available on the dev machine, this step will fail with "no GPU adapter" or similar. In that case skip and report status as `DONE_WITH_CONCERNS` — the code path is exercised by Task 3+4 unit tests.

- [ ] **Step 5: Workspace clippy**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/latency-bench/src/bin/bench-matrix.rs
git commit -m "latency-bench: add prdt-bench-matrix bin (60-config sweep)"
```

---

## Task 6: docs + final validation + tag

**Files:**
- Create: `docs/bench-matrix.md`

- [ ] **Step 1: Write docs/bench-matrix.md**

Create `docs/bench-matrix.md`:

```markdown
# Bench matrix (Plan 4 B1)

The `prdt-bench-matrix` bin sweeps the cartesian product of
**resolutions × bitrates × decoders × fps** through the in-process
loopback NVENC + MF/NVDEC pipeline. Each config records per-frame raw
samples and aggregates to a single row in `summary.csv`.

## Quick start

```bash
# Default 60-config sweep (3 res × 5 bitrates × 2 decoders × 2 fps,
# 10s each, ~15-20 min total on RTX 3070 Ti).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/2026-04-26/

# Custom subset (e.g. only NVDEC at 60fps).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/nvdec-only/ \
    --decoders nvdec \
    --fps 60

# Dry-run (print configs, don't execute).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` + `per-frame/` go here. Overwrites existing files. |
| `--resolutions <heights>` | `1080,1440,2160` | 16:9 width auto-derived |
| `--bitrates <mbps>` | `5,10,20,30,50` | Comma-separated Mbps |
| `--decoders <list>` | `mf,nvdec` | Choices: `mf`, `nvdec` |
| `--fps <list>` | `60,120` | Comma-separated |
| `--duration <humantime>` | `10s` | Per-config bench length |
| `--dry-run` | off | List configs, exit |

## Output layout

```
bench-results/<date>/
  summary.csv                  # 1 header + N config rows
  per-frame/
    1080p60-5mbps-mf.csv       # 1 header + ~600 frame rows
    1080p60-5mbps-nvdec.csv
    ...
    2160p120-50mbps-nvdec.csv
```

`config_id` format: `{height}p{fps}-{bitrate}mbps-{decoder}` —
ASCII, filesystem-safe, used as both the per-frame filename and
the leftmost column of `summary.csv`.

## summary.csv schema

```
config_id,resolution,bitrate_mbps,decoder,fps,sent,received,loss_ppm,
arrival_p50_us,arrival_p95_us,arrival_p99_us,
decode_p50_us,decode_p95_us,decode_p99_us,
e2e_p50_us,e2e_p95_us,e2e_p99_us
```

- `arrival_lag = recv_us - capture_us` (post-encode → arrived at the receive end)
- `decode_lag = decode_done_us - recv_us`
- `e2e_lag = decode_done_us - capture_us` — proxy for glass-to-glass; a
  true present-time stamp requires Plan 4 M3 camera measurement.

A skipped config (NVENC init failure, decoder unsupported, etc.)
emits a row with `loss_ppm = 1000000` and all percentiles = 0. The
log will say `config failed; skip row will be emitted`.

## per-frame/<config_id>.csv schema

```
seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us
```

The lag columns are pre-computed for analysis convenience.

## Sample interpretation

A row reading

```
1080p60-30mbps-nvdec,1920x1080,30,nvdec,60,600,600,0,1234,1890,2100,420,580,720,4500,7200,9100
```

means: 600 frames sent and all 600 received (loss_ppm=0); transport
arrival p95 was 1.89ms, decode p95 was 0.58ms, end-to-end p95 was
7.2ms (p99 9.1ms). Compared with the same config under MF decoder,
expect NVDEC to be lower across all three stages thanks to the
zero-copy CUDA→D3D11 path (Plan 2d zerocopy).

## Limitations

- **No GPU adapter / non-NVIDIA**: bin fails fast on first config with
  "no GPU adapter" (NVENC requires NVIDIA).
- **Single-process loopback**: encode and decode share the same GPU,
  same monotonic clock. Real 2-machine LAN behaviour will differ
  (clock-offset correction needed via Plan 4 M3 ping/pong).
- **No present_us**: the bin renders nothing; `e2e_lag` ends at
  `decode_done_us`. True glass-to-glass requires Plan 4 M3.
- **Resume not supported**: a run that crashes mid-sweep loses progress
  beyond what's already written under `per-frame/`. Re-run with a
  reduced axis subset to fill in.
```

- [ ] **Step 2: Run full test + clippy**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -5
```

Expected: `total: 284 failed: 0` (was 277 → +7 from Tasks 1, 3, 4); clippy clean.

If a smaller delta appears, count which new tests are missing and fix.

- [ ] **Step 3: Format**

```bash
rustfmt \
  crates/latency-bench/src/lib.rs \
  crates/latency-bench/src/main.rs \
  crates/latency-bench/src/full_pipeline.rs \
  crates/latency-bench/src/bin/bench-matrix.rs
git diff --stat
```

If non-empty:

```bash
git add -u
git commit -m "latency-bench: cargo fmt"
```

- [ ] **Step 4: Commit docs**

```bash
git add docs/bench-matrix.md
git commit -m "docs: add bench-matrix.md (Plan 4 B1 usage guide)"
```

- [ ] **Step 5: Manual smoke (optional, defer to controller)**

This step requires NVENC + at least 2 minutes of GPU time. The implementer subagent should NOT run the full 60-config sweep — controller decides when to do that. Just confirm the CLI parses default args:

```bash
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir /tmp/dry-default --dry-run | wc -l
```

Expected: `60`.

- [ ] **Step 6: Tag**

```bash
git tag -a plan4-b1-bench-matrix-complete -m "$(cat <<'EOF'
Plan 4 B1 bench matrix complete

Adds prdt-bench-matrix bin to latency-bench crate. Sweeps the cartesian
product of resolutions × bitrates × decoders × fps (60 default configs)
and writes summary.csv + per-frame/<id>.csv.

- latency-bench refactored: lib + main bin + matrix bin
- run_for_matrix(cfg) -> RunStats core extracted from existing run()
- MatrixAxes / ConfigStats / config_id / expand_matrix / aggregate
- write_summary_csv + write_per_frame_csv (pre-computed lag columns)
- Default axes: heights 1080/1440/2160, bitrates 5/10/20/30/50 Mbps,
  decoders mf/nvdec, fps 60/120, duration 10s per config
- 7 new unit tests covering axis expansion, config_id format,
  aggregate (full + skip rows), CSV writers
- docs/bench-matrix.md with usage + schema + sample interpretation
- Run-time on RTX 3070 Ti: ~15-20 min for the 60-config default sweep

Out of scope: B3 AV1 (NVENC AV1 unsupported), B4 LAN/TURN comparison
(requires 2-machine automation), B6 FEC sweep, B7 input round-trip,
B8 30-min stability, heatmap image generation, real glass-to-glass
(Plan 4 M3 camera measurement).
EOF
)"
git tag | grep plan4
```

Expected: `plan4-b1-bench-matrix-complete` listed.

- [ ] **Step 7: Final summary report**

Report back:

- All commits since `master..HEAD` (or the working branch)
- Workspace test count + delta from 277
- Clippy result
- Tag listing
- Manual smoke status (dry-run pass / full run deferred)
- Files added: `lib.rs`, `bin/bench-matrix.rs`, `docs/bench-matrix.md`
- Files modified: `Cargo.toml`, `main.rs`, `full_pipeline.rs`
- Note: actual benchmarking results require running with `--out-dir <real-path>` on Machine A; this plan delivers the tool, not the data

---

## Risks & Notes for Implementer

- **`#[cfg(windows)]` on `mod matrix`** is required because `MatrixAxes` references `ConsumerBackend` which lives in the windows-only `full_pipeline` module. Ensure all matrix tests are also `#[cfg(windows)]` so `cargo test` on non-Windows passes (only the percentile tests run there).
- **`cfg(windows)` on bin file**: the new bin starts with `#![cfg(windows)]`. On non-Windows, the file compiles to an empty crate but the `[[bin]]` declaration still attempts to link a `main`. Add `#![cfg(windows)]` at the top — on Linux the build will fail with "no main", but `cargo build -p prdt-latency-bench --bin prdt-bench-matrix` is only run on Windows in this project. If CI also runs on Linux, gate the bin in Cargo.toml via `[target.'cfg(windows)'.dependencies]` or move it to `examples/` later. **For now: Windows-only is acceptable** (matches the rest of the project's matrix bins).
- **`humantime::Duration` from clap**: requires the `humantime` crate (already in deps). The `Duration::from(args.duration)` conversion is the standard pattern.
- **`pub use full_pipeline::ConsumerBackend`** at the lib level needs `#[cfg(windows)]` because `full_pipeline` itself is windows-gated.
- **`StageTimes` privacy**: ensure the struct fields are `pub` (Task 2 Step 1 says to change `struct StageTimes` to `pub struct StageTimes` — both the struct AND all 5 fields need `pub`).
- **`tempfile`** is a dev-dep; it's already used by other crates' tests at workspace level so the version `3` matches.
- **CSV writer error path**: `write_per_frame_csv` returns `io::Result`. The bin logs a warning on failure but continues to the next config. Don't `?` it — partial results are valuable.
- **Skip row's `loss_ppm`**: spec says `1_000_000`. The aggregate function handles this branch; the CSV row will show `1000000` (no formatting). Operators reading the CSV know that = 100% loss = config skipped.
- **Default args use `default_values_t = vec![...]`**: clap derive's syntax for `Vec<T>` defaults. Required `value_delimiter = ','` so users can pass `--resolutions 1080,1440,2160` instead of repeating the flag.
- **Bin's `prdt_latency_bench::full_pipeline` import**: the lib re-exports `pub mod full_pipeline` (it's `pub mod` not just `mod`), so `prdt_latency_bench::full_pipeline::run_for_matrix` works.

---

## Self-Review

**Spec coverage:**
- §Architecture (lib + 2 bins, full_pipeline split, run_for_matrix) → Task 1, 2 ✓
- §共有型 (RunStats, ConfigStats, MatrixAxes) → Task 2, 3 ✓
- §CLI 7 flags + defaults → Task 5 Args struct ✓
- §データフロー 5 steps → Task 5 main() body ✓
- §構成 ID 命名 stable string → Task 3 config_id() ✓
- §per-frame CSV 8 columns → Task 4 write_per_frame_csv ✓
- §summary CSV 18 columns → Task 4 write_summary_csv ✓
- §エラーハンドリング (NVENC init fail / decoder fail / panic / out-dir overwrite) → Task 5 main loop's match arm + spec note ✓
- §進捗 logging (per config running/done) → Task 5 main() info! calls ✓
- §テスト 5 unit tests → Tasks 3, 4 (5 tests total: config_id, expand, aggregate-empty, aggregate-full, summary-writer + per-frame-writer = 6 actually) ✓
- §Exit criteria 6 items → Task 6 covers all 6 (build, test, clippy, manual smoke, docs, tag) ✓

**Placeholder scan:** No "TBD", "TODO", or vague stubs. Skip-row's `loss_ppm = 1_000_000` documented as intentional out-of-scope marker.

**Type consistency:**
- `ConfigStats { config_id, resolution, bitrate_mbps, decoder, fps, sent, received, loss_ppm, arrival_p50_us, arrival_p95_us, arrival_p99_us, decode_p50_us, decode_p95_us, decode_p99_us, e2e_p50_us, e2e_p95_us, e2e_p99_us }` — used identically in Task 3 (struct def), Task 4 (write_summary_csv), Task 6 (docs schema). 17 fields total ✓
- `RunStats { sent, received, frames }` — Task 2 def, Task 3+4 use, Task 5 use ✓
- `MatrixAxes { resolutions, bitrates_mbps, decoders, fps, duration }` — Task 3 def, Task 5 use ✓
- `config_id(resolution, fps, bitrate_mbps, decoder)` — Task 3 def, Task 5 use, doc string in Task 6 ✓
- `aggregate(cfg, run)` — Task 3 def, Task 5 use ✓
- `write_per_frame_csv(path, frames)` / `write_summary_csv(path, stats)` — Task 4 def, Task 5 use ✓
