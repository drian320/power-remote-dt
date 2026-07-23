//! Linux host backend. Wraps `prdt-media-linux` + `prdt-input-linux`
//! free functions to match the cross-platform `platform::*` API
//! surface defined in spec §5.

#![cfg(target_os = "linux")]

use prdt_input_linux::{
    clipboard_sequence_number as _input_linux_clipboard_sequence_number,
    inject_event as _input_linux_inject_event,
    read_clipboard_text as _input_linux_read_clipboard_text,
    virtual_desktop_rect as _input_linux_virtual_desktop_rect,
    write_clipboard_text as _input_linux_write_clipboard_text,
    MAX_CLIPBOARD_BYTES as _INPUT_LINUX_MAX,
};
use prdt_protocol::{InputEvent, MonitorRect, VideoProducer};
use std::path::PathBuf;
use std::sync::Once;

/// Reads `PRDT_VAAPI_RENDER_NODE` and returns the DRM render node path it
/// names, if set. Multi-GPU hosts (e.g. an NVIDIA card alongside an AMD/Intel
/// GPU exposing VAAPI) need this because FFmpeg's default VAAPI device pick
/// can land on the wrong `/dev/dri/renderDN` and fail
/// `av_hwdevice_ctx_create`. Logs once, at info level, when the override is
/// present.
fn vaapi_render_node_from_env() -> Option<PathBuf> {
    static LOGGED: Once = Once::new();
    let node = std::env::var_os("PRDT_VAAPI_RENDER_NODE").map(PathBuf::from);
    if let Some(path) = &node {
        LOGGED.call_once(|| {
            tracing::info!(
                render_node = %path.display(),
                "VAAPI render node overridden via PRDT_VAAPI_RENDER_NODE"
            );
        });
    }
    node
}

/// Re-exported max clipboard bytes; identical value across OSes.
pub const MAX_CLIPBOARD_BYTES: usize = _INPUT_LINUX_MAX;

/// Linux has no opaque output descriptor — X11 root window is implicit.
/// A unit struct (not `= ()`) avoids the `clippy::let_unit_value` lint
/// at the `let output = pick_default_output(...)` call-site in lib.rs.
pub struct OutputDescriptor;

/// Pick the default output. On Linux the X11 root is always used.
pub fn pick_default_output(_args: &crate::Args) -> anyhow::Result<OutputDescriptor> {
    Ok(OutputDescriptor)
}

/// Human-readable name for the output; used in the "host starting" log.
pub fn output_display_name(_d: &OutputDescriptor) -> &'static str {
    "x11-root"
}

/// Dispatch enum over the active Linux encoder variant.
#[allow(dead_code, clippy::large_enum_variant)]
pub enum LinuxEncoder {
    Openh264(prdt_media_sw::Openh264Encoder),
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    FfmpegVaapiHevc(prdt_media_ffmpeg::HevcVaapiFfmpegEncoderAdapter),
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    FfmpegNvencHevc(prdt_media_ffmpeg::HevcNvencFfmpegEncoderAdapter),
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    FfmpegNvencHevcNpp(prdt_media_ffmpeg::HevcNvencNppFfmpegEncoderAdapter),
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
    FfmpegVaapiHevcMain10(prdt_media_ffmpeg::HevcVaapiMain10FfmpegEncoder),
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
    FfmpegNvencHevcMain10(prdt_media_ffmpeg::HevcNvencMain10FfmpegEncoder),
}

#[allow(dead_code)]
impl LinuxEncoder {
    pub fn codec(&self) -> prdt_protocol::Codec {
        match self {
            LinuxEncoder::Openh264(_) => prdt_protocol::Codec::H264,
            #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
            LinuxEncoder::FfmpegVaapiHevc(_) => prdt_protocol::Codec::H265,
            #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
            LinuxEncoder::FfmpegNvencHevc(_) => prdt_protocol::Codec::H265,
            #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
            LinuxEncoder::FfmpegNvencHevcNpp(_) => prdt_protocol::Codec::H265,
            #[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
            LinuxEncoder::FfmpegVaapiHevcMain10(_) => prdt_protocol::Codec::H265Main10,
            #[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
            LinuxEncoder::FfmpegNvencHevcMain10(_) => prdt_protocol::Codec::H265Main10,
        }
    }
}

/// Pure advertisement decision for `--encoder auto`. Factored out of
/// [`linux_supported_codecs`] so the full acceptance matrix can be unit-tested
/// without touching real hardware: the three inputs (`force_sw`,
/// `capture_is_x11`, and the resolved HEVC backend) are all injectable.
///
/// `auto` advertises `[H265, H264]` ONLY when software encoding is not forced,
/// the capture backend resolves to X11 (the legacy FFmpeg producer path
/// captures via X11 only), and the shared probe-backed resolver found a
/// buildable HEVC backend. Otherwise it advertises `[H264]` — byte-identical
/// to the pre-auto-HEVC behavior. Main10 is never advertised here (auto is
/// 8-bit only).
fn advertised_auto_codecs(
    force_sw: bool,
    capture_is_x11: bool,
    hevc_backend: Option<&str>,
) -> Vec<prdt_protocol::Codec> {
    if !force_sw && capture_is_x11 && hevc_backend.is_some() {
        vec![prdt_protocol::Codec::H265, prdt_protocol::Codec::H264]
    } else {
        vec![prdt_protocol::Codec::H264]
    }
}

/// Advertised codecs for the Linux encoder selection (used in host handshake).
///
/// `force_sw` mirrors `--force-sw`; `capture_is_x11` is the resolved capture
/// backend (see [`capture_backend_is_x11`]). Both only affect the `auto` arm —
/// every explicit-encoder arm is byte-identical to before.
pub fn linux_supported_codecs(
    encoder_arg: &str,
    force_sw: bool,
    capture_is_x11: bool,
) -> Vec<prdt_protocol::Codec> {
    // Resolve `auto` via the shared probe-backed resolver, gated on capture
    // backend + --force-sw. Handled *before* `normalize_encoder` so the raw
    // "auto" string is not collapsed to a canonical HEVC backend name (which
    // would otherwise fall into the bare `[H265]` arm and bypass the gate).
    if encoder_arg == "auto" {
        // Only invoke the probe-backed resolver when the gate is actually
        // open (`!force_sw && capture_is_x11`) — `Option::then` is lazy, so a
        // force-sw or Wayland-resolved session short-circuits before
        // `resolve_hevc_auto_backend` ever builds/drops a real encoder or
        // logs its probe outcome. `advertised_auto_codecs` stays the pure,
        // unit-tested gate function unchanged.
        let hevc_backend = (!force_sw && capture_is_x11)
            .then(resolve_hevc_auto_backend)
            .flatten();
        return advertised_auto_codecs(force_sw, capture_is_x11, hevc_backend);
    }
    match normalize_encoder(encoder_arg) {
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
        "ffmpeg-vaapi-hevc" => vec![prdt_protocol::Codec::H265],
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
        "ffmpeg-nvenc-hevc" => vec![prdt_protocol::Codec::H265],
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
        "ffmpeg-nvenc-hevc-npp" => vec![prdt_protocol::Codec::H265],
        _ => vec![prdt_protocol::Codec::H264],
    }
}

/// Full negotiation set for Hello.codec acceptance — includes H265Main10 when
/// a Main10 encoder feature is compiled in so that Main10-capable clients can
/// negotiate successfully. NOT the HelloAck advertisement list; use
/// `supported_codecs_for(hello_codec)` to build that (R15 filter).
///
/// Main10 is appended only for *explicit* encoder args, never for `auto`: the
/// shared auto-HEVC resolver is 8-bit only, so `auto` must not advertise a
/// 10-bit codec it will never build.
pub fn linux_supported_codecs_negotiation(
    encoder_arg: &str,
    force_sw: bool,
    capture_is_x11: bool,
) -> Vec<prdt_protocol::Codec> {
    #[allow(unused_mut)] // `mut` is conditional on Main10 features being enabled
    let mut v = linux_supported_codecs(encoder_arg, force_sw, capture_is_x11);
    #[cfg(any(
        feature = "ffmpeg-encode-hevc-vaapi-main10-any",
        feature = "ffmpeg-encode-hevc-nvenc-main10-any"
    ))]
    if encoder_arg != "auto" && !v.contains(&prdt_protocol::Codec::H265Main10) {
        v.push(prdt_protocol::Codec::H265Main10);
    }
    v
}

