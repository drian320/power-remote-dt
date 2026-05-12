//! Wayland portal capture backend stub — scaffolding for T6.
//!
//! `WaylandPortalCapturer` satisfies the `CaptureSource` trait so that the
//! factory can reference the type now; actual PipeWire frame pulling is wired
//! up in T6. Until then every method returns a sentinel value or error.

#![cfg(target_os = "linux")]

use crate::capture_source::{CaptureSource, CaptureSourceError};
use thiserror::Error;

/// Errors produced when constructing a `WaylandPortalCapturer`.
#[derive(Debug, Error)]
pub enum WaylandPortalCapturerInitError {
    /// PipeWire frame pulling is not yet implemented (arrives in T6).
    #[error("wayland portal capturer not yet implemented")]
    NotImplemented,
}

/// Capture backend stub for the Wayland XDG ScreenCast portal.
///
/// This type exists so the factory layer can reference it during P5B-1.
/// Frame capture is implemented in T6 (PipeWire integration).
pub struct WaylandPortalCapturer {
    _todo: (),
}

impl WaylandPortalCapturer {
    /// Attempt to construct a `WaylandPortalCapturer`.
    ///
    /// Always returns `Err(NotImplemented)` until T6.
    pub fn new() -> Result<Self, WaylandPortalCapturerInitError> {
        Err(WaylandPortalCapturerInitError::NotImplemented)
    }
}

impl CaptureSource for WaylandPortalCapturer {
    fn geometry(&self) -> (u32, u32) {
        // Stub — real geometry comes from the PipeWire stream in T6.
        (1, 1)
    }

    fn capture_into(&mut self, _out: &mut Vec<u8>) -> Result<(), CaptureSourceError> {
        // Stub — real capture is wired up in T6.
        Err(CaptureSourceError::WouldBlock(
            "wayland portal capturer not yet implemented".into(),
        ))
    }
}
