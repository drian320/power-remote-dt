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
    input_lags: Vec<u64>,
    video_sent: u64,
    video_received: u64,
}

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
                let ev = InputEvent::MouseMove {
                    x: 0,
                    y: 0,
                    absolute: false,
                };
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
            // Phase 1: until cancel
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
            // Phase 2: drain in-flight (50 ms cap)
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

    // Wait, then cancel
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

fn aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats {
    let (input_p50_us, input_p95_us, input_p99_us) = if stats.input_lags.is_empty() {
        (0, 0, 0)
    } else {
        let mut lags = stats.input_lags.clone();
        let (p50, _, p95, p99, _) = prdt_latency_bench::percentiles(&mut lags);
        (p50, p95, p99)
    };
    let input_loss_ppm = if stats.input_sent > 0 {
        stats.input_sent.saturating_sub(stats.input_received) * 1_000_000 / stats.input_sent
    } else {
        0
    };
    let video_loss_ppm = if stats.video_sent > 0 {
        stats.video_sent.saturating_sub(stats.video_received) * 1_000_000 / stats.video_sent
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

fn write_summary_csv(path: &std::path::Path, stats: &[ConfigStats]) -> std::io::Result<()> {
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
        assert!(
            stats.input_sent >= 100,
            "expected ~200, got {}",
            stats.input_sent
        );
        assert_eq!(
            stats.input_received, stats.input_sent,
            "no drops at drop_ppm=0"
        );
        assert!(stats.video_sent >= 5);
        assert_eq!(
            stats.video_received, stats.video_sent,
            "no drops at drop_ppm=0"
        );
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
            input_lags: (50_000..=50_049u64).collect(),
            video_sent: 300,
            video_received: 297,
        };
        let s = aggregate(&cfg, &stats);
        assert_eq!(s.config_id, "lat50ms-drop10000ppm");
        assert_eq!(s.input_sent, 5000);
        assert_eq!(s.input_received, 4950);
        assert_eq!(s.input_loss_ppm, 10_000);
        assert_eq!(s.video_sent, 300);
        assert_eq!(s.video_received, 297);
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
}
