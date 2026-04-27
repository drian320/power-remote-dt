//! Plan 4 B1 bench matrix bin. Sweeps the cartesian product of
//! resolutions × bitrates × decoders × fps and writes per-frame raw
//! CSVs + a summary CSV.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use prdt_latency_bench::{
    aggregate, config_id, expand_matrix, full_pipeline, write_per_frame_csv, write_summary_csv,
    ConfigStats, ConsumerBackend, EncoderBackend, MatrixAxes,
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

    /// Comma-separated encoders. Choices: nvenc, mf.
    #[arg(long, value_delimiter = ',', default_values_t = vec!["nvenc".to_string()])]
    encoders: Vec<String>,

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
            "openh264" => Ok(ConsumerBackend::Openh264),
            other => Err(anyhow::anyhow!(
                "unknown decoder {other:?} (options: mf, nvdec, openh264)"
            )),
        })
        .collect()
}

fn parse_encoders(strs: &[String]) -> anyhow::Result<Vec<EncoderBackend>> {
    strs.iter()
        .map(|s| match s.as_str() {
            "nvenc" => Ok(EncoderBackend::Nvenc),
            "mf" => Ok(EncoderBackend::Mf),
            "openh264" => Ok(EncoderBackend::Openh264),
            other => Err(anyhow::anyhow!(
                "unknown encoder {other:?} (options: nvenc, mf, openh264)"
            )),
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
    let encoders = parse_encoders(&args.encoders)?;
    let resolutions = heights_to_resolutions(&args.resolutions);
    let axes = MatrixAxes {
        resolutions,
        bitrates_mbps: args.bitrates,
        decoders,
        encoders,
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
                c.bitrate_bps / 1_000_000,
                c.consumer,
                c.encoder,
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
    // Brief pause between configs so the previous config's NVENC/NVDEC/CUDA
    // context teardown has time to fully release GPU resources before the
    // next config rebuilds them. Without this, sporadic configs come up
    // with sent=1 received=0 because the next encoder steals state from
    // the still-tearing-down previous one. Empirically 250ms is sufficient
    // on RTX 3070 Ti (200ms was marginal in one observed run).
    const INTER_CONFIG_DELAY: Duration = Duration::from_millis(250);
    for (i, cfg) in configs.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(INTER_CONFIG_DELAY).await;
        }
        let id = config_id(
            (cfg.width, cfg.height),
            cfg.fps,
            cfg.bitrate_bps / 1_000_000,
            cfg.consumer,
            cfg.encoder,
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
