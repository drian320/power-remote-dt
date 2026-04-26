# Plan 4 B6 FEC Sweep Bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `prdt-fec-bench` bin to `latency-bench` that sweeps `(k, m, drop_ppm)` configurations, runs `packetize → drop → FrameAssembler` per trial, and writes a CSV summary of recovery rate and reconstruction latency.

**Architecture:** Pure-CPU bin in `crates/latency-bench/src/bin/fec-bench.rs`. Reuses public API from `prdt-transport` (`FecCodec`, `packetize`, `FrameAssembler`) and `prdt-protocol` (`EncodedFrame`, `VideoPacket`). No GPU, no async, no transport, no encryption. CLI accepts comma-separated axes with sane defaults (3 × 2 × 5 = 30 configs, ~30 s wall time).

**Tech Stack:** Rust 2021, existing `clap` derive, `tracing`, `humantime`. Inline xorshift64 RNG (no new deps).

**Spec:** `docs/superpowers/specs/2026-04-26-plan4-b6-fec-bench-design.md`

---

## File Structure

**Created files:**

```
crates/latency-bench/src/bin/
  fec-bench.rs                  CLI + matrix expand + simulate_one_trial +
                                aggregate + CSV writer + 5 unit tests
docs/
  fec-bench.md                  usage guide + sample CSV interpretation
```

**Modified files:**

```
crates/latency-bench/Cargo.toml + [[bin]] entry for prdt-fec-bench
                                + rand_core (for SeedableRng)? — actually
                                no, we roll xorshift64 inline
```

---

## API Reference (verified against current code)

These are the public symbols the bin uses. Verified from
`crates/transport/src/{fec.rs,packetize.rs,assembler.rs,lib.rs}`:

```rust
// prdt_transport::FecCodec
pub struct FecCodec { /* opaque */ }
impl FecCodec {
    pub fn new(k: usize, m: usize) -> Result<Self, TransportError>;
    pub fn k(&self) -> usize;
    pub fn m(&self) -> usize;
    pub fn reconstruct(&self, shards: Vec<Option<Vec<u8>>>) -> Result<Vec<Vec<u8>>, TransportError>;
}

// prdt_transport::packetize
pub fn packetize(
    frame: &EncodedFrame,
    fec: &FecCodec,
    chunk_payload_len: usize,
) -> Result<Vec<VideoPacket>, TransportError>;

// prdt_transport::assembler
pub struct FrameAssembler { /* opaque */ }
pub enum FeedResult { Pending, Stale, Complete(EncodedFrame) }
impl FrameAssembler {
    pub fn new(width: u32, height: u32, codec: Codec) -> Self;
    pub fn feed(&mut self, pkt: VideoPacket, fec: &FecCodec) -> Result<FeedResult, TransportError>;
}

// prdt_protocol
pub struct EncodedFrame {
    pub seq: u64,
    pub timestamp_host_us: u64,
    pub is_keyframe: bool,
    pub nal_units: bytes::Bytes,
    pub width: u32,
    pub height: u32,
    pub codec: prdt_protocol::frame::Codec,
}
pub struct VideoPacket {
    pub frame_seq: u64,
    pub timestamp_host_us: u64,
    pub chunk_idx: u16,
    pub source_chunks: u16,
    pub parity_chunks: u16,
    pub video_flags: u8,
    pub payload_bytes: u16,
    pub chunk_payload: Vec<u8>,
}
```

---

## Task 1: Bin scaffold + CLI parse + dry-run

**Files:**
- Modify: `crates/latency-bench/Cargo.toml`
- Create: `crates/latency-bench/src/bin/fec-bench.rs`

- [ ] **Step 1: Add `[[bin]]` to Cargo.toml**

In `crates/latency-bench/Cargo.toml`, after the existing `prdt-bench-matrix` `[[bin]]` block (around lines 16-18), append:

```toml

[[bin]]
name = "prdt-fec-bench"
path = "src/bin/fec-bench.rs"
```

(Blank line before, no other changes needed. Existing deps cover everything: clap, tracing, tracing-subscriber, anyhow, humantime, prdt-transport, prdt-protocol.)

Also add `prdt-transport = { path = "../transport" }` and `prdt-protocol = { path = "../protocol" }` to `[dependencies]` if NOT already present. Current `[dependencies]` block has them already from B1 — verify with `grep "prdt-transport\|prdt-protocol" crates/latency-bench/Cargo.toml`. If absent, add both.

