//! Linux media backend — XShm capture + OpenH264 SW encode/decode +
//! VideoProducer adapter. See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.
//!
//! The crate compiles to an empty library on non-Linux targets.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod capture_source;
pub mod core_adapter;
pub mod error;
pub mod frame;
pub mod i420_to_bgra;
pub mod linux_sw_producer;
pub mod policy;
pub mod sw_pipeline;
pub mod vaapi_pipeline;
pub mod wayland_portal;
pub mod x11_capture;

pub use capture_source::{CaptureSource, CaptureSourceError};
pub use error::LinuxMediaError;
pub use frame::BgraFrame;
pub use vaapi_pipeline::{LinuxVaapiEncoder, VaapiVideoProducer};

/// Production wiring entry point — host calls this to obtain a boxed
/// `VideoProducer` for the Linux SW path. The capture source is injected
/// (the factory picks X11 or Wayland-portal); width/height come from the
/// capture source via `geometry()`.
#[cfg(target_os = "linux")]
pub fn build_video_producer_with(
    capture: Box<dyn CaptureSource>,
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let (w, h) = capture.geometry();
    let enc =
        sw_pipeline::LinuxSwEncoder::new(w, h, bitrate_bps, fps).context("LinuxSwEncoder::new")?;
    linux_sw_producer::LinuxSwProducer::new(capture, enc, fps).context("LinuxSwProducer::new")
}

/// Legacy entry point — X11-only convenience wrapper retained so callers
/// that still want X11 explicitly (smoke tests, the
/// `build_video_decoder`-paired helper) don't have to assemble the
/// `Box<dyn CaptureSource>` themselves. Internally equivalent to
/// `build_video_producer_with(Box::new(X11ShmCapturer::new()?), ...)`.
#[cfg(target_os = "linux")]
pub fn build_video_producer(
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let cap = x11_capture::X11ShmCapturer::new().context("X11ShmCapturer::new")?;
    build_video_producer_with(Box::new(cap), bitrate_bps, fps)
}

/// VAAPI counterpart to `build_video_producer_with`. Constructs a
/// `LinuxVaapiEncoder` from the capture's geometry + the given bitrate/fps
/// and returns a `VaapiVideoProducer`. The encoder ctor (cros-libva
/// `Display::open_drm_display` + cap probe) is what fails in a container
/// without `/dev/dri/*` — the factory propagates that as `Unavailable`.
#[cfg(target_os = "linux")]
pub fn build_vaapi_video_producer_with(
    capture: Box<dyn CaptureSource>,
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<vaapi_pipeline::VaapiVideoProducer> {
    use anyhow::Context as _;
    let (w, h) = capture.geometry();
    let enc = vaapi_pipeline::LinuxVaapiEncoder::new(w, h, bitrate_bps, fps)
        .context("LinuxVaapiEncoder::new")?;
    vaapi_pipeline::VaapiVideoProducer::new(capture, enc, fps).context("VaapiVideoProducer::new")
}

/// Production wiring entry point — viewer calls this to obtain a SW decoder.
#[cfg(target_os = "linux")]
pub fn build_video_decoder() -> anyhow::Result<sw_pipeline::LinuxSwDecoder> {
    sw_pipeline::LinuxSwDecoder::new().map_err(Into::into)
}