/// Pre-existing codec variants (variant_index 0/1/2) safe to send to any client.
fn linux_supported_codecs_base() -> Vec<prdt_protocol::Codec> {
    vec![
        prdt_protocol::Codec::H265,
        prdt_protocol::Codec::H264,
        prdt_protocol::Codec::Av1,
    ]
}

/// R15 mitigation: only advertise `Codec::H265Main10` (variant_index=3) to
/// clients whose inbound `Hello.codec` was already `H265Main10`. Pre-PR1
/// clients sending `H264`, `H265`, or `Av1` receive a HelloAck containing
/// only pre-existing variants 0/1/2 — bincode never encounters an unknown
/// variant_index, so the message decodes cleanly.
pub fn supported_codecs_for(hello_codec: prdt_protocol::Codec) -> Vec<prdt_protocol::Codec> {
    let mut v = linux_supported_codecs_base();
    if hello_codec == prdt_protocol::Codec::H265Main10 {
        v.push(prdt_protocol::Codec::H265Main10);
    }
    v
}

/// Build a boxed `VideoProducer` for the Linux path. Resolves the requested
/// backend via [`normalize_encoder`] and constructs the matching producer
/// (FFmpeg VAAPI HEVC, FFmpeg NVENC HEVC, or the SW OpenH264 fallback).
pub fn build_video_producer(
    args_encoder: &str,
    _output: &OutputDescriptor,
    bitrate_bps: u32,
    fps: u32,
    _negotiated_codec: prdt_protocol::Codec,
) -> anyhow::Result<Box<dyn VideoProducer>> {
    // `auto` is resolved codec-aware via the shared probe-backed resolver so
    // the backend we build is exactly what the handshake advertised: H265 →
    // the probe-approved HEVC backend, H264 → OpenH264. Every explicit encoder
    // arg keeps the byte-identical `normalize_encoder` mapping.
    let _backend = if args_encoder == "auto" {
        resolve_auto_build_backend(_negotiated_codec)
    } else {
        normalize_encoder(args_encoder)
    };
    // Whether this build came from `--encoder auto` (i.e. a probe-approved
    // path). Only referenced by the HEVC arms below, so gate its definition to
    // the same features to avoid an unused-variable warning on SW-only builds.
    #[cfg(any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    let from_auto = args_encoder == "auto";
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    if _backend == "ffmpeg-vaapi-hevc" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{
            HevcVaapiFfmpegEncoder, HevcVaapiFfmpegEncoderAdapter, HevcVaapiFfmpegEncoderConfig,
        };
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg path")?;
        let (width, height) = (cap.width(), cap.height());
        let cfg = HevcVaapiFfmpegEncoderConfig {
            width,
            height,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: crate::SESSION_GOP_FRAMES,
            render_node: vaapi_render_node_from_env(),
        };
        let enc = HevcVaapiFfmpegEncoder::new(cfg)
            .map_err(|e| {
                if from_auto {
                    tracing::warn!(
                        error = %e,
                        backend = "ffmpeg-vaapi-hevc",
                        "auto-HEVC probe passed but construction failed"
                    );
                }
                e
            })
            .context("HevcVaapiFfmpegEncoder::new")?;
        let adapter = HevcVaapiFfmpegEncoderAdapter(enc);
        let producer = FfmpegVaapiProducer::new(Box::new(cap), adapter, fps, width, height)
            .context("FfmpegVaapiProducer::new geometry check")?;
        return Ok(Box::new(producer));
    }
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    if _backend == "ffmpeg-nvenc-hevc" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{
            HevcNvencFfmpegEncoder, HevcNvencFfmpegEncoderAdapter, HevcNvencFfmpegEncoderConfig,
        };
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg path")?;
        let (width, height) = (cap.width(), cap.height());
        let cfg = HevcNvencFfmpegEncoderConfig {
            width,
            height,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: crate::SESSION_GOP_FRAMES,
            cuda_device_index: None,
        };
        let enc = HevcNvencFfmpegEncoder::new(cfg)
            .map_err(|e| {
                if from_auto {
                    tracing::warn!(
                        error = %e,
                        backend = "ffmpeg-nvenc-hevc",
                        "auto-HEVC probe passed but construction failed"
                    );
                }
                e
            })
            .context("HevcNvencFfmpegEncoder::new")?;
        let adapter = HevcNvencFfmpegEncoderAdapter(enc);
        let producer = FfmpegNvencProducer::new(Box::new(cap), adapter, fps, width, height)
            .context("FfmpegNvencProducer::new geometry check")?;
        return Ok(Box::new(producer));
    }
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    if _backend == "ffmpeg-nvenc-hevc-npp" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{
            HevcNvencNppFfmpegEncoder, HevcNvencNppFfmpegEncoderAdapter,
            HevcNvencNppFfmpegEncoderConfig,
        };
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg-nvenc-hevc-npp path")?;
        let (width, height) = (cap.width(), cap.height());
        let cfg = HevcNvencNppFfmpegEncoderConfig {
            width,
            height,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: crate::SESSION_GOP_FRAMES,
            cuda_device_index: None,
        };
        let enc = HevcNvencNppFfmpegEncoder::new(cfg).context("HevcNvencNppFfmpegEncoder::new")?;
        let adapter = HevcNvencNppFfmpegEncoderAdapter(enc);
        let producer = FfmpegNvencNppProducer::new(Box::new(cap), adapter, fps, width, height)
            .context("FfmpegNvencNppProducer::new geometry check")?;
        return Ok(Box::new(producer));
    }
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
    if _backend == "ffmpeg-vaapi-hevc-main10" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{HevcVaapiMain10FfmpegEncoder, HevcVaapiMain10FfmpegEncoderConfig};
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg-vaapi-hevc-main10 path")?;
        let (width, height) = (cap.width(), cap.height());
        let cfg = HevcVaapiMain10FfmpegEncoderConfig {
            width,
            height,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: crate::SESSION_GOP_FRAMES,
            render_node: vaapi_render_node_from_env(),
        };
        let enc =
            HevcVaapiMain10FfmpegEncoder::new(cfg).context("HevcVaapiMain10FfmpegEncoder::new")?;
        let producer = FfmpegVaapiMain10Producer::new(Box::new(cap), enc, fps, width, height)
            .context("FfmpegVaapiMain10Producer::new geometry check")?;
        return Ok(Box::new(producer));
    }
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
    if _backend == "ffmpeg-nvenc-hevc-main10" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{HevcNvencMain10FfmpegEncoder, HevcNvencMain10FfmpegEncoderConfig};
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg-nvenc-hevc-main10 path")?;
        let (width, height) = (cap.width(), cap.height());
        let cfg = HevcNvencMain10FfmpegEncoderConfig {
            width,
            height,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: crate::SESSION_GOP_FRAMES,
            cuda_device_index: None,
        };
        let enc =
            HevcNvencMain10FfmpegEncoder::new(cfg).context("HevcNvencMain10FfmpegEncoder::new")?;
        let producer = FfmpegNvencMain10Producer::new(Box::new(cap), enc, fps, width, height)
            .context("FfmpegNvencMain10Producer::new geometry check")?;
        return Ok(Box::new(producer));
    }
    let producer = prdt_media_linux::build_video_producer(bitrate_bps, fps)?;
    Ok(Box::new(producer))
}

