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
