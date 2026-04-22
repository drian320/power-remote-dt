//! Phase 0 Plan 1: skeleton only. The full M2 harness lands in Plan 4,
//! but we lay out the CLI and the in-process test loop here so the
//! transport layer exercise path exists.

use std::time::{Duration, Instant};

use bytes::Bytes;
use clap::Parser;
use prdt_protocol::{frame::Codec, EncodedFrame};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "prdt-latency-bench")]
struct Args {
    /// Mode: only `in-process` for Phase 0 Plan 1 (loopback via InProcTransport).
    #[arg(long, default_value = "in-process")]
    mode: String,

    /// Resolution (for sizing the synthetic frame). WxH.
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

    /// Added latency per message in milliseconds.
    #[arg(long, default_value_t = 0u64)]
    latency_ms: u64,
}

fn parse_res(s: &str) -> (u32, u32) {
    let (w, h) = s.split_once('x').unwrap_or(("1920", "1080"));
    (w.parse().unwrap_or(1920), h.parse().unwrap_or(1080))
}

fn synthetic_bytes(bytes: usize) -> Bytes {
    Bytes::from(vec![0x42u8; bytes])
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.mode != "in-process" {
        anyhow::bail!("Phase 0 Plan 1 only supports --mode=in-process");
    }
    let (w, h) = parse_res(&args.resolution);
    let duration: Duration = args.duration.into();
    let frame_interval = Duration::from_secs_f64(1.0 / args.fps as f64);
    // Approx bitrate ~50 Mbps at 4K60. Frame bytes = bitrate / 8 / fps.
    let target_bitrate_bps = 50_000_000u64;
    let frame_bytes = (target_bitrate_bps / 8 / args.fps as u64) as usize;
    let frame_bytes = frame_bytes.min(12 * 1200); // cap at 12 chunks for Plan 1

    info!(
        resolution = %args.resolution,
        fps = args.fps,
        duration_ms = duration.as_millis(),
        frame_bytes,
        "starting in-process latency bench"
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
            let now_us = (Instant::now().elapsed().as_micros()) as u64;
            let frame = EncodedFrame {
                seq,
                timestamp_host_us: now_us,
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

    let mut received = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(_))) => received += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let sent = sender.await.unwrap_or(0);
    info!(sent, received, "bench done");
    Ok(())
}
