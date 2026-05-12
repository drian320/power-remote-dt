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

/// Build a boxed `VideoProducer` for the Linux SW path. Args from the
/// CLI that name HW backends are normalized to openh264 with a warn-log.
pub fn build_video_producer(
    args_encoder: &str,
    _output: &OutputDescriptor,
    bitrate_bps: u32,
    fps: u32,
    _negotiated_codec: prdt_protocol::Codec,
) -> anyhow::Result<Box<dyn VideoProducer>> {
    let _ = normalize_encoder(args_encoder); // warn-log if the user passed nvenc/mf
    let producer = prdt_media_linux::build_video_producer(bitrate_bps, fps)?;
    Ok(Box::new(producer))
}

/// Map any encoder CLI arg to "openh264" on Linux; warn-log when the
/// user requested an HW backend that we don't yet support.
fn normalize_encoder(arg: &str) -> &'static str {
    match arg {
        "openh264" | "auto" => "openh264",
        "nvenc" | "mf" => {
            tracing::warn!(
                requested = arg,
                "Linux SW codec only; falling back to openh264"
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

pub fn factory(
    capture_backend_arg: &str,
) -> std::sync::Arc<dyn prdt_media_policy::ProducerFactory> {
    use prdt_media_linux::policy::{detect_capture_backend, CaptureBackendChoice, LinuxSwFactory};
    let choice = CaptureBackendChoice::parse(capture_backend_arg);
    let backend = detect_capture_backend(choice);
    tracing::info!(
        choice = ?choice,
        resolved = ?backend,
        "P5B-1 capture backend resolved"
    );
    std::sync::Arc::new(LinuxSwFactory::new(backend))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_normalize_encoder_falls_back_for_hw() {
        assert_eq!(normalize_encoder("openh264"), "openh264");
        assert_eq!(normalize_encoder("auto"), "openh264");
        assert_eq!(normalize_encoder("nvenc"), "openh264");
        assert_eq!(normalize_encoder("mf"), "openh264");
        assert_eq!(normalize_encoder("bogus"), "openh264");
    }
}