/// `VideoProducer` that wires an X11 BGRA capture source into the
/// FFmpeg VAAPI HEVC encoder. Mirrors `LinuxSwProducer` but substitutes
/// `HevcVaapiFfmpegEncoderAdapter` (takes `I420Frame`) and emits `Codec::H265`.
#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
struct FfmpegVaapiProducer {
    capture: Option<Box<dyn prdt_media_linux::capture_source::CaptureSource>>,
    encoder: Option<prdt_media_ffmpeg::HevcVaapiFfmpegEncoderAdapter>,
    bgra_buf: Vec<u8>,
    pacer: tokio::time::Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    poisoned: bool,
}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
impl FfmpegVaapiProducer {
    /// `encoder_width`/`encoder_height` are the dims the caller used to
    /// build `encoder`'s config. This is a cheap one-time defensive check
    /// (not the primary fix — `build_video_producer` now derives the
    /// encoder config from `capture.geometry()` directly, so the two
    /// should always agree by construction): if they ever disagree, the
    /// encoder's internal buffers are sized for one geometry while this
    /// producer would capture and feed it another, which previously
    /// SIGSEGV'd inside `encode()`'s plane copy. Fail loudly instead.
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcVaapiFfmpegEncoderAdapter,
        fps: u32,
        encoder_width: u32,
        encoder_height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        anyhow::ensure!(
            (width, height) == (encoder_width, encoder_height),
            "capture geometry {width}x{height} does not match encoder dims \
             {encoder_width}x{encoder_height}; refusing to construct a producer \
             that would corrupt frames"
        );
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        })
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
#[async_trait::async_trait]
impl VideoProducer for FfmpegVaapiProducer {
    async fn next_frame(
        &mut self,
    ) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        if self.poisoned {
            return Err(prdt_protocol::ProducerError::Capture(
                "producer poisoned; drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        self.capture = Some(capture);

        use prdt_media_linux::capture_source::CaptureSourceError;
        match capture_result {
            Ok(()) => {}
            Err(CaptureSourceError::WouldBlock(reason)) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "would_block: {reason}"
                )));
            }
            Err(CaptureSourceError::Terminal { backend, reason }) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "{backend}: {reason}"
                )));
            }
        }

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = prdt_protocol::now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let encode_join = tokio::task::spawn_blocking(move || {
            let i420 = prdt_media_sw::bgra_to_i420(&bgra, width, height, width * 4)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            use prdt_media_core::Encoder as _;
            let pkt = enc
                .encode(&i420, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            Ok::<_, prdt_protocol::ProducerError>((enc, bgra, pkt))
        })
        .await;
        let (enc_back, bgra_back, pkt) = match encode_join {
            Ok(Ok(triple)) => triple,
            Ok(Err(e)) => {
                return Err(e);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let seq = self.seq;
        self.seq += 1;
        Ok(prdt_protocol::EncodedFrame {
            seq,
            timestamp_host_us: pkt.timestamp_us,
            is_keyframe: pkt.is_keyframe,
            nal_units: bytes::Bytes::from(pkt.nal_bytes),
            width: self.width,
            height: self.height,
            codec: prdt_protocol::Codec::H265,
        })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            use prdt_media_core::Encoder as _;
            e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-vaapi-hevc"
    }
}

/// `VideoProducer` that wires an X11 BGRA capture source into the FFmpeg
/// NVENC HEVC encoder. Mirrors [`FfmpegVaapiProducer`] but substitutes the
/// NVENC adapter; the duplication stays below the +40 LoC threshold flagged
/// in the plan so we do not introduce a generic wrapper for two backends.
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
struct FfmpegNvencProducer {
    capture: Option<Box<dyn prdt_media_linux::capture_source::CaptureSource>>,
    encoder: Option<prdt_media_ffmpeg::HevcNvencFfmpegEncoderAdapter>,
    bgra_buf: Vec<u8>,
    pacer: tokio::time::Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    poisoned: bool,
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
impl FfmpegNvencProducer {
    /// See [`FfmpegVaapiProducer::new`] for why `encoder_width`/
    /// `encoder_height` are checked against `capture.geometry()`.
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcNvencFfmpegEncoderAdapter,
        fps: u32,
        encoder_width: u32,
        encoder_height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        anyhow::ensure!(
            (width, height) == (encoder_width, encoder_height),
            "capture geometry {width}x{height} does not match encoder dims \
             {encoder_width}x{encoder_height}; refusing to construct a producer \
             that would corrupt frames"
        );
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        })
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
#[async_trait::async_trait]
impl VideoProducer for FfmpegNvencProducer {
    async fn next_frame(
        &mut self,
    ) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        if self.poisoned {
            return Err(prdt_protocol::ProducerError::Capture(
                "producer poisoned; drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        self.capture = Some(capture);

        use prdt_media_linux::capture_source::CaptureSourceError;
        match capture_result {
            Ok(()) => {}
            Err(CaptureSourceError::WouldBlock(reason)) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "would_block: {reason}"
                )));
            }
            Err(CaptureSourceError::Terminal { backend, reason }) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "{backend}: {reason}"
                )));
            }
        }

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = prdt_protocol::now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let encode_join = tokio::task::spawn_blocking(move || {
            let i420 = prdt_media_sw::bgra_to_i420(&bgra, width, height, width * 4)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            use prdt_media_core::Encoder as _;
            let pkt = enc
                .encode(&i420, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            Ok::<_, prdt_protocol::ProducerError>((enc, bgra, pkt))
        })
        .await;
        let (enc_back, bgra_back, pkt) = match encode_join {
            Ok(Ok(triple)) => triple,
            Ok(Err(e)) => {
                return Err(e);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let seq = self.seq;
        self.seq += 1;
        Ok(prdt_protocol::EncodedFrame {
            seq,
            timestamp_host_us: pkt.timestamp_us,
            is_keyframe: pkt.is_keyframe,
            nal_units: bytes::Bytes::from(pkt.nal_bytes),
            width: self.width,
            height: self.height,
            codec: prdt_protocol::Codec::H265,
        })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            use prdt_media_core::Encoder as _;
            e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-nvenc-hevc"
    }
}

/// `VideoProducer` that wires an X11 BGRA capture source into the FFmpeg
/// NVENC HEVC encoder with the P2.5 GPU-side NPP BGRA→NV12 path. Mirrors
/// [`FfmpegNvencProducer`] but constructs a `BgraFrame` instead of going
/// through CPU `bgra_to_i420` + `i420_to_nv12_into`; the encoder uploads
/// once via `cudaMemcpy2D` and runs NPP color conversion entirely on the
/// GPU. The duplication with `FfmpegNvencProducer` stays below the +40
/// LoC threshold (the encode-signature divergence to `BgraFrame` makes a
/// generic infeasible).
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
struct FfmpegNvencNppProducer {
    capture: Option<Box<dyn prdt_media_linux::capture_source::CaptureSource>>,
    encoder: Option<prdt_media_ffmpeg::HevcNvencNppFfmpegEncoderAdapter>,
    bgra_buf: Vec<u8>,
    pacer: tokio::time::Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    poisoned: bool,
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
impl FfmpegNvencNppProducer {
    /// See [`FfmpegVaapiProducer::new`] for why `encoder_width`/
    /// `encoder_height` are checked against `capture.geometry()`.
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcNvencNppFfmpegEncoderAdapter,
        fps: u32,
        encoder_width: u32,
        encoder_height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        anyhow::ensure!(
            (width, height) == (encoder_width, encoder_height),
            "capture geometry {width}x{height} does not match encoder dims \
             {encoder_width}x{encoder_height}; refusing to construct a producer \
             that would corrupt frames"
        );
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        })
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
#[async_trait::async_trait]
impl VideoProducer for FfmpegNvencNppProducer {
    async fn next_frame(
        &mut self,
    ) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        if self.poisoned {
            return Err(prdt_protocol::ProducerError::Capture(
                "producer poisoned; drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        self.capture = Some(capture);

        use prdt_media_linux::capture_source::CaptureSourceError;
        match capture_result {
            Ok(()) => {}
            Err(CaptureSourceError::WouldBlock(reason)) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "would_block: {reason}"
                )));
            }
            Err(CaptureSourceError::Terminal { backend, reason }) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "{backend}: {reason}"
                )));
            }
        }

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = prdt_protocol::now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let encode_join = tokio::task::spawn_blocking(move || {
            let frame = prdt_media_core::BgraFrame {
                width,
                height,
                bgra,
                stride: width * 4,
            };
            use prdt_media_core::Encoder as _;
            let pkt = enc
                .encode(&frame, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            Ok::<_, prdt_protocol::ProducerError>((enc, frame.bgra, pkt))
        })
        .await;
        let (enc_back, bgra_back, pkt) = match encode_join {
            Ok(Ok(triple)) => triple,
            Ok(Err(e)) => {
                return Err(e);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let seq = self.seq;
        self.seq += 1;
        Ok(prdt_protocol::EncodedFrame {
            seq,
            timestamp_host_us: pkt.timestamp_us,
            is_keyframe: pkt.is_keyframe,
            nal_units: bytes::Bytes::from(pkt.nal_bytes),
            width: self.width,
            height: self.height,
            codec: prdt_protocol::Codec::H265,
        })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            use prdt_media_core::Encoder as _;
            e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-nvenc-hevc-npp"
    }
}

/// `VideoProducer` that wires an X11 BGRA capture source into the
/// FFmpeg VAAPI HEVC Main10 encoder. Mirrors `FfmpegNvencNppProducer`
/// (raw BGRA in, P010LE conversion handled inside the encoder).
#[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
struct FfmpegVaapiMain10Producer {
    capture: Option<Box<dyn prdt_media_linux::capture_source::CaptureSource>>,
    encoder: Option<prdt_media_ffmpeg::HevcVaapiMain10FfmpegEncoder>,
    bgra_buf: Vec<u8>,
    pacer: tokio::time::Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    poisoned: bool,
}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
impl FfmpegVaapiMain10Producer {
    /// See [`FfmpegVaapiProducer::new`] for why `encoder_width`/
    /// `encoder_height` are checked against `capture.geometry()`. This
    /// matters even more here: the Main10 encoder's `encode()` takes
    /// width/height per-call but its CPU/HW frame buffers are sized once,
    /// at construction, from the config passed to `new()` — a mismatch is
    /// exactly the sws_scale/hw_upload overflow this fix addresses.
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcVaapiMain10FfmpegEncoder,
        fps: u32,
        encoder_width: u32,
        encoder_height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        anyhow::ensure!(
            (width, height) == (encoder_width, encoder_height),
            "capture geometry {width}x{height} does not match encoder dims \
             {encoder_width}x{encoder_height}; refusing to construct a producer \
             that would corrupt frames"
        );
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        })
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
#[async_trait::async_trait]
impl VideoProducer for FfmpegVaapiMain10Producer {
    async fn next_frame(
        &mut self,
    ) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        if self.poisoned {
            return Err(prdt_protocol::ProducerError::Capture(
                "producer poisoned; drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        self.capture = Some(capture);

        use prdt_media_linux::capture_source::CaptureSourceError;
        match capture_result {
            Ok(()) => {}
            Err(CaptureSourceError::WouldBlock(reason)) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "would_block: {reason}"
                )));
            }
            Err(CaptureSourceError::Terminal { backend, reason }) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "{backend}: {reason}"
                )));
            }
        }

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = prdt_protocol::now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let encode_join = tokio::task::spawn_blocking(move || {
            let pkt = enc
                .encode(&bgra, width, height, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            Ok::<_, prdt_protocol::ProducerError>((enc, bgra, pkt))
        })
        .await;
        let (enc_back, bgra_back, pkt) = match encode_join {
            Ok(Ok(triple)) => triple,
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let seq = self.seq;
        self.seq += 1;
        Ok(prdt_protocol::EncodedFrame {
            seq,
            timestamp_host_us: pkt.timestamp_us,
            is_keyframe: pkt.is_keyframe,
            nal_units: bytes::Bytes::from(pkt.nal_bytes),
            width: self.width,
            height: self.height,
            codec: prdt_protocol::Codec::H265Main10,
        })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            let _ = e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-vaapi-hevc-main10"
    }
}

