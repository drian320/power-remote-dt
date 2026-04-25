//! Full-pipeline M2 bench: synthetic BGRA texture → NVENC encode →
//! InProcTransport → MF decode → (texture). Single-process, single-machine,
//! so every stage timestamp sits on the shared `now_monotonic_us` epoch and
//! per-stage latencies are directly subtractable.
//!
//! Windows-only. NVENC needs an NVIDIA adapter and the Video Codec SDK;
//! MF H.265 decode needs the HEVC Video Extensions installed. (The module
//! is already `#[cfg(windows)]`-gated at the `mod` site in main.rs.)

use std::time::{Duration, Instant};

use prdt_media_win::synthetic::make_counter_texture;
use prdt_media_win::{
    pick_default_adapter, D3d11Device, MfD3d11Consumer, NvdecD3d11Consumer, NvencEncoder,
    NvencEncoderConfig,
};
use prdt_protocol::{now_monotonic_us, ConsumerError, EncodedFrame, VideoConsumer};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};
use tracing::{info, warn};

/// Which decoder backend the bench exercises for the "decode" stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerBackend {
    /// Media Foundation H.265 MFT via HEVC Video Extensions.
    Mf,
    /// Plan 2d direct nvcuvid.dll path.
    Nvdec,
}

impl std::str::FromStr for ConsumerBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "mf" => Ok(Self::Mf),
            "nvdec" => Ok(Self::Nvdec),
            other => anyhow::bail!("unknown consumer backend {other:?} (options: mf, nvdec)"),
        }
    }
}

/// Enum wrapper so the bench body can drive either consumer through a
/// single match-based dispatch instead of duplicating the decode loop.
enum BenchConsumer {
    Mf(MfD3d11Consumer),
    Nvdec(NvdecD3d11Consumer),
}

impl BenchConsumer {
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError> {
        match self {
            Self::Mf(c) => c.submit(frame).await,
            Self::Nvdec(c) => c.submit(frame).await,
        }
    }
    fn take_latest_texture(&mut self) -> bool {
        match self {
            Self::Mf(c) => c.take_latest_texture().is_some(),
            #[cfg(prdt_nvdec_bindings)]
            Self::Nvdec(c) => c.take_latest_dual_plane().is_some(),
            #[cfg(not(prdt_nvdec_bindings))]
            Self::Nvdec(_) => false,
        }
    }
}

pub struct FullPipelineConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration: Duration,
    pub bitrate_bps: u32,
    pub drop_ppm: u32,
    pub latency_ms: u64,
    pub csv: Option<std::path::PathBuf>,
    pub consumer: ConsumerBackend,
}

pub struct StageTimes {
    pub seq: u64,
    pub capture_us: u64,
    pub encode_done_us: u64,
    pub recv_us: u64,
    pub decode_done_us: u64,
}

/// Result of a single bench config run. `frames` is the per-frame raw
/// data; `sent` is the sender's seq counter; `received` is the count
/// of frames that made it through both transport and decode.
pub struct RunStats {
    pub sent: u64,
    pub received: u64,
    pub frames: Vec<StageTimes>,
}

pub async fn run(cfg: FullPipelineConfig) -> anyhow::Result<()> {
    let csv_path = cfg.csv.clone();
    let stats = run_for_matrix(&cfg).await?;

    if stats.frames.is_empty() {
        info!(sent = stats.sent, decoded = stats.received, "bench done but decoded 0 frames");
        return Ok(());
    }

    // Per-stage latency arrays (computed from frames).
    let mut encode: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.encode_done_us.saturating_sub(s.capture_us))
        .collect();
    let mut transport: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.recv_us.saturating_sub(s.encode_done_us))
        .collect();
    let mut decode: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.decode_done_us.saturating_sub(s.recv_us))
        .collect();
    let mut e2e: Vec<u64> = stats
        .frames
        .iter()
        .map(|s| s.decode_done_us.saturating_sub(s.capture_us))
        .collect();

    let (e50, _, e95, e99, _) = crate::percentiles(&mut encode);
    let (t50, _, t95, t99, _) = crate::percentiles(&mut transport);
    let (d50, _, d95, d99, _) = crate::percentiles(&mut decode);
    let (w50, _, w95, w99, wmax) = crate::percentiles(&mut e2e);

    info!(
        sent = stats.sent,
        decoded = stats.received,
        encode_p50_us = e50,
        encode_p95_us = e95,
        encode_p99_us = e99,
        transport_p50_us = t50,
        transport_p95_us = t95,
        transport_p99_us = t99,
        decode_p50_us = d50,
        decode_p95_us = d95,
        decode_p99_us = d99,
        e2e_p50_us = w50,
        e2e_p95_us = w95,
        e2e_p99_us = w99,
        e2e_max_us = wmax,
        "full-pipeline bench done",
    );

    if let Some(path) = csv_path {
        use std::io::Write;
        let mut wtr = std::fs::File::create(&path)?;
        writeln!(wtr, "seq,capture_us,encode_done_us,recv_us,decode_done_us,e2e_us")?;
        for s in &stats.frames {
            let e = s.decode_done_us.saturating_sub(s.capture_us);
            writeln!(
                wtr,
                "{},{},{},{},{},{}",
                s.seq, s.capture_us, s.encode_done_us, s.recv_us, s.decode_done_us, e
            )?;
        }
        info!(path = %path.display(), "wrote CSV");
    }

    Ok(())
}