- [ ] **Step 2: Create the bin scaffold with CLI + dry-run**

Create `crates/latency-bench/src/bin/fec-bench.rs`:

```rust
//! Plan 4 B6 FEC sweep bench. Tests the FEC algorithm directly:
//! synthetic frame -> packetize -> per-packet drop -> FrameAssembler.
//! No transport, no GPU, no async. Sweeps (k, m, drop_ppm) and writes
//! a recovery-rate + reconstruction-latency CSV.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "prdt-fec-bench",
    about = "Plan 4 B6 FEC sweep: (k x m x drop_ppm) recovery rate + reconstruction overhead"
)]
struct Args {
    /// Output dir. Will contain `summary.csv`. Created if missing;
    /// existing files are overwritten.
    #[arg(long)]
    out_dir: PathBuf,

    /// Comma-separated data shards (k).
    #[arg(long, value_delimiter = ',', default_values_t = vec![8usize, 32usize, 64usize])]
    ks: Vec<usize>,

    /// Comma-separated parity shards (m).
    #[arg(long, value_delimiter = ',', default_values_t = vec![2usize, 6usize])]
    ms: Vec<usize>,

    /// Comma-separated per-packet drop probability in ppm (0..=1_000_000).
    #[arg(long, value_delimiter = ',', default_values_t = vec![0u32, 10_000u32, 50_000u32, 100_000u32, 200_000u32])]
    drops: Vec<u32>,

    /// Synthetic frame size in bytes.
    #[arg(long, default_value_t = 5000usize)]
    frame_bytes: usize,

    /// Per-packet payload size (MTU-aware).
    #[arg(long, default_value_t = 1200usize)]
    chunk_payload_len: usize,

    /// Frames per config.
    #[arg(long, default_value_t = 1000u64)]
    trials: u64,

    /// RNG seed (any non-zero u64).
    #[arg(long, default_value_t = 4242u64)]
    seed: u64,

    /// Print the matrix and exit without running.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy)]
struct Cfg {
    k: usize,
    m: usize,
    drop_ppm: u32,
    frame_bytes: usize,
    chunk_payload_len: usize,
    trials: u64,
}

fn config_id(cfg: &Cfg) -> String {
    format!("k{}m{}-drop{}", cfg.k, cfg.m, cfg.drop_ppm)
}

fn expand_matrix(args: &Args) -> Vec<Cfg> {
    let mut out = Vec::with_capacity(args.ks.len() * args.ms.len() * args.drops.len());
    for &k in &args.ks {
        for &m in &args.ms {
            for &drop_ppm in &args.drops {
                out.push(Cfg {
                    k,
                    m,
                    drop_ppm,
                    frame_bytes: args.frame_bytes,
                    chunk_payload_len: args.chunk_payload_len,
                    trials: args.trials,
                });
            }
        }
    }
    out
}

fn main() -> anyhow::Result<()> {
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
        let c = Cfg { k: 8, m: 2, drop_ppm: 0, frame_bytes: 5000, chunk_payload_len: 1200, trials: 1000 };
        assert_eq!(config_id(&c), "k8m2-drop0");
        let c = Cfg { k: 64, m: 6, drop_ppm: 200_000, frame_bytes: 5000, chunk_payload_len: 1200, trials: 1000 };
        assert_eq!(config_id(&c), "k64m6-drop200000");
    }

    #[test]
    fn expand_matrix_cartesian() {
        let args = Args {
            out_dir: PathBuf::from("/tmp/fake"),
            ks: vec![8, 32],
            ms: vec![2, 6],
            drops: vec![0, 100_000],
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 100,
            seed: 1,
            dry_run: true,
        };
        let cfgs = expand_matrix(&args);
        assert_eq!(cfgs.len(), 8); // 2 * 2 * 2
        // outer-to-inner: k -> m -> drop
        assert_eq!((cfgs[0].k, cfgs[0].m, cfgs[0].drop_ppm), (8, 2, 0));
        assert_eq!((cfgs[1].k, cfgs[1].m, cfgs[1].drop_ppm), (8, 2, 100_000));
        assert_eq!((cfgs[2].k, cfgs[2].m, cfgs[2].drop_ppm), (8, 6, 0));
        assert_eq!((cfgs[4].k, cfgs[4].m, cfgs[4].drop_ppm), (32, 2, 0));
    }
}
```