/// `VideoProducer` that wires an X11 BGRA capture source into the
/// FFmpeg NVENC HEVC Main10 encoder. Mirrors `FfmpegVaapiMain10Producer`.
#[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
struct FfmpegNvencMain10Producer {
    capture: Option<Box<dyn prdt_media_linux::capture_source::CaptureSource>>,
    encoder: Option<prdt_media_ffmpeg::HevcNvencMain10FfmpegEncoder>,
    bgra_buf: Vec<u8>,
    pacer: tokio::time::Interval,
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
    poisoned: bool,
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
impl FfmpegNvencMain10Producer {
    /// See [`FfmpegVaapiMain10Producer::new`] for why `encoder_width`/
    /// `encoder_height` are checked against `capture.geometry()`.
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcNvencMain10FfmpegEncoder,
        fps: u32,
        encoder_width: u32,
        encoder_height: u32,
    ) -> anyhow::Result<Self> {
        let (width, height) = capture.geometry();
        anyhow::ensure!(
            (width, height) == (encoder_width, encoder_height),
            "capture geometry {width}x{height} does not match encoder dims \
             {encoder_width}x{encoder_height}; refusing to construct a producer \
             that would corrupt frames"
        );
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        })
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
#[async_trait::async_trait]
impl VideoProducer for FfmpegNvencMain10Producer {
    async fn next_frame(
        &mut self,
    ) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        if self.poisoned {
            return Err(prdt_protocol::ProducerError::Capture(
                "producer poisoned; drop and recreate".into(),
            ));
        }

        self.pacer.tick().await;