/// Core bench loop without any I/O. Returns the raw per-frame samples
/// and counters; the caller decides how to log/aggregate/write CSV.
///
/// Used by both the single-config `run()` (which logs + writes one CSV)
/// and the matrix bin (which writes per-frame + summary CSVs).
pub async fn run_for_matrix(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats> {
    let adapter = pick_default_adapter().map_err(|e| anyhow::anyhow!("no GPU adapter: {e}"))?;
    if !adapter.is_nvidia() {
        anyhow::bail!(
            "full-pipeline mode requires an NVIDIA adapter; got {}",
            adapter.name
        );
    }
    let dev = D3d11Device::create(&adapter).map_err(|e| anyhow::anyhow!("D3D11 device: {e}"))?;

    let enc_cfg = NvencEncoderConfig {
        width: cfg.width,
        height: cfg.height,
        fps_numerator: cfg.fps,
        fps_denominator: 1,
        bitrate_bps: cfg.bitrate_bps,
        gop_length: cfg.fps * 2,
    };
    let encoder = NvencEncoder::new(&dev, &enc_cfg)
        .map_err(|e| anyhow::anyhow!("NvencEncoder::new: {e}"))?;
    info!(
        resolution = format!("{}x{}", cfg.width, cfg.height),
        fps = cfg.fps,
        bitrate_mbps = cfg.bitrate_bps / 1_000_000,
        "NVENC encoder ready",
    );

    let mut consumer = match cfg.consumer {
        ConsumerBackend::Mf => BenchConsumer::Mf(
            MfD3d11Consumer::new(&dev, cfg.width, cfg.height)
                .map_err(|e| anyhow::anyhow!("MfD3d11Consumer::new: {e}"))?,
        ),
        ConsumerBackend::Nvdec => BenchConsumer::Nvdec(
            NvdecD3d11Consumer::new(&dev, cfg.width, cfg.height)
                .map_err(|e| anyhow::anyhow!("NvdecD3d11Consumer::new: {e}"))?,
        ),
    };
    info!(backend = ?cfg.consumer, "decoder ready");

    let (host_side, viewer_side) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: cfg.drop_ppm,
        latency: if cfg.latency_ms > 0 {
            Some(Duration::from_millis(cfg.latency_ms))
        } else {
            None
        },
    });

    let frame_interval = Duration::from_secs_f64(1.0 / cfg.fps as f64);
    let deadline = Instant::now() + cfg.duration;
    let mut samples: Vec<StageTimes> = Vec::new();
    let mut next_tick = Instant::now();
    let mut seq: u64 = 0;
    let mut decoded: u64 = 0;

    while Instant::now() < deadline {
        let capture_us = now_monotonic_us();
        let tex = make_counter_texture(&dev, cfg.width, cfg.height, seq as u32)
            .map_err(|e| anyhow::anyhow!("synthetic texture: {e}"))?;
        let force_idr = seq == 0;
        let encoded = encoder
            .encode(&tex, force_idr, capture_us)
            .map_err(|e| anyhow::anyhow!("encode: {e}"))?;
        let encode_done_us = now_monotonic_us();

        let frame = EncodedFrame::new_h265(
            seq,
            capture_us,
            encoded.is_keyframe,
            bytes::Bytes::from(encoded.nal_bytes),
            cfg.width,
            cfg.height,
        );
        if let Err(e) = host_side.send_video(frame).await {
            warn!(?e, seq, "send_video failed; stopping");
            break;
        }

        loop {
            match tokio::time::timeout(Duration::from_millis(1), viewer_side.recv()).await {
                Ok(Ok(ReceivedMessage::Video(rx_frame))) => {
                    let recv_us = now_monotonic_us();
                    let rx_seq = rx_frame.seq;
                    let rx_capture_us = rx_frame.timestamp_host_us;
                    match consumer.submit(rx_frame).await {
                        Ok(()) => {}
                        Err(ConsumerError::Decode(msg)) => {
                            warn!(seq = rx_seq, msg, "decode error");
                            continue;
                        }
                        Err(e) => {
                            warn!(?e, "consumer error");
                            continue;
                        }
                    }
                    if consumer.take_latest_texture() {
                        let decode_done_us = now_monotonic_us();
                        decoded += 1;
                        samples.push(StageTimes {
                            seq: rx_seq,
                            capture_us: rx_capture_us,
                            encode_done_us,
                            recv_us,
                            decode_done_us,
                        });
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        seq += 1;
        next_tick += frame_interval;
        let sleep_until = next_tick;
        let now = Instant::now();
        if sleep_until > now {
            tokio::time::sleep(sleep_until - now).await;
        }
    }

    // Drain remaining decoded frames.
    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(50), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(rx_frame))) => {
                let recv_us = now_monotonic_us();
                let rx_seq = rx_frame.seq;
                let rx_capture_us = rx_frame.timestamp_host_us;
                let _ = consumer.submit(rx_frame).await;
                if consumer.take_latest_texture() {
                    let decode_done_us = now_monotonic_us();
                    decoded += 1;
                    samples.push(StageTimes {
                        seq: rx_seq,
                        capture_us: rx_capture_us,
                        encode_done_us: recv_us,
                        recv_us,
                        decode_done_us,
                    });
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    Ok(RunStats {
        sent: seq,
        received: decoded,
        frames: samples,
    })
}
