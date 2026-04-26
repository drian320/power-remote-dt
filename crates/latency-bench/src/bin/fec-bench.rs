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
#[allow(dead_code)] // frame_bytes / chunk_payload_len / trials used in Tasks 2-4
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
