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
    lags: Vec<u64>,
}

#[allow(dead_code)] // used in Task 4 main loop
async fn run_one_config(cfg: &Cfg) -> RunStats {
    let (host_side, viewer_side) =
        InProcTransport::pair(LoopbackOptions::default());
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
            let drain_until = tokio::time::Instant::now()
                + Duration::from_millis(50);
            loop {
                match tokio::time::timeout_at(
                    drain_until,
                    host_side.recv(),
                )
                .await
                {
                    Ok(Ok(ReceivedMessage::Input(_))) => {
                        if let Ok(sent_us) = sent_ts_rx.try_recv() {
                            let lag = now_monotonic_us().saturating_sub(sent_us);
                            lags.push(lag);
                        }
                        received += 1;
                    }
                    Ok(Ok(_)) => {} // discard non-input
                    _ => break,    // timeout or channel closed
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
        assert!(stats.input_sent >= 10, "expected ~20 events, got {}", stats.input_sent);
        assert!(stats.input_sent <= 30);
        assert_eq!(stats.input_received, stats.input_sent, "no drops at default LoopbackOptions");
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
        assert_eq!(stats.input_received, stats.input_sent, "no drops at default LoopbackOptions");
    }
}