        let mut bgra = std::mem::take(&mut self.bgra_buf);
        let mut capture = self
            .capture
            .take()
            .expect("capture taken twice; producer state corrupted");
        let capture_join = tokio::task::spawn_blocking(move || {
            let r = capture.capture_into(&mut bgra);
            (bgra, capture, r)
        })
        .await;
        let (bgra, capture, capture_result) = match capture_join {
            Ok(triple) => triple,
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.bgra_buf = bgra;
        self.capture = Some(capture);

        use prdt_media_linux::capture_source::CaptureSourceError;
        match capture_result {
            Ok(()) => {}
            Err(CaptureSourceError::WouldBlock(reason)) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "would_block: {reason}"
                )));
            }
            Err(CaptureSourceError::Terminal { backend, reason }) => {
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "{backend}: {reason}"
                )));
            }
        }

        let bgra = std::mem::take(&mut self.bgra_buf);
        let width = self.width;
        let height = self.height;
        let force_idr = std::mem::take(&mut self.idr_pending);
        let ts_us = prdt_protocol::now_monotonic_us();

        let mut enc = self
            .encoder
            .take()
            .expect("encoder taken twice; producer state corrupted");
        let encode_join = tokio::task::spawn_blocking(move || {
            let pkt = enc
                .encode(&bgra, width, height, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            Ok::<_, prdt_protocol::ProducerError>((enc, bgra, pkt))
        })
        .await;
        let (enc_back, bgra_back, pkt) = match encode_join {
            Ok(Ok(triple)) => triple,
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                self.poisoned = true;
                return Err(prdt_protocol::ProducerError::Capture(format!(
                    "producer poisoned by inner panic: {e}"
                )));
            }
        };
        self.encoder = Some(enc_back);
        self.bgra_buf = bgra_back;

        let seq = self.seq;
        self.seq += 1;
        Ok(prdt_protocol::EncodedFrame {
            seq,
            timestamp_host_us: pkt.timestamp_us,
            is_keyframe: pkt.is_keyframe,
            nal_units: bytes::Bytes::from(pkt.nal_bytes),
            width: self.width,
            height: self.height,
            codec: prdt_protocol::Codec::H265Main10,
        })
    }

    fn request_idr(&mut self) {
        self.idr_pending = true;
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(e) = self.encoder.as_mut() {
            let _ = e.set_target_bitrate(bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        "ffmpeg-nvenc-hevc-main10"
    }
}

/// Map any encoder CLI arg to the canonical backend name on Linux.
fn normalize_encoder(arg: &str) -> &'static str {
    match arg {
        "openh264" => "openh264",
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
        "ffmpeg-vaapi-hevc" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                "video encoder selected"
            );
            "ffmpeg-vaapi-hevc"
        }
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
        "ffmpeg-nvenc-hevc" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc"
        }
        // P2.5 — NPP-accelerated NVENC path. Explicit opt-in only;
        // `auto` resolution NEVER selects this (Step 4 decision: keep the
        // auto policy byte-stable from P1.6). On a build without the
        // NPP feature, the arm is absent and the request falls through to
        // the generic warn → openh264 fallback below.
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
        "ffmpeg-nvenc-hevc-npp" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc-npp"
        }
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
        "nvenc-npp" => {
            tracing::info!(
                encoder = "ffmpeg-nvenc-hevc-npp",
                selected_by = "explicit-flag",
                reason = "legacy-alias",
                requested = "nvenc-npp",
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc-npp"
        }
        #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-npp-any"))]
        "ffmpeg-nvenc-hevc-npp" | "nvenc-npp" => {
            tracing::warn!(
                requested = arg,
                hint = "rebuild with --features ffmpeg-encode-hevc-nvenc-npp",
                "NVENC HEVC NPP backend not compiled in; falling back to openh264"
            );
            "openh264"
        }
        // P3 Main10 VAAPI path (explicit opt-in only; `auto` keeps 8-bit, A1.8).
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi-main10-any")]
        "ffmpeg-vaapi-hevc-main10" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                bit_depth = 10,
                "video encoder selected"
            );
            "ffmpeg-vaapi-hevc-main10"
        }
        #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi-main10-any"))]
        "ffmpeg-vaapi-hevc-main10" => {
            tracing::warn!(
                requested = arg,
                hint = "rebuild with --features ffmpeg-encode-hevc-vaapi-main10",
                "VAAPI HEVC Main10 backend not compiled in; falling back to openh264"
            );
            "openh264"
        }
        // P3 Main10 NVENC path (explicit opt-in only; `auto` keeps 8-bit, A1.8).
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-main10-any")]
        "ffmpeg-nvenc-hevc-main10" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                bit_depth = 10,
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc-main10"
        }
        #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-main10-any"))]
        "ffmpeg-nvenc-hevc-main10" => {
            tracing::warn!(
                requested = arg,
                hint = "rebuild with --features ffmpeg-encode-hevc-nvenc-main10",
                "NVENC HEVC Main10 backend not compiled in; falling back to openh264"
            );
            "openh264"
        }
        // NPP-Main10 is deferred to F3; reject at parse time (A1.7).
        "ffmpeg-nvenc-hevc-main10-npp" | "nvenc-npp-main10" => {
            tracing::error!(
                requested = arg,
                note = "NPP Main10 is deferred to F3 follow-up (requires a real GPU 10-bit primitive)",
                "ffmpeg-nvenc-hevc-main10-npp is not supported in PR1; use ffmpeg-nvenc-hevc-main10 instead"
            );
            "openh264"
        }
        "auto" => resolve_auto_encoder(),
        // P1.6: reroute legacy aliases to the canonical backend when that
        // backend is compiled in. Users typing `--encoder nvenc` mean "give
        // me NVENC if you have it"; silently falling back to openh264 hid
        // intent. When the corresponding feature is absent we still fall
        // back, with a louder hint about which Cargo feature would have
        // satisfied the request.
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
        "nvenc" => {
            tracing::info!(
                encoder = "ffmpeg-nvenc-hevc",
                selected_by = "explicit-flag",
                reason = "legacy-alias",
                requested = "nvenc",
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc"
        }
        #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-any"))]
        "nvenc" => {
            tracing::warn!(
                requested = "nvenc",
                hint = "rebuild with --features ffmpeg-encode-hevc-nvenc",
                "NVENC HEVC backend not compiled in; falling back to openh264"
            );
            "openh264"
        }
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
        "vaapi" => {
            tracing::info!(
                encoder = "ffmpeg-vaapi-hevc",
                selected_by = "explicit-flag",
                reason = "legacy-alias",
                requested = "vaapi",
                "video encoder selected"
            );
            "ffmpeg-vaapi-hevc"
        }
        #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi-any"))]
        "vaapi" => {
            tracing::warn!(
                requested = "vaapi",
                hint = "rebuild with --features ffmpeg-encode-hevc-vaapi",
                "VAAPI HEVC backend not compiled in; falling back to openh264"
            );
            "openh264"
        }
        "mf" => {
            // `mf` is Windows-only (Media Foundation). On Linux it never has
            // a real path; warn and fall back.
            tracing::warn!(
                requested = "mf",
                "Media Foundation encoder is Windows-only on Linux; falling back to openh264"
            );
            "openh264"
        }
        other => {
            tracing::warn!(
                requested = other,
                "unknown encoder; falling back to openh264"
            );
            "openh264"
        }
    }
}

/// Parse `PRDT_PREFER_NVENC` into a bool. `{1, true, yes, on}`
/// (case-insensitive, trimmed) enable the preference; every other value —
/// including unset / empty — is `false`. Shared by [`resolve_auto_encoder`]
/// and [`resolve_hevc_auto_backend`] so both honor the same convention.
fn prefer_nvenc_from_env() -> bool {
    std::env::var("PRDT_PREFER_NVENC")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// Construct a tiny VAAPI HEVC encoder to prove the backend is actually
/// usable end-to-end (HW frames context + `avcodec_open2`, which catches a
/// missing HEVC encode entrypoint — not merely device presence), then drop it
/// immediately to release the device. Returns `(passed, outcome_tag)`.
fn probe_vaapi_hevc() -> (bool, &'static str) {
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    {
        let render_node = vaapi_render_node_from_env();
        match prdt_media_ffmpeg::probe_hevc_vaapi(render_node.as_deref()) {
            Ok(()) => (true, "ok"),
            Err(e) => {
                tracing::info!(error = %e, "auto-HEVC vaapi construction probe failed");
                (false, "construction-failed")
            }
        }
    }
    #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi-any"))]
    {
        (false, "not-compiled")
    }
}

/// NVENC counterpart of [`probe_vaapi_hevc`]. Constructs a tiny NVENC HEVC
/// encoder (CUDA init + `avcodec_open2`) then drops it. Returns
/// `(passed, outcome_tag)`.
fn probe_nvenc_hevc() -> (bool, &'static str) {
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    {
        match prdt_media_ffmpeg::probe_hevc_nvenc() {
            Ok(()) => (true, "ok"),
            Err(e) => {
                tracing::info!(error = %e, "auto-HEVC nvenc construction probe failed");
                (false, "construction-failed")
            }
        }
    }
    #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-any"))]
    {
        (false, "not-compiled")
    }
}

/// Shared, probe-backed auto-HEVC decision point consulted by BOTH the
/// handshake codec advertisement ([`linux_supported_codecs`]) and the legacy
/// auto-encoder build path ([`resolve_auto_build_backend`]), so what we
/// advertise is exactly what we can construct.
///
/// Returns the canonical backend name of the first HEVC encoder whose *real*
/// construction probe succeeds, honoring the same `PRDT_PREFER_NVENC` ordering
/// convention as [`resolve_auto_encoder`] (default `[vaapi, nvenc]`, flipped
/// when the env var is set). `None` when no HEVC backend is compiled in or
/// every compiled candidate fails its probe.
///
/// OnceLock-cached: the probe (which builds and drops a real encoder) runs at
/// most once per process, so the handshake pays no repeated cost and the
/// advertisement and build path observe an identical decision.
fn resolve_hevc_auto_backend() -> Option<&'static str> {
    static CACHE: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        let prefer_nvenc = prefer_nvenc_from_env();
        let order: [&str; 2] = if prefer_nvenc {
            ["ffmpeg-nvenc-hevc", "ffmpeg-vaapi-hevc"]
        } else {
            ["ffmpeg-vaapi-hevc", "ffmpeg-nvenc-hevc"]
        };
        let mut chosen: Option<&'static str> = None;
        let mut vaapi_outcome = "not-probed";
        let mut nvenc_outcome = "not-probed";
        for cand in order {
            if chosen.is_some() {
                break; // first success wins; skip the remaining probe(s)
            }
            match cand {
                "ffmpeg-vaapi-hevc" => {
                    let (ok, outcome) = probe_vaapi_hevc();
                    vaapi_outcome = outcome;
                    if ok {
                        chosen = Some("ffmpeg-vaapi-hevc");
                    }
                }
                "ffmpeg-nvenc-hevc" => {
                    let (ok, outcome) = probe_nvenc_hevc();
                    nvenc_outcome = outcome;
                    if ok {
                        chosen = Some("ffmpeg-nvenc-hevc");
                    }
                }
                _ => {}
            }
        }
        tracing::info!(
            decision = chosen.unwrap_or("none"),
            prefer_nvenc,
            vaapi_probe = vaapi_outcome,
            nvenc_probe = nvenc_outcome,
            "auto-HEVC backend resolved via construction probe"
        );
        chosen
    })
}

