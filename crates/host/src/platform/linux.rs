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
use std::sync::Once;

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
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
    FfmpegVaapiHevc(prdt_media_ffmpeg::HevcVaapiFfmpegEncoderAdapter),
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
    FfmpegNvencHevc(prdt_media_ffmpeg::HevcNvencFfmpegEncoderAdapter),
}

#[allow(dead_code)]
impl LinuxEncoder {
    pub fn codec(&self) -> prdt_protocol::Codec {
        match self {
            LinuxEncoder::Openh264(_) => prdt_protocol::Codec::H264,
            #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
            LinuxEncoder::FfmpegVaapiHevc(_) => prdt_protocol::Codec::H265,
            #[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
            LinuxEncoder::FfmpegNvencHevc(_) => prdt_protocol::Codec::H265,
        }
    }
}

/// Advertised codecs for the Linux encoder selection (used in host handshake).
pub fn linux_supported_codecs(encoder_arg: &str) -> Vec<prdt_protocol::Codec> {
    match normalize_encoder(encoder_arg) {
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
        "ffmpeg-vaapi-hevc" => vec![prdt_protocol::Codec::H265],
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
        "ffmpeg-nvenc-hevc" => vec![prdt_protocol::Codec::H265],
        _ => vec![prdt_protocol::Codec::H264],
    }
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
    let _backend = normalize_encoder(args_encoder);
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
    if _backend == "ffmpeg-vaapi-hevc" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{
            HevcVaapiFfmpegEncoder, HevcVaapiFfmpegEncoderAdapter, HevcVaapiFfmpegEncoderConfig,
        };
        let cfg = HevcVaapiFfmpegEncoderConfig {
            width: 1920,
            height: 1080,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: fps,
            render_node: None,
        };
        let enc = HevcVaapiFfmpegEncoder::new(cfg).context("HevcVaapiFfmpegEncoder::new")?;
        let adapter = HevcVaapiFfmpegEncoderAdapter(enc);
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg path")?;
        let producer = FfmpegVaapiProducer::new(Box::new(cap), adapter, fps);
        return Ok(Box::new(producer));
    }
    #[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
    if _backend == "ffmpeg-nvenc-hevc" {
        use anyhow::Context as _;
        use prdt_media_ffmpeg::{
            HevcNvencFfmpegEncoder, HevcNvencFfmpegEncoderAdapter, HevcNvencFfmpegEncoderConfig,
        };
        let cfg = HevcNvencFfmpegEncoderConfig {
            width: 1920,
            height: 1080,
            fps,
            initial_bitrate_bps: bitrate_bps,
            gop_size: fps,
            cuda_device_index: None,
        };
        let enc = HevcNvencFfmpegEncoder::new(cfg).context("HevcNvencFfmpegEncoder::new")?;
        let adapter = HevcNvencFfmpegEncoderAdapter(enc);
        let cap = prdt_media_linux::x11_capture::X11ShmCapturer::new()
            .context("X11ShmCapturer::new for ffmpeg path")?;
        let producer = FfmpegNvencProducer::new(Box::new(cap), adapter, fps);
        return Ok(Box::new(producer));
    }
    let producer = prdt_media_linux::build_video_producer(bitrate_bps, fps)?;
    Ok(Box::new(producer))
}

/// `VideoProducer` that wires an X11 BGRA capture source into the
/// FFmpeg VAAPI HEVC encoder. Mirrors `LinuxSwProducer` but substitutes
/// `HevcVaapiFfmpegEncoderAdapter` (takes `I420Frame`) and emits `Codec::H265`.
#[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
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

#[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
impl FfmpegVaapiProducer {
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcVaapiFfmpegEncoderAdapter,
        fps: u32,
    ) -> Self {
        let (width, height) = capture.geometry();
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        }
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
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
#[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
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