- [ ] **Step 3: Build + dry-run smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -3
cargo run -p prdt-latency-bench --bin prdt-fec-bench -- --out-dir /tmp/dry --dry-run 2>/dev/null | wc -l
```

Expected: clean build; dry-run prints exactly `30` lines (3 ks × 2 ms × 5 drops).

- [ ] **Step 4: Run unit tests**

```bash
cargo test -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -10
```

Expected: 2 tests pass (`config_id_format_canonical`, `expand_matrix_cartesian`).

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/Cargo.toml \
        crates/latency-bench/src/bin/fec-bench.rs
git commit -m "fec-bench: scaffold bin with CLI + dry-run + matrix expansion"
```

---

## Task 2: `simulate_one_trial` with TDD

**Files:**
- Modify: `crates/latency-bench/src/bin/fec-bench.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/latency-bench/src/bin/fec-bench.rs`:

```rust
    #[test]
    fn trial_no_drop_completes_no_fec() {
        let cfg = Cfg { k: 4, m: 2, drop_ppm: 0, frame_bytes: 2000, chunk_payload_len: 1200, trials: 1 };
        let result = simulate_one_trial(&cfg, /*trial_idx=*/0, /*seed=*/123);
        assert_eq!(result.outcome, TrialOutcome::CompleteNoFec);
        assert_eq!(result.reconstruct_us, None);
    }

    #[test]
    fn trial_full_drop_lost() {
        let cfg = Cfg { k: 4, m: 2, drop_ppm: 1_000_000, frame_bytes: 2000, chunk_payload_len: 1200, trials: 1 };
        let result = simulate_one_trial(&cfg, /*trial_idx=*/0, /*seed=*/123);
        assert_eq!(result.outcome, TrialOutcome::Lost);
        assert_eq!(result.reconstruct_us, None);
    }

    #[test]
    fn trial_recoverable_drop_completes_with_fec() {
        // k=4, m=2 -> 6 packets, drop 1 source chunk -> 5/6 survive,
        // FEC must reconstruct. The seed below was picked so that with
        // drop_ppm=200_000 (20%), exactly 1 source chunk is dropped on
        // trial 0 — verified by running the test and observing.
        let cfg = Cfg { k: 4, m: 2, drop_ppm: 200_000, frame_bytes: 2000, chunk_payload_len: 1200, trials: 1 };
        // Try a few seeds to find one that triggers FEC reliably for this test.
        // Loop until we find a CompleteWithFec outcome (sanity: with 20% drop
        // of 6 packets, the chance of 1+ drops is ~74%; the chance that
        // exactly one of those drops hits a SOURCE chunk specifically is
        // dominant; so most seeds yield CompleteWithFec).
        let mut found = false;
        for seed in 1..=20u64 {
            let r = simulate_one_trial(&cfg, 0, seed);
            if r.outcome == TrialOutcome::CompleteWithFec && r.reconstruct_us.is_some() {
                found = true;
                break;
            }
        }
        assert!(found, "expected at least one seed in 1..=20 to trigger FEC");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -15
```

Expected: compile errors — `cannot find function simulate_one_trial`, `cannot find type TrialOutcome` etc.

- [ ] **Step 3: Implement `simulate_one_trial`**

In `crates/latency-bench/src/bin/fec-bench.rs`, between the `expand_matrix` function and the `main` function, add:

