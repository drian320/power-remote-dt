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
    let (host_side, viewer_side) = InProcTransport::pair(LoopbackOptions::default());
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
                    is_keyframe: seq.is_multiple_of(30),
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
    // Two-phase: normal receive loop until cancel fires, then a brief drain
    // to collect any events already buffered in the channel at cancel time.
    let recv_task = {
        let host_side = Arc::clone(&host_side);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut received: u64 = 0;
            let mut lags: Vec<u64> = Vec::new();

            // Phase 1: receive until cancelled.
            loop {
                tokio::select! {
                    biased;
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

            // Phase 2: drain any messages already buffered in the channel
            // (sender may have enqueued one more event just before cancel).
            let drain_until = tokio::time::Instant::now() + Duration::from_millis(50);
            loop {
                match tokio::time::timeout_at(drain_until, host_side.recv()).await {
                    Ok(Ok(ReceivedMessage::Input(_))) => {
                        if let Ok(sent_us) = sent_ts_rx.try_recv() {
                            let lag = now_monotonic_us().saturating_sub(sent_us);
                            lags.push(lag);
                        }
                        received += 1;
                    }
                    Ok(Ok(_)) => {} // discard non-input
                    _ => break,     // timeout or channel closed
                }
            }

            (received, lags)
        })
    };

    // ---- Wait for the configured duration, then cancel ----
    tokio::time::sleep(cfg.duration).await;
    cancel.cancel();

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

#[allow(clippy::manual_checked_ops)] // pre-existing on master, surfaced by L1.5a Linux compilation
fn aggregate(cfg: &Cfg, stats: &RunStats) -> ConfigStats {
    let (input_p50_us, input_p95_us, input_p99_us) = if stats.lags.is_empty() {
        (0, 0, 0)
    } else {
        let mut lags = stats.lags.clone();
        let (p50, _, p95, p99, _) = prdt_latency_bench::percentiles(&mut lags);
        (p50, p95, p99)
    };
    let input_loss_ppm = if stats.input_sent > 0 {
        (stats.input_sent.saturating_sub(stats.input_received)) * 1_000_000 / stats.input_sent
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

fn write_summary_csv(path: &std::path::Path, stats: &[ConfigStats]) -> std::io::Result<()> {
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
        assert!(
            stats.input_sent >= 10,
            "expected ~20 events, got {}",
            stats.input_sent
        );
        assert!(stats.input_sent <= 30);
        assert_eq!(
            stats.input_received, stats.input_sent,
            "no drops at default LoopbackOptions"
        );
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
        assert_eq!(
            stats.input_received, stats.input_sent,
            "no drops at default LoopbackOptions"
        );
    }

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
}