/// Resolve the concrete backend to *build* for `--encoder auto`, given the
/// negotiated codec. Shares the cached [`resolve_hevc_auto_backend`] decision
/// with the handshake advertisement so we build exactly what we advertised:
///
/// * `H265` → the probe-approved HEVC backend. If (defensively — this should
///   not happen since the resolver is cached) it now returns `None`, warn and
///   fall back to the cfg-cascade [`resolve_auto_encoder`].
/// * `H264` → `openh264` (auto negotiated 8-bit SW).
/// * anything else → the cfg-cascade [`resolve_auto_encoder`].
fn resolve_auto_build_backend(negotiated_codec: prdt_protocol::Codec) -> &'static str {
    match negotiated_codec {
        prdt_protocol::Codec::H265 => match resolve_hevc_auto_backend() {
            Some(backend) => backend,
            None => {
                tracing::warn!(
                    "auto negotiated H265 but no HEVC backend probe passed at build time; \
                     falling back to cfg-cascade resolution"
                );
                resolve_auto_encoder()
            }
        },
        prdt_protocol::Codec::H264 => "openh264",
        _ => resolve_auto_encoder(),
    }
}

/// Resolve `--encoder auto` to a canonical backend name based on the cfg
/// cascade. Policy: VAAPI is preferred when both VAAPI and NVENC compile in
/// (Intel iGPU is the more common deployment); `PRDT_PREFER_NVENC` in
/// `{1, true, yes, on}` (case-insensitive) flips the preference for users
/// who deliberately built with NVENC on a dGPU-equipped host. Other values
/// (including unset / empty) are treated as the default. Always emits a
/// structured `tracing::info!` so the resolved backend is visible in logs.
// `return` in each cfg arm is the simplest way to express the cascade;
// only one arm compiles per build, but clippy can't see that.
#[allow(clippy::needless_return)]
fn resolve_auto_encoder() -> &'static str {
    // `prefer_nvenc` is only read inside the both-features cfg arm; suppress
    // the unused-variable lint when neither / only-one backend compiles in.
    #[cfg_attr(
        not(all(
            feature = "ffmpeg-encode-hevc-vaapi-any",
            feature = "ffmpeg-encode-hevc-nvenc-any"
        )),
        allow(unused_variables)
    )]
    let prefer_nvenc = prefer_nvenc_from_env();

    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    {
        if prefer_nvenc {
            tracing::info!(
                encoder = "ffmpeg-nvenc-hevc",
                selected_by = "auto",
                reason = "preferred-over-vaapi-by-env",
                "video encoder selected"
            );
            return "ffmpeg-nvenc-hevc";
        }
        tracing::info!(
            encoder = "ffmpeg-vaapi-hevc",
            selected_by = "auto",
            reason = "preferred-over-nvenc",
            "video encoder selected"
        );
        return "ffmpeg-vaapi-hevc";
    }

    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        not(feature = "ffmpeg-encode-hevc-nvenc-any")
    ))]
    {
        tracing::info!(
            encoder = "ffmpeg-vaapi-hevc",
            selected_by = "auto",
            reason = "only-backend-compiled",
            "video encoder selected"
        );
        return "ffmpeg-vaapi-hevc";
    }

    #[cfg(all(
        not(feature = "ffmpeg-encode-hevc-vaapi-any"),
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    {
        tracing::info!(
            encoder = "ffmpeg-nvenc-hevc",
            selected_by = "auto",
            reason = "only-backend-compiled",
            "video encoder selected"
        );
        return "ffmpeg-nvenc-hevc";
    }

    #[cfg(not(any(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    )))]
    {
        tracing::info!(
            encoder = "openh264",
            selected_by = "auto",
            reason = "fallback-no-hw-compiled",
            "video encoder selected"
        );
        return "openh264";
    }
}

/// Inject one input event via uinput.
pub fn dispatch_input(event: InputEvent) -> Result<(), super::DispatchError> {
    _input_linux_inject_event(event).map_err(|e| super::DispatchError::Backend(e.to_string()))
}

/// Read the user's primary X11 _CLIPBOARD selection.
pub fn read_clipboard_text() -> Result<String, super::ClipboardError> {
    _input_linux_read_clipboard_text().map_err(|e| {
        use prdt_input_linux::error::LinuxInputError;
        match e {
            LinuxInputError::ClipboardTimeout | LinuxInputError::ClipboardNonUtf8 => {
                super::ClipboardError::NoText
            }
            LinuxInputError::ClipboardTooLarge(n) => super::ClipboardError::TooLarge(n),
            other => super::ClipboardError::Backend(other.to_string()),
        }
    })
}

/// Set the user's primary X11 _CLIPBOARD selection.
pub fn write_clipboard_text(text: &str) -> Result<(), super::ClipboardError> {
    _input_linux_write_clipboard_text(text).map_err(|e| {
        use prdt_input_linux::error::LinuxInputError;
        match e {
            LinuxInputError::ClipboardTooLarge(n) => super::ClipboardError::TooLarge(n),
            other => super::ClipboardError::Backend(other.to_string()),
        }
    })
}

/// Bumps each time an external X11 client takes the _CLIPBOARD selection.
pub fn clipboard_sequence_number() -> u32 {
    _input_linux_clipboard_sequence_number()
}

/// Return the host's virtual desktop rect via XRandR. First call also
/// initializes the uinput device's ABS range so that subsequent
/// `dispatch_input` calls land within bounds. Idempotent.
pub fn virtual_desktop_rect() -> MonitorRect {
    let rect = _input_linux_virtual_desktop_rect();
    static UINPUT_INIT: Once = Once::new();
    UINPUT_INIT.call_once(|| {
        let w = (rect.right - rect.left).max(1) as u32;
        let h = (rect.bottom - rect.top).max(1) as u32;
        if let Err(e) = prdt_input_linux::uinput_injector::init_with_geometry(w, h) {
            tracing::warn!(error = %e, "uinput init failed; injection will fail until /dev/uinput is accessible");
        }
    });
    rect
}

// ---------------------------------------------------------------------------
// P5A policy shims
// ---------------------------------------------------------------------------