#[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
impl FfmpegNvencProducer {
    fn new(
        capture: Box<dyn prdt_media_linux::capture_source::CaptureSource>,
        encoder: prdt_media_ffmpeg::HevcNvencFfmpegEncoderAdapter,
        fps: u32,
    ) -> Self {
        let (width, height) = capture.geometry();
        let micros = if fps == 0 {
            16_667
        } else {
            1_000_000 / fps as u64
        };
        let mut pacer = tokio::time::interval(std::time::Duration::from_micros(micros));
        pacer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            capture: Some(capture),
            encoder: Some(encoder),
            bgra_buf: vec![0u8; (width * height * 4) as usize],
            pacer,
            seq: 0,
            idr_pending: true,
            width,
            height,
            poisoned: false,
        }
    }
}

#[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
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

/// Map any encoder CLI arg to the canonical backend name on Linux.
fn normalize_encoder(arg: &str) -> &'static str {
    match arg {
        "openh264" => "openh264",
        #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
        "ffmpeg-vaapi-hevc" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                "video encoder selected"
            );
            "ffmpeg-vaapi-hevc"
        }
        #[cfg(feature = "ffmpeg-encode-hevc-nvenc")]
        "ffmpeg-nvenc-hevc" => {
            tracing::info!(
                encoder = %arg,
                selected_by = "explicit-flag",
                reason = "user-requested",
                "video encoder selected"
            );
            "ffmpeg-nvenc-hevc"
        }
        "auto" => resolve_auto_encoder(),
        "nvenc" | "mf" | "vaapi" => {
            // Legacy alias arm — unchanged in P1.5. Rerouting `"nvenc"` to
            // the NVENC backend is deferred to P1.6 (separate PR).
            tracing::warn!(
                requested = arg,
                "Linux HW codec only via 'ffmpeg-vaapi-hevc'; falling back to openh264"
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
            feature = "ffmpeg-encode-hevc-vaapi",
            feature = "ffmpeg-encode-hevc-nvenc"
        )),
        allow(unused_variables)
    )]
    let prefer_nvenc = std::env::var("PRDT_PREFER_NVENC")
        .ok()
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);

    #[cfg(all(
        feature = "ffmpeg-encode-hevc-vaapi",
        feature = "ffmpeg-encode-hevc-nvenc"
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
        feature = "ffmpeg-encode-hevc-vaapi",
        not(feature = "ffmpeg-encode-hevc-nvenc")
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
        not(feature = "ffmpeg-encode-hevc-vaapi"),
        feature = "ffmpeg-encode-hevc-nvenc"
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
        feature = "ffmpeg-encode-hevc-vaapi",
        feature = "ffmpeg-encode-hevc-nvenc"
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn linux_normalize_encoder_falls_back_for_hw() {
        assert_eq!(normalize_encoder("openh264"), "openh264");
        assert_eq!(normalize_encoder("nvenc"), "openh264");
        assert_eq!(normalize_encoder("mf"), "openh264");
        assert_eq!(normalize_encoder("vaapi"), "openh264");
        assert_eq!(normalize_encoder("bogus"), "openh264");
    }

    #[test]
    #[serial]
    #[cfg(not(feature = "ffmpeg-encode-hevc-vaapi"))]
    fn linux_normalize_encoder_auto_fallback_without_feature() {
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "openh264");
    }

    #[test]
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
    fn linux_normalize_encoder_ffmpeg_vaapi_hevc_arm() {
        assert_eq!(normalize_encoder("ffmpeg-vaapi-hevc"), "ffmpeg-vaapi-hevc");
    }

    #[test]
    #[serial]
    #[cfg(feature = "ffmpeg-encode-hevc-vaapi")]
    fn linux_normalize_encoder_auto_prefers_hw_with_feature() {
        // P1.5: env-var poisoning could flip the both-features cfg arm; keep
        // the env clean so this test asserts the documented default.
        std::env::remove_var("PRDT_PREFER_NVENC");
        assert_eq!(normalize_encoder("auto"), "ffmpeg-vaapi-hevc");
    }
}
