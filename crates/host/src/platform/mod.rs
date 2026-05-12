//! Platform-specific host backends. The cfg-aliased re-exports below give
//! `lib.rs` a single, OS-transparent symbol set. See spec §5.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("input dispatch backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard backend error: {0}")]
    Backend(String),
    #[error("no text content available")]
    NoText,
    #[error("clipboard payload too large: {0} bytes")]
    TooLarge(usize),
}

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{
    build_video_producer, clipboard_sequence_number, dispatch_input, output_display_name,
    pick_default_output, read_clipboard_text, virtual_desktop_rect, write_clipboard_text,
    MAX_CLIPBOARD_BYTES,
};
// OutputDescriptor is re-exported for downstream callers that need to name
// the type explicitly (e.g. GUI-host wrappers). lib.rs uses it opaquely.
#[cfg(windows)]
pub use win::OutputDescriptor;
// Internal Windows-only types still used by lib.rs (e.g. tests). Removed in T7.
#[cfg(windows)]
pub use win::{DxgiSwProducer, VideoEncoderBackend};
// P5A policy shims.
#[cfg(windows)]
pub use win::{factory, probe};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    build_video_producer, clipboard_sequence_number, dispatch_input, output_display_name,
    pick_default_output, read_clipboard_text, virtual_desktop_rect, write_clipboard_text,
    MAX_CLIPBOARD_BYTES,
};
// P5A policy shims.
// Note: on Linux, `factory` returns `Arc<LinuxSwFactory>` (concrete) so the
// host can call `take_cursor_rx()` after bootstrap. The type coerces to
// `Arc<dyn ProducerFactory>` at the `PolicyDriven::bootstrap` call site.
#[cfg(target_os = "linux")]
pub use linux::{factory, probe};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_error_round_trip() {
        let e = DispatchError::Backend("uinput closed".into());
        assert_eq!(e.to_string(), "input dispatch backend error: uinput closed");
    }

    #[test]
    fn clipboard_error_variants_match_l0() {
        assert_eq!(
            ClipboardError::NoText.to_string(),
            "no text content available"
        );
        assert_eq!(
            ClipboardError::TooLarge(70_000).to_string(),
            "clipboard payload too large: 70000 bytes"
        );
        assert_eq!(
            ClipboardError::Backend("xfixes drop".into()).to_string(),
            "clipboard backend error: xfixes drop"
        );
    }
}