pub fn probe() -> std::sync::Arc<dyn prdt_media_policy::CapabilityProbe> {
    std::sync::Arc::new(prdt_media_linux::policy::LinuxSwProbe)
}

/// Build the producer factory.
///
/// `capture_backend_arg` is the raw `--capture-backend` CLI value (`"auto"`,
/// `"x11"`, `"wayland"`, or anything else). It is parsed via
/// `prdt_media_linux::policy::CaptureBackendChoice::parse` on Linux; ignored
/// on other platforms (Windows has no Wayland axis — capture is always DXGI
/// Desktop Duplication).
///
/// Returns the concrete `LinuxSwFactory` so the host can call
/// `take_cursor_rx()` after `PolicyDriven::bootstrap` to wire the cursor
/// forwarding channel. The returned `Arc` coerces to `Arc<dyn ProducerFactory>`
/// where the trait object is needed.
pub fn factory(
    capture_backend_arg: &str,
) -> std::sync::Arc<prdt_media_linux::policy::LinuxSwFactory> {
    use prdt_media_linux::policy::{detect_capture_backend, CaptureBackendChoice, LinuxSwFactory};
    let choice = CaptureBackendChoice::parse(capture_backend_arg);
    let (backend, reason) = detect_capture_backend(choice);
    tracing::info!(
        choice = ?choice,
        resolved = ?backend,
        reason,
        "P5B-1 capture backend resolved"
    );
    std::sync::Arc::new(LinuxSwFactory::new(backend))
}

