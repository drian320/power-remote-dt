//! Event-free availability probes for the HEVC hardware encoders.
//!
//! [`crate::hevc_vaapi_encoder::HevcVaapiFfmpegEncoder::new`] and
//! [`crate::hevc_nvenc_encoder::HevcNvencFfmpegEncoder::new`] both emit the
//! `target: "video.pipeline", event = "encoder_ready"` tracing event as their
//! final construction step. That event is meant to mark the *real* encoder a
//! session will stream through — but callers that only want to know "is this
//! backend usable on this host" (the host's `--encoder auto` availability
//! probe) previously called `::new` directly, which meant a probe-then-build
//! auto session logged `encoder_ready` twice: once for the probe's throwaway
//! encoder (gop=30, the probe's tiny config) and once for the real one
//! (gop=fps, the session's actual config). That broke the documented smoke
//! assertion `grep -c encoder_ready == 1`.
//!
//! These helpers go through the same `new_inner` construction path (HW device
//! open, codec open, BSF init where applicable, frame allocation) with
//! `emit_ready_event = false`, so probing has the identical pass/fail
//! semantics as `::new` minus the log line, then drop the encoder immediately
//! to release the device/context.

use crate::error::FfmpegError;

/// Tiny config shared by both probes — matches what the host's auto-HEVC
/// resolver has always used for probing (320x180 @ 30fps, 1 Mbps, gop 30).
const PROBE_WIDTH: u32 = 320;
const PROBE_HEIGHT: u32 = 180;
const PROBE_FPS: u32 = 30;
const PROBE_BITRATE_BPS: u32 = 1_000_000;
const PROBE_GOP_SIZE: u32 = 30;

/// Probe VAAPI HEVC encoder availability by constructing a tiny throwaway
/// encoder (proving HW frames context + `avcodec_open2` succeed — not merely
/// device node presence) and dropping it immediately. Does NOT emit
/// `encoder_ready`.
#[cfg(all(feature = "ffmpeg-encode-hevc-vaapi-any", target_os = "linux"))]
pub fn probe_hevc_vaapi(render_node: Option<&std::path::Path>) -> Result<(), FfmpegError> {
    use crate::hevc_vaapi_encoder::{HevcVaapiFfmpegEncoder, HevcVaapiFfmpegEncoderConfig};

    let cfg = HevcVaapiFfmpegEncoderConfig {
        width: PROBE_WIDTH,
        height: PROBE_HEIGHT,
        fps: PROBE_FPS,
        initial_bitrate_bps: PROBE_BITRATE_BPS,
        gop_size: PROBE_GOP_SIZE,
        render_node: render_node.map(std::path::Path::to_path_buf),
    };
    HevcVaapiFfmpegEncoder::new_inner(cfg, false).map(drop)
}

/// Probe NVENC HEVC encoder availability by constructing a tiny throwaway
/// encoder (proving CUDA init + `avcodec_open2` succeed) and dropping it
/// immediately. Does NOT emit `encoder_ready`.
#[cfg(all(feature = "ffmpeg-encode-hevc-nvenc-any", target_os = "linux"))]
pub fn probe_hevc_nvenc() -> Result<(), FfmpegError> {
    use crate::hevc_nvenc_encoder::{HevcNvencFfmpegEncoder, HevcNvencFfmpegEncoderConfig};

    let cfg = HevcNvencFfmpegEncoderConfig {
        width: PROBE_WIDTH,
        height: PROBE_HEIGHT,
        fps: PROBE_FPS,
        initial_bitrate_bps: PROBE_BITRATE_BPS,
        gop_size: PROBE_GOP_SIZE,
        cuda_device_index: None,
    };
    HevcNvencFfmpegEncoder::new_inner(cfg, false).map(drop)
}
