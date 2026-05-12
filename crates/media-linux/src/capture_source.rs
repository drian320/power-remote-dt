//! Capture-source abstraction shared by the X11 and Wayland-portal backends.
//!
//! `LinuxSwProducer` holds a `Box<dyn CaptureSource>`; the concrete impl is
//! picked at construction time by `LinuxSwFactory` based on the resolved
//! `CaptureBackend` (see `policy.rs`).
//!
//! The trait is deliberately small so the producer doesn't have to know
//! whether it owns an X11 SHM segment or a PipeWire stream. Errors are
//! surfaced via a shared `CaptureSourceError`; both backends map their
//! internal errors into this enum.

#![cfg(target_os = "linux")]

use thiserror::Error;

/// Error type for capture-source operations. Variants are deliberately
/// coarse: terminal failures should surface as `Terminal { backend, reason }`
/// (which the producer maps to `ProducerError::Capture`), while transient
/// "no frame yet" conditions surface as `WouldBlock` so the producer can
/// tick once and retry on the next pacer beat.
#[derive(Debug, Error)]
pub enum CaptureSourceError {
    /// No frame was available in the configured wait window.
    ///
    /// **Current handling** (P5B-1): the producer surfaces this as
    /// `ProducerError::Capture("would_block: <reason>")`, which the session loop
    /// treats the same as any other capture failure.  Future work (P5B-2 or
    /// later) may distinguish this from `Terminal` and tick-and-retry instead of
    /// failing the session; until then `WouldBlock` and `Terminal` are
    /// behaviourally identical at the producer level — backends should use
    /// `WouldBlock` only when the condition is genuinely transient (e.g. a
    /// PipeWire empty-queue wakeup) so the future tick-and-retry wiring lands
    /// without ambiguity.
    ///
    /// Carries a short reason string for log triage.
    #[error("would block: {0}")]
    WouldBlock(String),

    /// Permanent failure — capture cannot continue. Wraps the backend
    /// name so the producer can attribute it cleanly.
    #[error("capture terminal on {backend}: {reason}")]
    Terminal {
        backend: &'static str,
        reason: String,
    },
}

/// Common interface implemented by every Linux capture backend.
///
/// `geometry()` is exposed per-call (not stored once at construction) so the
/// Wayland portal can report a mid-session resize when the user resizes the
/// captured monitor. The X11 path returns a fixed value (root window
/// geometry is read once in `X11ShmCapturer::new`).
pub trait CaptureSource: Send {
    /// Return the (width, height) the next call to `capture_into` will fill,
    /// in pixels. Must be ≥ 1×1.
    fn geometry(&self) -> (u32, u32);

    /// Block until a new frame is available, then resize `out` to
    /// `geometry().0 * geometry().1 * 4` bytes (or larger if the backend
    /// uses padding) and fill it with BGRA / BGRx data.
    ///
    /// Returns `Err(WouldBlock)` for transient empty-frame conditions
    /// (producer converts to a tick) and `Err(Terminal)` for permanent
    /// failures (producer surfaces as `ProducerError::Capture`).
    fn capture_into(&mut self, out: &mut Vec<u8>) -> Result<(), CaptureSourceError>;
}