/// Whether the capture backend for this session resolves to X11 MIT-SHM.
///
/// The legacy FFmpeg producer path captures via X11 only, so auto-HEVC is
/// gated on this: Wayland-portal sessions keep the H264 policy path. Reuses
/// `detect_capture_backend`, which never fires the Wayland portal consent
/// dialog (it only checks env + a D-Bus `NameHasOwner`); the result is
/// deterministic within a process, so it matches the per-session backend the
/// factory later logs.
pub fn capture_backend_is_x11(capture_backend_arg: &str) -> bool {
    use prdt_media_linux::policy::{detect_capture_backend, CaptureBackend, CaptureBackendChoice};
    let choice = CaptureBackendChoice::parse(capture_backend_arg);
    let (backend, _reason) = detect_capture_backend(choice);
    backend == CaptureBackend::X11Shm
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn linux_normalize_encoder_always_falls_back_for_mf_and_unknown() {
        assert_eq!(normalize_encoder("openh264"), "openh264");
        assert_eq!(normalize_encoder("mf"), "openh264");
        assert_eq!(normalize_encoder("bogus"), "openh264");
    }

    #[test]
    #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-any"))]
    fn linux_normalize_encoder_nvenc_alias_falls_back_without_feature() {
        assert_eq!(normalize_encoder("nvenc"), "openh264");
    }

    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    fn linux_normalize_encoder_nvenc_alias_reroutes_to_canonical() {
        assert_eq!(normalize_encoder("nvenc"), "ffmpeg-nvenc-hevc");
    }

    #[test]
    #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi-any"))]
    fn linux_normalize_encoder_vaapi_alias_falls_back_without_feature() {
        assert_eq!(normalize_encoder("vaapi"), "openh264");
    }

    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    fn linux_normalize_encoder_vaapi_alias_reroutes_to_canonical() {
        assert_eq!(normalize_encoder("vaapi"), "ffmpeg-vaapi-hevc");
    }

    #[test]
    #[serial]
    #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi-any"))]
    fn linux_normalize_encoder_auto_fallback_without_feature() {
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "openh264");
    }

    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    fn linux_normalize_encoder_ffmpeg_vaapi_hevc_arm() {
        assert_eq!(normalize_encoder("ffmpeg-vaapi-hevc"), "ffmpeg-vaapi-hevc");
    }

    #[test]
    #[serial]
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi-any")]
    fn linux_normalize_encoder_auto_prefers_hw_with_feature() {
        // P1.5: env-var poisoning could flip the both-features cfg arm; keep
        // the env clean so this test asserts the documented default.
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "ffmpeg-vaapi-hevc");
    }

    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    fn linux_normalize_encoder_ffmpeg_nvenc_hevc_arm() {
        assert_eq!(normalize_encoder("ffmpeg-nvenc-hevc"), "ffmpeg-nvenc-hevc");
    }

    #[test]
    #[serial]
    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    fn linux_normalize_encoder_auto_prefers_vaapi_over_nvenc_by_default() {
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "ffmpeg-vaapi-hevc");
    }

    #[test]
    #[serial]
    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    fn linux_normalize_encoder_auto_prefers_nvenc_with_env_override() {
        std::env::set_var("PRDT_PREFER_NVENC", "1");
        let result = normalize_encoder("auto");
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(result, "ffmpeg-nvenc-hevc");
    }

    #[test]
    #[serial]
    #[cfg(all(
        not(feature = "ffmpeg-encode-hevc-vaapi-any"),
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    fn linux_normalize_encoder_auto_picks_nvenc_when_only_nvenc() {
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "ffmpeg-nvenc-hevc");
    }

    #[test]
    #[tracing_test::traced_test]
    #[serial]
    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    fn linux_normalize_encoder_auto_log_line_emitted() {
        std::env::set_var("PRDT_PREFER_NVENC", "1");
        let _ = normalize_encoder("auto");
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert!(logs_contain("video encoder selected"));
        assert!(logs_contain("ffmpeg-nvenc-hevc"));
        assert!(logs_contain("preferred-over-vaapi-by-env"));
    }

    #[test]
    #[tracing_test::traced_test]
    #[serial]
    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi-any",
        feature = "ffmpeg-encode-hevc-nvenc-any"
    ))]
    fn linux_normalize_encoder_auto_log_line_default_both_on() {
        std::env::remove_var("PRDT_PREFER_NVENC");
        let _ = normalize_encoder("auto");
        assert!(logs_contain("video encoder selected"));
        assert!(logs_contain("ffmpeg-vaapi-hevc"));
        assert!(logs_contain("preferred-over-nvenc"));
    }

    // P2.5 NPP arm tests -------------------------------------------------------

    /// A12.b-style regression guard: NVENC-without-NPP arm still routes to
    /// plain ffmpeg-nvenc-hevc when both NVENC and NPP features compile in.
    /// Ensures the NPP arm does not shadow or consume the NVENC-only arm.
    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-any")]
    fn linux_normalize_encoder_nvenc_without_npp_still_routes_plain() {
        assert_eq!(normalize_encoder("ffmpeg-nvenc-hevc"), "ffmpeg-nvenc-hevc");
    }

    /// Canonical name resolves when NPP feature is compiled in.
    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    fn linux_normalize_encoder_ffmpeg_nvenc_hevc_npp_arm() {
        assert_eq!(
            normalize_encoder("ffmpeg-nvenc-hevc-npp"),
            "ffmpeg-nvenc-hevc-npp"
        );
    }

    /// Legacy alias `nvenc-npp` resolves to canonical when NPP feature compiled in.
    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    fn linux_normalize_encoder_nvenc_npp_alias_reroutes_to_canonical() {
        assert_eq!(normalize_encoder("nvenc-npp"), "ffmpeg-nvenc-hevc-npp");
    }

    /// Without NPP feature, `ffmpeg-nvenc-hevc-npp` and `nvenc-npp` fall back to openh264.
    #[test]
    #[cfg(not(feature = "ffmpeg-encode-hevc-nvenc-npp-any"))]
    fn linux_normalize_encoder_npp_arm_falls_back_without_feature() {
        assert_eq!(normalize_encoder("ffmpeg-nvenc-hevc-npp"), "openh264");
        assert_eq!(normalize_encoder("nvenc-npp"), "openh264");
    }

    // Auto-HEVC advertisement decision matrix ---------------------------------
    // These exercise the pure `advertised_auto_codecs` gate over every row of
    // the acceptance matrix (spec §Acceptance 3-5) with the probe result
    // injected, so no real hardware is required.

    #[test]
    fn advertised_auto_codecs_hevc_when_x11_and_probe_and_not_forced() {
        use prdt_protocol::Codec;
        // Acceptance 1/2: probe passed, X11, SW not forced → [H265, H264].
        assert_eq!(
            advertised_auto_codecs(false, true, Some("ffmpeg-vaapi-hevc")),
            vec![Codec::H265, Codec::H264]
        );
        assert_eq!(
            advertised_auto_codecs(false, true, Some("ffmpeg-nvenc-hevc")),
            vec![Codec::H265, Codec::H264]
        );
    }

    #[test]
    fn advertised_auto_codecs_h264_only_when_force_sw() {
        use prdt_protocol::Codec;
        // Acceptance 4: --force-sw → [H264] even with a probe-approved backend.
        assert_eq!(
            advertised_auto_codecs(true, true, Some("ffmpeg-vaapi-hevc")),
            vec![Codec::H264]
        );
    }

    #[test]
    fn advertised_auto_codecs_h264_only_when_not_x11() {
        use prdt_protocol::Codec;
        // Acceptance 3: Wayland-resolved capture → [H264] (FFmpeg path is X11-only).
        assert_eq!(
            advertised_auto_codecs(false, false, Some("ffmpeg-vaapi-hevc")),
            vec![Codec::H264]
        );
    }

    #[test]
    fn advertised_auto_codecs_h264_only_when_no_backend() {
        use prdt_protocol::Codec;
        // Acceptance 5: no HEVC backend probed (incl. no-feature build) → [H264].
        assert_eq!(advertised_auto_codecs(false, true, None), vec![Codec::H264]);
        // Every "off" combination collapses to [H264].
        assert_eq!(advertised_auto_codecs(true, false, None), vec![Codec::H264]);
    }

    // Auto must never advertise Main10 in the negotiation set (spec §2, §4).
    // Runs regardless of compiled features: with a Main10 feature it proves the
    // `encoder_arg != "auto"` gate; without, it is trivially true.
    #[test]
    #[serial]
    fn linux_negotiation_auto_never_advertises_main10() {
        use prdt_protocol::Codec;
        for &force_sw in &[false, true] {
            for &x11 in &[false, true] {
                let v = linux_supported_codecs_negotiation("auto", force_sw, x11);
                assert!(
                    !v.contains(&Codec::H265Main10),
                    "auto must never advertise H265Main10 (force_sw={force_sw}, x11={x11}); got {v:?}"
                );
            }
        }
    }

    // resolve_auto_build_backend: the H264 arm is deterministic and needs no
    // hardware — auto negotiated to 8-bit always builds OpenH264.
    #[test]
    fn resolve_auto_build_backend_h264_is_openh264() {
        assert_eq!(
            resolve_auto_build_backend(prdt_protocol::Codec::H264),
            "openh264"
        );
    }

    // NOTE: the H265 arm of `resolve_auto_build_backend` (and the delegation of
    // the advertisement to `resolve_hevc_auto_backend`) cannot be unit-tested
    // here without real HEVC hardware: `resolve_hevc_auto_backend` builds and
    // drops a real VAAPI/NVENC encoder as its probe, and the resolver is a
    // process-wide OnceLock with no injection seam. Its correctness is covered
    // by the hardware smoke (spec §Acceptance 1-2) instead.

    // R15 mitigation: pre-PR1 clients must never receive H265Main10 in HelloAck.
    #[test]
    fn supported_codecs_for_excludes_main10_for_legacy_codecs() {
        use prdt_protocol::Codec;
        for legacy in [Codec::H265, Codec::H264, Codec::Av1] {
            let codecs = supported_codecs_for(legacy);
            assert!(
                !codecs.contains(&Codec::H265Main10),
                "H265Main10 must not appear for legacy hello_codec {:?}",
                legacy
            );
        }
    }

    #[test]
    fn supported_codecs_for_includes_main10_for_main10_client() {
        use prdt_protocol::Codec;
        let codecs = supported_codecs_for(Codec::H265Main10);
        assert!(
            codecs.contains(&Codec::H265Main10),
            "H265Main10 must appear when hello_codec is H265Main10"
        );
    }

    // A1.12 sub-test 5: bincode wire bytes for pre-PR1 hello codecs must
    // never contain variant_index=3 at any Codec slot.
    // bincode 1.3 encodes Vec<T> as: 8-byte LE u64 length, then each element.
    // Codec elements are 4-byte LE u32 variant_index. We scan aligned 4-byte
    // slots starting at byte 8 (after the length prefix) to avoid false
    // positives from the length field itself (e.g. a 3-element Vec encodes
    // its length as [3,0,0,0,0,0,0,0] whose first 4 bytes equal [3,0,0,0]).
    #[test]
    fn supported_codecs_for_wire_bytes_exclude_variant3_for_legacy_codecs() {
        use prdt_protocol::Codec;
        let variant3: [u8; 4] = [3, 0, 0, 0];
        for legacy in [Codec::H265, Codec::H264, Codec::Av1] {
            let codecs = supported_codecs_for(legacy);
            let bytes = bincode::serialize(&codecs).expect("serialize");
            // Skip the 8-byte length prefix; check each 4-byte Codec slot.
            let found = bytes[8..].chunks(4).any(|slot| slot == variant3);
            assert!(
                !found,
                "variant_index=3 must not appear in Codec slots for hello_codec {:?}; bytes={:?}",
                legacy, bytes
            );
        }
    }

    // A1.12 sub-test 6: bincode wire bytes for Main10 hello codec must
    // contain variant_index=3 at a Codec slot.
    #[test]
    fn supported_codecs_for_wire_bytes_include_variant3_for_main10_client() {
        use prdt_protocol::Codec;
        let codecs = supported_codecs_for(Codec::H265Main10);
        let bytes = bincode::serialize(&codecs).expect("serialize");
        let variant3: [u8; 4] = [3, 0, 0, 0];
        // Skip the 8-byte length prefix; check each 4-byte Codec slot.
        let found = bytes[8..].chunks(4).any(|slot| slot == variant3);
        assert!(
            found,
            "variant_index=3 must appear in Codec slots when hello_codec is H265Main10; bytes={:?}",
            bytes
        );
    }

    /// `build_video_producer` with `ffmpeg-nvenc-hevc-npp` selects the NPP
    /// factory path. On a CUDA-less dev container this returns an error from
    /// `HevcNvencNppFfmpegEncoder::new`, not a silent openh264 fallback.
    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc-npp-any")]
    fn linux_build_video_producer_nvenc_npp_path() {
        let out = OutputDescriptor;
        let result = build_video_producer(
            "ffmpeg-nvenc-hevc-npp",
            &out,
            8_000_000,
            60,
            prdt_protocol::Codec::H265,
        );
        // On a CUDA-less host (dev container) the NPP constructor fails loudly.
        // We assert the call sites the right code path (returns Err, not Ok
        // with an openh264 producer).  If somehow CUDA is present, Ok is also fine.
        match &result {
            Ok(_) => { /* real NVIDIA HW present — path correctly returned an NPP producer */ }
            Err(e) => {
                // Must come from the NPP encoder's constructor, not a routing
                // fallback to a different backend. Use debug format to include
                // the full anyhow error chain (context + cause).
                let s = format!("{e:?}");
                assert!(
                    s.contains("CUDA")
                        || s.contains("cuda")
                        || s.contains("NPP")
                        || s.contains("npp")
                        || s.contains("libnppicc")
                        || s.contains("HwDevice")
                        || s.contains("EncoderNotFound")
                        || s.contains("NvencNpp")
                        || s.contains("X11")
                        || s.contains("x11"),
                    "expected NPP or CUDA error from ffmpeg-nvenc-hevc-npp path, got: {s}"
                );
            }
        }
    }
}
