//! Full-pipeline M2 bench: synthetic BGRA texture → NVENC encode →
//! InProcTransport → MF decode → (texture). Single-process, single-machine,
//! so every stage timestamp sits on the shared `now_monotonic_us` epoch and
//! per-stage latencies are directly subtractable.
//!
//! Windows-only. NVENC needs an NVIDIA adapter and the Video Codec SDK;
//! MF H.265 decode needs the HEVC Video Extensions installed. (The module
//! is already `#[cfg(windows)]`-gated at the `mod` site in main.rs.)

use std::time::{Duration, Instant};

use prdt_media_sw::traits::{SwH264Decoder, SwH264Encoder};
use prdt_media_sw::{make_counter_i420, Openh264Decoder, Openh264Encoder, Openh264EncoderConfig};
use prdt_media_win::synthetic::make_counter_texture;
#[cfg(prdt_nvenc_bindings)]
use prdt_media_win::NvencEncoder;
use prdt_media_win::{
    pick_default_adapter, D3d11Device, Hevc265Encoder, HwHevcEncoder, MfD3d11Consumer,
    MfH265Encoder, NvdecD3d11Consumer, NvencEncoderConfig,
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
    /// Pure-software OpenH264 decoder (CPU I420 output).
    Openh264,
}

impl std::str::FromStr for ConsumerBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "mf" => Ok(Self::Mf),
            "nvdec" => Ok(Self::Nvdec),
            "openh264" => Ok(Self::Openh264),
            other => {
                anyhow::bail!("unknown consumer backend {other:?} (options: mf, nvdec, openh264)")
            }
        }
    }
}

/// Which encoder backend the bench exercises for the "encode" stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    /// NVIDIA NVENC hardware encoder.
    Nvenc,
    /// Media Foundation H.265 encoder MFT.
    Mf,
    /// Pure-software OpenH264 encoder.
    Openh264,
}

impl std::str::FromStr for EncoderBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "nvenc" => Ok(Self::Nvenc),
            "mf" => Ok(Self::Mf),
            "openh264" => Ok(Self::Openh264),
            other => {
                anyhow::bail!("unknown encoder backend {other:?} (options: nvenc, mf, openh264)")
            }
        }
    }
}

