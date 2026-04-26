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
#[allow(dead_code)] // used in Task 3
struct RunStats {
    input_sent: u64,
    input_received: u64,
    input_lags: Vec<u64>,
    video_sent: u64,
    video_received: u64,
}

#[allow(dead_code)] // wired in Task 4
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
        assert!(stats.input_sent >= 100, "expected ~200, got {}", stats.input_sent);
        assert_eq!(stats.input_received, stats.input_sent, "no drops at drop_ppm=0");
        assert!(stats.video_sent >= 5);
        assert_eq!(stats.video_received, stats.video_sent, "no drops at drop_ppm=0");
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
}
