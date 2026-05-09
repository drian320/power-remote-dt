//! Platform-specific viewer backends. cfg-aliased re-exports give lib.rs
//! a single OS-transparent symbol set. Mirrors crates/host/src/platform/mod.rs.

use thiserror::Error;

#[allow(dead_code)] // DeviceLost is only constructed on Windows
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("renderer init failed: {0}")]
    Init(String),
    #[error("renderer present failed: {0}")]
    Present(String),
    #[error("renderer device lost (unrecoverable; restart required): {0}")]
    DeviceLost(String),
}

#[allow(dead_code)] // Decode is reserved for future codec-error mapping
#[derive(Debug, Error)]
pub enum ConsumerError {
    #[error("decoder error: {0}")]
    Decode(String),
    #[error("decoder backend init failed: {0}")]
    Init(String),
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

pub mod input_map;
pub use input_map::{map_winit_mouse_button, physical_key_to_scancode};

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win::{
    build_consumer, build_render, clipboard_sequence_number, present_frame, read_clipboard_text,
    resize_renderer, virtual_desktop_rect, write_clipboard_text,
    PlatformConsumer, PlatformFrame, PlatformRender, MAX_CLIPBOARD_BYTES,
};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
#[allow(unused_imports)] // virtual_desktop_rect is reserved for L2 multi-monitor work
pub use linux::{
    build_consumer, build_render, clipboard_sequence_number, present_frame, read_clipboard_text,
    resize_renderer, virtual_desktop_rect, write_clipboard_text,
    PlatformConsumer, PlatformFrame, PlatformRender, MAX_CLIPBOARD_BYTES,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_error_variants_format_correctly() {
        assert_eq!(
            RenderError::Init("softbuffer down".into()).to_string(),
            "renderer init failed: softbuffer down",
        );
        assert_eq!(
            RenderError::Present("present hung".into()).to_string(),
            "renderer present failed: present hung",
        );
        assert_eq!(
            RenderError::DeviceLost("DXGI removed".into()).to_string(),
            "renderer device lost (unrecoverable; restart required): DXGI removed",
        );
    }

    #[test]
    fn consumer_error_variants_format_correctly() {
        assert_eq!(
            ConsumerError::Decode("nal junk".into()).to_string(),
            "decoder error: nal junk",
        );
        assert_eq!(
            ConsumerError::Init("openh264 missing".into()).to_string(),
            "decoder backend init failed: openh264 missing",
        );
    }

    #[test]
    fn clipboard_error_variants_match_l0() {
        assert_eq!(
            ClipboardError::NoText.to_string(),
            "no text content available",
        );
        assert_eq!(
            ClipboardError::TooLarge(70_000).to_string(),
            "clipboard payload too large: 70000 bytes",
        );
        assert_eq!(
            ClipboardError::Backend("xfixes drop".into()).to_string(),
            "clipboard backend error: xfixes drop",
        );
    }
}