```rust
use std::time::Instant;

use bytes::Bytes;
use prdt_protocol::{frame::Codec, EncodedFrame};
use prdt_transport::{
    assembler::{FeedResult, FrameAssembler},
    packetize::packetize,
    FecCodec,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrialOutcome {
    CompleteNoFec,
    CompleteWithFec,
    Lost,
}

#[derive(Debug, Clone, Copy)]
struct TrialResult {
    outcome: TrialOutcome,
    reconstruct_us: Option<u64>,
}

/// xorshift64-based RNG. Pure function of (seed, salt). Cheap, deterministic.
fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x.max(1) // never return 0 (would freeze the generator)
}

/// Roll a per-packet drop decision. Mixes seed + trial_idx + packet_idx
/// so each trial is reproducible.
fn should_drop(drop_ppm: u32, seed: u64, trial_idx: u64, packet_idx: u16) -> bool {
    if drop_ppm == 0 {
        return false;
    }
    if drop_ppm >= 1_000_000 {
        return true;
    }
    let mut x = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(trial_idx)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(packet_idx as u64);
    x = xorshift64(x);
    let r = (x % 1_000_000) as u32;
    r < drop_ppm
}

/// Build a synthetic frame of `frame_bytes` bytes, content varied
/// deterministically by `seed + trial_idx`.
fn make_frame(seed: u64, trial_idx: u64, frame_bytes: usize) -> EncodedFrame {
    let mut buf = Vec::with_capacity(frame_bytes);
    let base = seed.wrapping_add(trial_idx);
    for i in 0..frame_bytes {
        // Per-byte content varies, shards never collapse to all-zero.
        buf.push(((base.rotate_left(i as u32 % 64)) ^ (i as u64)) as u8);
    }
    EncodedFrame {
        seq: trial_idx + 1,
        timestamp_host_us: 0,
        is_keyframe: true,
        nal_units: Bytes::from(buf),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    }
}

/// Run one packetize -> drop -> assemble cycle.
fn simulate_one_trial(cfg: &Cfg, trial_idx: u64, seed: u64) -> TrialResult {
    let fec = match FecCodec::new(cfg.k, cfg.m) {
        Ok(f) => f,
        Err(_) => {
            return TrialResult { outcome: TrialOutcome::Lost, reconstruct_us: None };
        }
    };
    let frame = make_frame(seed, trial_idx, cfg.frame_bytes);
    let packets = match packetize(&frame, &fec, cfg.chunk_payload_len) {
        Ok(p) => p,
        Err(_) => {
            return TrialResult { outcome: TrialOutcome::Lost, reconstruct_us: None };
        }
    };

    // Track which source-chunk indices (0..k) survive. If all survive, no
    // FEC was needed even if assembler accepted parity packets too.
    let mut all_source_present = true;
    let mut surviving: Vec<_> = packets
        .iter()
        .enumerate()
        .filter_map(|(i, pkt)| {
            if should_drop(cfg.drop_ppm, seed, trial_idx, i as u16) {
                if pkt.chunk_idx < cfg.k as u16 {
                    all_source_present = false;
                }
                None
            } else {
                Some(pkt.clone())
            }
        })
        .collect();
    // Feed all surviving packets — order doesn't matter for the assembler,
    // but feeding in randomized index order is closer to real network
    // behaviour. Use a deterministic shuffle keyed on the seed.
    let mut shuffle_seed = xorshift64(seed.wrapping_add(trial_idx));
    for i in (1..surviving.len()).rev() {
        shuffle_seed = xorshift64(shuffle_seed);
        let j = (shuffle_seed as usize) % (i + 1);
        surviving.swap(i, j);
    }

    let mut asm = FrameAssembler::new(frame.width, frame.height, frame.codec);
    let start = Instant::now();
    let mut completed = false;
    for pkt in surviving {
        match asm.feed(pkt, &fec) {
            Ok(FeedResult::Complete(_)) => {
                completed = true;
                break;
            }
            Ok(FeedResult::Pending) | Ok(FeedResult::Stale) => {}
            Err(_) => break, // FEC reconstruction error -> lost
        }
    }
    let elapsed_us = start.elapsed().as_micros() as u64;

    if !completed {
        return TrialResult { outcome: TrialOutcome::Lost, reconstruct_us: None };
    }
    if all_source_present {
        TrialResult { outcome: TrialOutcome::CompleteNoFec, reconstruct_us: None }
    } else {
        TrialResult { outcome: TrialOutcome::CompleteWithFec, reconstruct_us: Some(elapsed_us) }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -10
```

Expected: 5 tests pass (`config_id_format_canonical`, `expand_matrix_cartesian`, `trial_no_drop_completes_no_fec`, `trial_full_drop_lost`, `trial_recoverable_drop_completes_with_fec`).

```bash
cargo clippy -p prdt-latency-bench --bin prdt-fec-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/fec-bench.rs
git commit -m "fec-bench: implement simulate_one_trial with TDD (no_fec / with_fec / lost)"
```

---

## Task 3: Aggregation + CSV writer

