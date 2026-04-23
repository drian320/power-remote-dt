//! Plan 4 M2 transport-layer latency bench.
//!
//! Runs an in-process `InProcTransport` pair and streams synthetic video
//! frames from one side to the other at a configurable fps, optionally with
//! induced drop and added one-way latency. Frames carry a `timestamp_host_us`
//! stamped against the shared process-wide monotonic clock
//! (`prdt_protocol::now_monotonic_us`), so the viewer side can compute
//! `recv_us - host_ts_us` and emit p50/p95/p99 at exit.
//!
//! This does NOT cover NVENC encode or MF decode — those happen on GPU and
//! need a D3D11 device / real monitor. The full-pipeline M2 (NVENC + MF
//! in-process) is tracked as remaining Plan 4 work in PHASE0-STATUS.md.

use std::time::{Duration, Instant};

use bytes::Bytes;
use clap::Parser;
use prdt_protocol::{frame::Codec, now_monotonic_us, EncodedFrame};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "prdt-latency-bench",
    about = "M2 in-process transport latency bench"
)]
struct Args {
    /// Run mode. Only `in-process` is supported; the flag exists to leave
    /// room for future `lan-loopback` / `cross-machine` modes.
    #[arg(long, default_value = "in-process")]
    mode: String,

    /// Resolution for sizing the synthetic frame. WxH.
    #[arg(long, default_value = "1920x1080")]
    resolution: String,

    /// Frames per second.
    #[arg(long, default_value_t = 60u32)]
    fps: u32,

    /// How long to run.
    #[arg(long, default_value = "5s")]
    duration: humantime::Duration,

    /// Per-message drop probability in ppm.
    #[arg(long, default_value_t = 0u32)]
    drop_ppm: u32,

    /// Added one-way latency per message in milliseconds.
    #[arg(long, default_value_t = 0u64)]
    latency_ms: u64,

    /// Optional CSV file to write one `seq,host_ts_us,recv_us,lag_us` row per
    /// received frame.
    #[arg(long)]
    csv: Option<std::path::PathBuf>,
}

fn parse_res(s: &str) -> anyhow::Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("bad --resolution {s:?}, expected WIDTHxHEIGHT"))?;
    let w: u32 = w.parse()?;
    let h: u32 = h.parse()?;
    Ok((w, h))
}

fn synthetic_bytes(bytes: usize) -> Bytes {
    Bytes::from(vec![0x42u8; bytes])
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    seq: u64,
    host_ts_us: u64,
    recv_us: u64,
}

fn percentiles(lags_us: &mut [u64]) -> (u64, u64, u64, u64, u64) {
    lags_us.sort_unstable();
    let pick = |p: f64| -> u64 {
        let idx = ((lags_us.len() as f64 - 1.0) * p).round() as usize;
        lags_us[idx]
    };
    (
        pick(0.50),
        pick(0.90),
        pick(0.95),
        pick(0.99),
        *lags_us.last().unwrap_or(&0),
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.mode != "in-process" {
        anyhow::bail!("only --mode=in-process is supported at this time");
    }
    let (w, h) = parse_res(&args.resolution)?;
    let duration: Duration = args.duration.into();
    let frame_interval = Duration::from_secs_f64(1.0 / args.fps as f64);
    let target_bitrate_bps = 50_000_000u64;
    let frame_bytes = (target_bitrate_bps / 8 / args.fps as u64) as usize;
    // Keep the bench under packetize's FEC ceiling so synthetic frames don't
    // trip FrameTooLarge. 12 * 1200 = 14.4 KB/frame ≈ 6.9 Mbps @ 60fps —
    // still representative for the transport's per-packet latency, which is
    // what this bench measures.
    let frame_bytes = frame_bytes.min(12 * 1200);

    info!(
        resolution = %args.resolution,
        fps = args.fps,
        duration_ms = duration.as_millis(),
        frame_bytes,
        drop_ppm = args.drop_ppm,
        latency_ms = args.latency_ms,
        "starting M2 in-process latency bench",
    );

    let (host_side, viewer_side) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: args.drop_ppm,
        latency: if args.latency_ms > 0 {
            Some(Duration::from_millis(args.latency_ms))
        } else {
            None
        },
    });

    let deadline = Instant::now() + duration;
    let sender = tokio::spawn(async move {
        let mut seq = 0u64;
        let mut next = Instant::now();
        while Instant::now() < deadline {
            tokio::time::sleep_until(next.into()).await;
            let frame = EncodedFrame {
                seq,
                timestamp_host_us: now_monotonic_us(),
                is_keyframe: seq % 60 == 0,
                nal_units: synthetic_bytes(frame_bytes),
                width: w,
                height: h,
                codec: Codec::H265,
            };
            if host_side.send_video(frame).await.is_err() {
                break;
            }
            seq += 1;
            next += frame_interval;
        }
        seq
    });

    let mut samples: Vec<Sample> =
        Vec::with_capacity(duration.as_secs() as usize * args.fps as usize + 64);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(frame))) => {
                samples.push(Sample {
                    seq: frame.seq,
                    host_ts_us: frame.timestamp_host_us,
                    recv_us: now_monotonic_us(),
                });
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                warn!(?e, "recv error; stopping");
                break;
            }
            Err(_) => break, // 500ms idle — sender is done
        }
    }
    let sent = sender.await.unwrap_or(0);

    // Latency stats.
    if samples.is_empty() {
        info!(sent, "bench done but received 0 frames");
        return Ok(());
    }
    let received = samples.len() as u64;
    let lost = sent.saturating_sub(received);
    let loss_ppm = if sent > 0 {
        ((lost as f64 / sent as f64) * 1_000_000.0) as u64
    } else {
        0
    };

    let mut lags_us: Vec<u64> = samples
        .iter()
        .map(|s| s.recv_us.saturating_sub(s.host_ts_us))
        .collect();
    let (p50, p90, p95, p99, p100) = percentiles(&mut lags_us);
    let mean = lags_us.iter().sum::<u64>() / lags_us.len() as u64;

    info!(
        sent,
        received,
        lost,
        loss_ppm,
        lag_mean_us = mean,
        lag_p50_us = p50,
        lag_p90_us = p90,
        lag_p95_us = p95,
        lag_p99_us = p99,
        lag_max_us = p100,
        "bench done",
    );

    if let Some(path) = args.csv {
        let mut wtr = std::fs::File::create(&path)?;
        use std::io::Write;
        writeln!(wtr, "seq,host_ts_us,recv_us,lag_us")?;
        for s in &samples {
            let lag = s.recv_us.saturating_sub(s.host_ts_us);
            writeln!(wtr, "{},{},{},{}", s.seq, s.host_ts_us, s.recv_us, lag)?;
        }
        info!(path = %path.display(), "wrote CSV samples");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_monotonic() {
        let mut v: Vec<u64> = (1..=100).collect();
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert!(p50 <= p90);
        assert!(p90 <= p95);
        assert!(p95 <= p99);
        assert!(p99 <= p100);
        assert_eq!(p100, 100);
    }

    #[test]
    fn percentiles_single_sample() {
        let mut v = vec![42u64];
        let (p50, p90, p95, p99, p100) = percentiles(&mut v);
        assert_eq!((p50, p90, p95, p99, p100), (42, 42, 42, 42, 42));
    }

    #[test]
    fn parse_res_accepts_wxh() {
        let (w, h) = parse_res("1920x1080").unwrap();
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn parse_res_rejects_garbage() {
        assert!(parse_res("nope").is_err());
        assert!(parse_res("1920").is_err());
        assert!(parse_res("").is_err());
    }
}
