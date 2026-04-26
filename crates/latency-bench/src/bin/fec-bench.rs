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

/// xorshift64-based RNG. Pure function: takes a state, returns the next
/// state. Cheap, deterministic. The caller threads state through repeated
/// calls or seeds it from a per-call hash.
fn xorshift64(mut x: u64) -> u64 {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x.max(1) // never return 0 (would freeze the generator)
}

/// Roll a per-packet drop decision. Mixes seed + trial_idx + packet_idx
/// so each trial is reproducible and packet decisions within one trial
/// are independent.
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
/// deterministically by `seed + trial_idx`. The per-byte content varies
/// so packetize doesn't accidentally collapse identical shards.
fn make_frame(seed: u64, trial_idx: u64, frame_bytes: usize) -> EncodedFrame {
    let mut buf = Vec::with_capacity(frame_bytes);
    let base = seed.wrapping_add(trial_idx);
    for i in 0..frame_bytes {
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
            return TrialResult {
                outcome: TrialOutcome::Lost,
                reconstruct_us: None,
            };
        }
    };
    let frame = make_frame(seed, trial_idx, cfg.frame_bytes);
    let packets = match packetize(&frame, &fec, cfg.chunk_payload_len) {
        Ok(p) => p,
        Err(_) => {
            return TrialResult {
                outcome: TrialOutcome::Lost,
                reconstruct_us: None,
            };
        }
    };

    // Track whether any source-chunk packet (idx 0..k) was dropped. If all
    // source chunks survive, no FEC was needed even if parity packets were
    // also delivered.
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

    // Deterministic shuffle so packet order varies per trial but is
    // reproducible given the same seed/trial_idx.
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
        return TrialResult {
            outcome: TrialOutcome::Lost,
            reconstruct_us: None,
        };
    }
    if all_source_present {
        TrialResult {
            outcome: TrialOutcome::CompleteNoFec,
            reconstruct_us: None,
        }
    } else {
        TrialResult {
            outcome: TrialOutcome::CompleteWithFec,
            reconstruct_us: Some(elapsed_us),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_id_format_canonical() {
        let c = Cfg {
            k: 8,
            m: 2,
            drop_ppm: 0,
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 1000,
        };
        assert_eq!(config_id(&c), "k8m2-drop0");
        let c = Cfg {
            k: 64,
            m: 6,
            drop_ppm: 200_000,
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 1000,
        };
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

    #[test]
    fn trial_no_drop_completes_no_fec() {
        let cfg = Cfg {
            k: 4,
            m: 2,
            drop_ppm: 0,
            frame_bytes: 2000,
            chunk_payload_len: 1200,
            trials: 1,
        };
        let result = simulate_one_trial(&cfg, /*trial_idx=*/ 0, /*seed=*/ 123);
        assert_eq!(result.outcome, TrialOutcome::CompleteNoFec);
        assert_eq!(result.reconstruct_us, None);
    }

    #[test]
    fn trial_full_drop_lost() {
        let cfg = Cfg {
            k: 4,
            m: 2,
            drop_ppm: 1_000_000,
            frame_bytes: 2000,
            chunk_payload_len: 1200,
            trials: 1,
        };
        let result = simulate_one_trial(&cfg, /*trial_idx=*/ 0, /*seed=*/ 123);
        assert_eq!(result.outcome, TrialOutcome::Lost);
        assert_eq!(result.reconstruct_us, None);
    }

    #[test]
    fn trial_recoverable_drop_completes_with_fec() {
        // k=4, m=2 -> 6 packets, 20% drop rate -> usually some packets get
        // dropped. We probe seeds 1..=20 looking for one where:
        //   - At least one source-chunk packet (idx 0..k) was dropped (so
        //     FEC reconstruction is required)
        //   - Total surviving packets >= k (so FEC can succeed)
        // This proves the CompleteWithFec branch fires correctly.
        let cfg = Cfg {
            k: 4,
            m: 2,
            drop_ppm: 200_000,
            frame_bytes: 2000,
            chunk_payload_len: 1200,
            trials: 1,
        };
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

    #[test]
    fn aggregate_collects_outcomes() {
        let trials: Vec<TrialResult> = vec![
            TrialResult {
                outcome: TrialOutcome::CompleteNoFec,
                reconstruct_us: None,
            },
            TrialResult {
                outcome: TrialOutcome::CompleteNoFec,
                reconstruct_us: None,
            },
            TrialResult {
                outcome: TrialOutcome::CompleteWithFec,
                reconstruct_us: Some(20),
            },
            TrialResult {
                outcome: TrialOutcome::CompleteWithFec,
                reconstruct_us: Some(60),
            },
            TrialResult {
                outcome: TrialOutcome::Lost,
                reconstruct_us: None,
            },
        ];
        let cfg = Cfg {
            k: 8,
            m: 2,
            drop_ppm: 100_000,
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 5,
        };
        let s = aggregate(&cfg, &trials);
        assert_eq!(s.complete_no_fec, 2);
        assert_eq!(s.complete_with_fec, 2);
        assert_eq!(s.lost, 1);
        // recovery = 4/5 = 800_000 ppm
        assert_eq!(s.recovery_rate_ppm, 800_000);
        // p50 of [20, 60] under round-style picking: idx = round((2-1)*0.5) = round(0.5) = 1
        // -> v[1] = 60
        assert_eq!(s.reconstruct_p50_us, 60);
        assert_eq!(s.reconstruct_p95_us, 60);
    }

    #[test]
    fn aggregate_empty_reconstruct_emits_zeros() {
        let trials: Vec<TrialResult> = vec![
            TrialResult {
                outcome: TrialOutcome::CompleteNoFec,
                reconstruct_us: None,
            },
            TrialResult {
                outcome: TrialOutcome::CompleteNoFec,
                reconstruct_us: None,
            },
        ];
        let cfg = Cfg {
            k: 8,
            m: 2,
            drop_ppm: 0,
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 2,
        };
        let s = aggregate(&cfg, &trials);
        assert_eq!(s.recovery_rate_ppm, 1_000_000);
        assert_eq!(s.reconstruct_p50_us, 0);
        assert_eq!(s.reconstruct_p95_us, 0);
    }

    #[test]
    fn summary_csv_writer_emits_header_and_one_row() {
        let cfg = Cfg {
            k: 8,
            m: 2,
            drop_ppm: 50_000,
            frame_bytes: 5000,
            chunk_payload_len: 1200,
            trials: 100,
        };
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
        assert_eq!(
            lines[1],
            "k8m2-drop50000,8,2,50000,5000,100,90,9,1,990000,18,35"
        );
    }
}