**Files:**
- Modify: `crates/latency-bench/src/bin/fec-bench.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn aggregate_collects_outcomes() {
        let trials: Vec<TrialResult> = vec![
            TrialResult { outcome: TrialOutcome::CompleteNoFec, reconstruct_us: None },
            TrialResult { outcome: TrialOutcome::CompleteNoFec, reconstruct_us: None },
            TrialResult { outcome: TrialOutcome::CompleteWithFec, reconstruct_us: Some(20) },
            TrialResult { outcome: TrialOutcome::CompleteWithFec, reconstruct_us: Some(60) },
            TrialResult { outcome: TrialOutcome::Lost, reconstruct_us: None },
        ];
        let cfg = Cfg { k: 8, m: 2, drop_ppm: 100_000, frame_bytes: 5000, chunk_payload_len: 1200, trials: 5 };
        let s = aggregate(&cfg, &trials);
        assert_eq!(s.complete_no_fec, 2);
        assert_eq!(s.complete_with_fec, 2);
        assert_eq!(s.lost, 1);
        // recovery = 4/5 = 800_000 ppm
        assert_eq!(s.recovery_rate_ppm, 800_000);
        // p50 of [20, 60] is 60 (round percentile picks idx 1)
        assert_eq!(s.reconstruct_p50_us, 60);
        assert_eq!(s.reconstruct_p95_us, 60);
    }

    #[test]
    fn aggregate_empty_reconstruct_emits_zeros() {
        let trials: Vec<TrialResult> = vec![
            TrialResult { outcome: TrialOutcome::CompleteNoFec, reconstruct_us: None },
            TrialResult { outcome: TrialOutcome::CompleteNoFec, reconstruct_us: None },
        ];
        let cfg = Cfg { k: 8, m: 2, drop_ppm: 0, frame_bytes: 5000, chunk_payload_len: 1200, trials: 2 };
        let s = aggregate(&cfg, &trials);
        assert_eq!(s.recovery_rate_ppm, 1_000_000);
        assert_eq!(s.reconstruct_p50_us, 0);
        assert_eq!(s.reconstruct_p95_us, 0);
    }

    #[test]
    fn summary_csv_writer_emits_header_and_one_row() {
        let cfg = Cfg { k: 8, m: 2, drop_ppm: 50_000, frame_bytes: 5000, chunk_payload_len: 1200, trials: 100 };
        let s = ConfigStats {
            config_id: config_id(&cfg),
            cfg,
            complete_no_fec: 90,
            complete_with_fec: 9,
            lost: 1,
            recovery_rate_ppm: 990_000,
            reconstruct_p50_us: 18,
            reconstruct_p95_us: 35,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summary.csv");
        write_summary_csv(&path, std::slice::from_ref(&s)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row");
        assert_eq!(
            lines[0],
            "config_id,k,m,drop_ppm,frame_bytes,trials,complete_no_fec,complete_with_fec,lost,recovery_rate_ppm,reconstruct_p50_us,reconstruct_p95_us"
        );
        assert_eq!(lines[1], "k8m2-drop50000,8,2,50000,5000,100,90,9,1,990000,18,35");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -10
```

Expected: compile errors — `cannot find function aggregate`, `cannot find type ConfigStats`, `cannot find function write_summary_csv`.

- [ ] **Step 3: Implement aggregation + CSV writer**

In `crates/latency-bench/src/bin/fec-bench.rs`, between the `simulate_one_trial` function and `main`, add:

```rust
struct ConfigStats {
    config_id: String,
    cfg: Cfg,
    complete_no_fec: u64,
    complete_with_fec: u64,
    lost: u64,
    recovery_rate_ppm: u64,
    reconstruct_p50_us: u64,
    reconstruct_p95_us: u64,
}

fn aggregate(cfg: &Cfg, trials: &[TrialResult]) -> ConfigStats {
    let mut complete_no_fec = 0u64;
    let mut complete_with_fec = 0u64;
    let mut lost = 0u64;
    let mut reconstructs: Vec<u64> = Vec::new();
    for t in trials {
        match t.outcome {
            TrialOutcome::CompleteNoFec => complete_no_fec += 1,
            TrialOutcome::CompleteWithFec => {
                complete_with_fec += 1;
                if let Some(us) = t.reconstruct_us {
                    reconstructs.push(us);
                }
            }
            TrialOutcome::Lost => lost += 1,
        }
    }
    let total = (complete_no_fec + complete_with_fec + lost).max(1);
    let recovery_rate_ppm = (complete_no_fec + complete_with_fec) * 1_000_000 / total;
    let (reconstruct_p50_us, reconstruct_p95_us) = if reconstructs.is_empty() {
        (0, 0)
    } else {
        let (p50, _, p95, _, _) = prdt_latency_bench::percentiles(&mut reconstructs);
        (p50, p95)
    };
    ConfigStats {
        config_id: config_id(cfg),
        cfg: *cfg,
        complete_no_fec,
        complete_with_fec,
        lost,
        recovery_rate_ppm,
        reconstruct_p50_us,
        reconstruct_p95_us,
    }
}

fn write_summary_csv(path: &std::path::Path, stats: &[ConfigStats]) -> std::io::Result<()> {
    use std::io::Write;
    let mut wtr = std::fs::File::create(path)?;
    writeln!(
        wtr,
        "config_id,k,m,drop_ppm,frame_bytes,trials,complete_no_fec,complete_with_fec,lost,recovery_rate_ppm,reconstruct_p50_us,reconstruct_p95_us"
    )?;
    for s in stats {
        writeln!(
            wtr,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            s.config_id,
            s.cfg.k,
            s.cfg.m,
            s.cfg.drop_ppm,
            s.cfg.frame_bytes,
            s.cfg.trials,
            s.complete_no_fec,
            s.complete_with_fec,
            s.lost,
            s.recovery_rate_ppm,
            s.reconstruct_p50_us,
            s.reconstruct_p95_us
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p prdt-latency-bench --bin prdt-fec-bench 2>&1 | tail -15
```