/// Enum wrapper so the bench body can drive either consumer through a
/// single match-based dispatch instead of duplicating the decode loop.
/// SW-only `Openh264` lives in the parallel `run_for_matrix_openh264`
/// path because it bypasses D3D11 entirely.
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
    pub encoder: EncoderBackend,
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
        info!(
            sent = stats.sent,
            decoded = stats.received,
            "bench done but decoded 0 frames"
        );
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
        writeln!(
            wtr,
            "seq,capture_us,encode_done_us,recv_us,decode_done_us,e2e_us"
        )?;
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
///
/// Dispatches between two paths:
/// - SW (openh264↔openh264): pure-CPU, no D3D11. Goes via
///   `run_for_matrix_openh264`.
/// - HW: NVENC/MF encode → NVDEC/MF decode through a D3D11 device, with
///   counter textures synthesised on the GPU.
///
/// Mixed configs (SW encoder + HW decoder, or vice versa) are rejected
/// — the bench has no I420↔NV12 GPU upload path and the asymmetric
/// case isn't part of the Phase 5 baseline contract.
pub async fn run_for_matrix(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats> {
    match (cfg.encoder, cfg.consumer) {
        (EncoderBackend::Openh264, ConsumerBackend::Openh264) => {
            return run_for_matrix_openh264(cfg).await;
        }
        (EncoderBackend::Openh264, _) | (_, ConsumerBackend::Openh264) => {
            anyhow::bail!(
                "mixed SW/HW pipeline not supported (encoder={:?}, decoder={:?})",
                cfg.encoder,
                cfg.consumer
            );
        }
        _ => {}
    }

    let adapter = pick_default_adapter().map_err(|e| anyhow::anyhow!("no GPU adapter: {e}"))?;
    let dev = D3d11Device::create(&adapter).map_err(|e| anyhow::anyhow!("D3D11 device: {e}"))?;

    let enc_cfg = NvencEncoderConfig {
        width: cfg.width,
        height: cfg.height,
        fps_numerator: cfg.fps,
        fps_denominator: 1,
        bitrate_bps: cfg.bitrate_bps,
        gop_length: cfg.fps * 2,
    };
    let mut encoder: HwHevcEncoder = match cfg.encoder {
        #[cfg(prdt_nvenc_bindings)]
        EncoderBackend::Nvenc => NvencEncoder::new(&dev, &enc_cfg)
            .map_err(|e| anyhow::anyhow!("NvencEncoder::new: {e}"))?
            .into(),
        #[cfg(not(prdt_nvenc_bindings))]
        EncoderBackend::Nvenc => {
            anyhow::bail!("nvenc backend not built (NV_CODEC_SDK_PATH unset at build time)")
        }
        EncoderBackend::Mf => MfH265Encoder::new(&dev, &enc_cfg)
            .map_err(|e| anyhow::anyhow!("MfH265Encoder::new: {e}"))?
            .into(),
        EncoderBackend::Openh264 => unreachable!("SW encoder dispatched above"),
    };
    info!(
        resolution = format!("{}x{}", cfg.width, cfg.height),
        fps = cfg.fps,
        bitrate_mbps = cfg.bitrate_bps / 1_000_000,
        backend = encoder.backend_name(),
        "encoder ready",
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
        ConsumerBackend::Openh264 => unreachable!("SW decoder dispatched above"),
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
        let encoded = Hevc265Encoder::encode(&mut encoder, &tex, force_idr, capture_us)
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

/// SW-only full-pipeline bench: synthetic I420 → OpenH264 encode →
/// InProcTransport → OpenH264 decode. No D3D11 device, no GPU.
/// Mirrors the structure of `run_for_matrix` so summary CSVs from
/// SW and HW configs are directly comparable.
pub async fn run_for_matrix_openh264(cfg: &FullPipelineConfig) -> anyhow::Result<RunStats> {
    let mut encoder = Openh264Encoder::new(Openh264EncoderConfig {
        width: cfg.width,
        height: cfg.height,
        target_bitrate_bps: cfg.bitrate_bps,
        max_fps: cfg.fps as f32,
    })
    .map_err(|e| anyhow::anyhow!("Openh264Encoder::new: {e}"))?;
    let mut decoder =
        Openh264Decoder::new().map_err(|e| anyhow::anyhow!("Openh264Decoder::new: {e}"))?;

    info!(
        resolution = format!("{}x{}", cfg.width, cfg.height),
        fps = cfg.fps,
        bitrate_mbps = cfg.bitrate_bps / 1_000_000,
        backend = encoder.backend_name(),
        "openh264 encoder ready",
    );
    info!(backend = decoder.backend_name(), "openh264 decoder ready");

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
        let i420 = make_counter_i420(cfg.width, cfg.height, seq as u32)
            .map_err(|e| anyhow::anyhow!("make_counter_i420: {e}"))?;
        let force_idr = seq == 0;
        let frame = encoder
            .encode(&i420, force_idr, capture_us)
            .map_err(|e| anyhow::anyhow!("encode: {e}"))?;
        let encode_done_us = now_monotonic_us();

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
                    let nal_units = rx_frame.nal_units;
                    match decoder.decode(&nal_units) {
                        Ok(Some(_)) => {
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
                        Ok(None) => {
                            // Decoder needs more input — keep going.
                        }
                        Err(e) => {
                            warn!(seq = rx_seq, ?e, "openh264 decode error");
                        }
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

    let drain_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_deadline {
        match tokio::time::timeout(Duration::from_millis(50), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(rx_frame))) => {
                let recv_us = now_monotonic_us();
                let rx_seq = rx_frame.seq;
                let rx_capture_us = rx_frame.timestamp_host_us;
                let nal_units = rx_frame.nal_units;
                if let Ok(Some(_)) = decoder.decode(&nal_units) {
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
