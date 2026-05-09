//! Linux media backend — XShm capture + OpenH264 SW encode/decode +
//! VideoProducer adapter. See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.
//!
//! The crate compiles to an empty library on non-Linux targets.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod error;
pub mod frame;
pub mod x11_capture;
pub mod sw_pipeline;
pub mod i420_to_bgra;
pub mod linux_sw_producer;
// Subsequent tasks will add:
//   pub mod core_adapter;

pub use error::LinuxMediaError;
pub use frame::BgraFrame;

/// Production wiring entry point — host calls this to obtain a boxed
/// `VideoProducer` for the Linux SW path. Width/height come from the
/// X server; the host need only pass bitrate + fps (and the
/// `--encoder` flag selection, currently always SW on Linux).
#[cfg(target_os = "linux")]
pub fn build_video_producer(
    bitrate_bps: u32,
    fps: u32,
) -> anyhow::Result<linux_sw_producer::LinuxSwProducer> {
    use anyhow::Context as _;
    let cap = x11_capture::X11ShmCapturer::new().context("X11ShmCapturer::new")?;
    let enc = sw_pipeline::LinuxSwEncoder::new(cap.width(), cap.height(), bitrate_bps, fps)
        .context("LinuxSwEncoder::new")?;
    linux_sw_producer::LinuxSwProducer::new(cap, enc, fps).context("LinuxSwProducer::new")
}

/// Production wiring entry point — viewer calls this to obtain a SW
/// decoder.
#[cfg(target_os = "linux")]
pub fn build_video_decoder() -> anyhow::Result<sw_pipeline::LinuxSwDecoder> {
    sw_pipeline::LinuxSwDecoder::new().map_err(Into::into)
}