Expected: 8 tests pass (5 prior + 3 new: `aggregate_collects_outcomes`, `aggregate_empty_reconstruct_emits_zeros`, `summary_csv_writer_emits_header_and_one_row`).

```bash
cargo clippy -p prdt-latency-bench --bin prdt-fec-bench --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/latency-bench/src/bin/fec-bench.rs
git commit -m "fec-bench: add ConfigStats + aggregate + write_summary_csv"
```

---

## Task 4: Wire main loop + smoke + docs + tag

**Files:**
- Modify: `crates/latency-bench/src/bin/fec-bench.rs` (replace `bail!` with real loop)
- Create: `docs/fec-bench.md`

- [ ] **Step 1: Replace the bail!() with the real trial loop**

In `crates/latency-bench/src/bin/fec-bench.rs`, find:

```rust
    // Trial loop + summary CSV come in Tasks 2-4.
    anyhow::bail!("trial loop not yet implemented (Tasks 2-4)");
}
```

Replace with:

```rust
    let mut all_stats: Vec<ConfigStats> = Vec::with_capacity(configs.len());
    for (i, cfg) in configs.iter().enumerate() {
        let id = config_id(cfg);
        info!(
            "[{:>3}/{}] running {} trials={}",
            i + 1,
            configs.len(),
            id,
            cfg.trials
        );
        let mut trials = Vec::with_capacity(cfg.trials as usize);
        for trial_idx in 0..cfg.trials {
            trials.push(simulate_one_trial(cfg, trial_idx, args.seed));
        }
        let stats = aggregate(cfg, &trials);
        info!(
            "[{:>3}/{}] done    {} recovery={}ppm reconstruct_p50_us={}",
            i + 1,
            configs.len(),
            id,
            stats.recovery_rate_ppm,
            stats.reconstruct_p50_us
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

- [ ] **Step 2: Build + run a tiny smoke**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec-smoke/ \
    --ks 8 --ms 2 --drops 0,100000 --trials 100 2>&1 | tail -10
cat bench-results/fec-smoke/summary.csv
```

Expected:
- 2-row CSV (drop=0 and drop=100000) plus header
- drop=0 row: `recovery_rate_ppm = 1000000`
- drop=100000 row: `recovery_rate_ppm < 1000000` but > 800000 (since k=8, m=2 can absorb 2 drops out of 10 packets, ~20% drop covers 1-2 drops most of the time)

- [ ] **Step 3: Run the default 30-config sweep**

```bash
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec-default/ 2>&1 | tail -20
wc -l bench-results/fec-default/summary.csv
```

Expected: 31 lines (header + 30 configs). All recovery_rate_ppm values look reasonable:
- (k=8, m=2, drop=0): 1000000
- (k=8, m=2, drop=200000): expect significant losses (1-2 drops out of 10 packets is borderline; 3+ drops is unrecoverable)
- (k=64, m=6): higher drop tolerance per parity ratio
- (k=*, m=*, drop=0): always 1000000

If recovery rates look unreasonable (e.g. all 0), revisit the `should_drop` RNG mixing or the source-chunk-tracking logic.

- [ ] **Step 4: Run full workspace tests + clippy**

```bash
cargo test -p prdt-latency-bench 2>&1 | grep "test result" | tail -10
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: latency-bench tests show 8+ for the bin (5 from Task 2 + 3 from Task 3); existing matrix tests still pass; clippy clean.

- [ ] **Step 5: Create docs/fec-bench.md**

Create `docs/fec-bench.md`:

```markdown
# FEC sweep bench (Plan 4 B6)

