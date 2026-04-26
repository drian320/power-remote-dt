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