The `prdt-fec-bench` bin tests the Reed-Solomon FEC algorithm
directly: synthetic frame -> packetize -> per-packet drop ->
FrameAssembler. No transport, no GPU, no async. Sweeps
`(k × m × drop_ppm)` and writes a recovery-rate + reconstruction-
latency CSV.

## Quick start

```bash
# Default 30-config sweep, ~30 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec/

# Custom subset (only k=64, sweep drop rates).
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec-k64/ \
    --ks 64 --ms 2,6 --drops 0,50000,100000,200000,300000

# Dry-run (list configs).
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--ks <list>` | `8,32,64` | Data shards per frame. |
| `--ms <list>` | `2,6` | Parity shards per frame. |
| `--drops <ppm>` | `0,10000,50000,100000,200000` | Per-packet drop probability in ppm. |
| `--frame-bytes <N>` | `5000` | Synthetic frame size; must fit in `k * chunk_payload_len`. |
| `--chunk-payload-len <N>` | `1200` | Per-packet payload size (MTU-aware). |
| `--trials <N>` | `1000` | Frames per config. |
| `--seed <u64>` | `4242` | RNG seed (any non-zero). |
| `--dry-run` | off | List configs and exit. |

## summary.csv schema

```
config_id,k,m,drop_ppm,frame_bytes,trials,complete_no_fec,complete_with_fec,lost,recovery_rate_ppm,reconstruct_p50_us,reconstruct_p95_us
```

`config_id` format: `k{K}m{M}-drop{ppm}` (e.g. `k8m2-drop50000`).

- `complete_no_fec`: trials where all source shards arrived; FEC was
  not exercised even if parity packets were also delivered.
- `complete_with_fec`: trials where at least one source shard was
  dropped, but enough total shards (>= k) arrived; FEC reconstructed.
- `lost`: trials where fewer than k shards arrived; unrecoverable.
- `recovery_rate_ppm`: `(complete_no_fec + complete_with_fec) /
  trials` in ppm.
- `reconstruct_p50_us` / `_p95_us`: reconstruction latency over
  `complete_with_fec` trials only. Zero when no trials triggered FEC.

## Sample interpretation

```
k8m2-drop100000,8,2,100000,5000,1000,432,521,47,953000,18,42
```

means: 1000 trials at k=8 m=2 with 10% per-packet drop. 432 trials
arrived clean, 521 needed FEC reconstruction, 47 lost. Overall
953,000 ppm = 95.3% recovery rate. Median FEC reconstruction took
18 µs, p95 was 42 µs.

## Limitations

- **Independent random drop only**: no bursty patterns, no targeted
  attack on parity shards.
- **Uniform frame size**: `--frame-bytes` is fixed per run; codec
  output in real life is bursty (IDR vs P-frame).
- **No latency bench**: the bin measures FEC algorithm overhead,
  not transport latency under FEC. A future bench using
  `CustomUdpTransport` with packet-level drop injection would
  cover that case.
- **Frame-bytes must fit in `k * chunk_payload_len`**: with the
  defaults, the largest frame is `8 * 1200 = 9600` bytes for
  k=8 configs. A frame larger than that would error in `packetize`
  and be reported as `lost`.
```

- [ ] **Step 6: Commit docs + tag**

```bash
git add crates/latency-bench/src/bin/fec-bench.rs \
        docs/fec-bench.md
git commit -m "fec-bench: wire main loop + smoke + docs"
```

```bash
git tag -a plan4-b6-fec-bench-complete -m "$(cat <<'EOF'
Plan 4 B6 FEC sweep bench complete

Adds prdt-fec-bench bin to latency-bench crate. Tests the FEC
algorithm directly (packetize -> per-packet drop -> FrameAssembler)
without GPU / transport / async. Default sweep: 3 ks x 2 ms x 5
drop_ppm = 30 configs at 1000 trials each, ~30 s wall time.

- Cfg + expand_matrix (cartesian product, k outer / drop inner)
- simulate_one_trial: classify outcome as CompleteNoFec /
  CompleteWithFec / Lost; track which source-chunk indices survived
- aggregate: recovery_rate_ppm + reconstruct p50/p95 (FEC only)
- write_summary_csv: 12-column header per spec
- 8 unit tests (config_id, expand, 3 trial outcomes, 2 aggregate, csv)
- xorshift64 RNG inline (no new deps)
- docs/fec-bench.md with usage + schema + sample interpretation

Out of scope: bursty drop patterns, targeted parity attacks,
transport-level latency under FEC (future B-class bench), variable
per-frame size, codec-aware frame sizing.
EOF
)"
git tag | grep plan4-b
```

Expected: `plan4-b1-bench-matrix-complete` and `plan4-b6-fec-bench-complete` listed.

- [ ] **Step 7: Final summary report**

Report back:

- Files added: `crates/latency-bench/src/bin/fec-bench.rs`, `docs/fec-bench.md`
- File modified: `crates/latency-bench/Cargo.toml` (+ `[[bin]]` entry)
- Workspace test count + delta from current
- Clippy result
- Tag listing
- Sample summary.csv top rows
- Manual smoke status: dry-run + tiny smoke + 30-config default sweep all confirmed

---

## Risks & Notes for Implementer

- **`prdt_latency_bench::percentiles` import**: the bin needs `use prdt_latency_bench::percentiles;` if not already imported. The function is the public API from B1's lib.rs.
- **`prdt_protocol::frame::Codec`**: `Codec::H265` is the variant. Verify with `grep "Codec::" crates/protocol/src/frame.rs` if unsure.
- **`bytes::Bytes::from(Vec<u8>)`**: trivially `from`-converts.
- **`humantime::Duration` is NOT used** in this bin; only `--duration` is (string parsing not needed).
- **Wait — the `--duration` flag is NOT in this bin's CLI**: trials count is `--trials`, time-bound is implicit (1000 trials × ~30 µs ≈ 30 ms per config). Don't add a `--duration` axis.
- **`#![cfg(windows)]`**: NOT needed. The fec-bench has no NVENC/D3D11/Windows-specific deps. The bin should build and run on Linux too. Drop the Windows gate (unlike B1's bench-matrix which IS Windows-only).
- **`tempfile` dev-dep**: already present (added in B1 Task 1).
- **No `[[bin]]` ordering issue**: Cargo.toml allows multiple `[[bin]]` blocks in any order.

---

## Self-Review

**Spec coverage:**
- §Architecture (separate bin, pure CPU) → Task 1 ✓
- §CLI 9 flags → Task 1 Args struct ✓
- §Trial loop (packetize, drop, feed, classify) → Task 2 simulate_one_trial ✓
- §FEC trigger detection (track source-chunk survival) → Task 2 `all_source_present` ✓
- §Aggregation (counters + percentiles) → Task 3 aggregate ✓
- §Output summary.csv with 12 columns → Task 3 write_summary_csv ✓
- §Tests (5 unit) → Tasks 1+2+3 totals 8 unit tests (more is fine) ✓
- §Error handling (FecCodec::new fail, packetize fail) → Task 2 simulate_one_trial returns Lost ✓
- §Progress logging → Task 4 main loop info!() ✓
- §Risk: frame_bytes vs k → defaults adjusted to 5000 ✓
- §Reproducibility: seeded RNG → Task 2 xorshift64 ✓
- §Exit criteria 6 items → Tasks 1-4 cover all (build, test, clippy, smoke, docs, tag) ✓

**Placeholder scan:** No "TBD", "implement later". The Task 1 main() ends with `bail!("trial loop not yet implemented (Tasks 2-4)")` which is intentional — Task 4 Step 1 explicitly replaces it.

**Type consistency:**
- `Cfg { k, m, drop_ppm, frame_bytes, chunk_payload_len, trials }` — Task 1 def, Tasks 2/3 use ✓
- `TrialOutcome { CompleteNoFec, CompleteWithFec, Lost }` — Task 2 def, Task 3 use ✓
- `TrialResult { outcome, reconstruct_us }` — Task 2 def, Task 3 use ✓
- `ConfigStats { config_id, cfg, complete_no_fec, complete_with_fec, lost, recovery_rate_ppm, reconstruct_p50_us, reconstruct_p95_us }` — Task 3 def, Task 4 use ✓
- `simulate_one_trial(cfg: &Cfg, trial_idx: u64, seed: u64) -> TrialResult` — Task 2 def, Tasks 2/4 use ✓
- `aggregate(cfg: &Cfg, trials: &[TrialResult]) -> ConfigStats` — Task 3 def, Task 4 use ✓
- `write_summary_csv(path: &Path, stats: &[ConfigStats]) -> io::Result<()>` — Task 3 def, Task 4 use ✓
