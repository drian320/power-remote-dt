//! L0 trait wrappers — unit-test surface only. Production wiring uses
//! the free functions from `lib.rs` directly.

use crate::{
    clipboard_sequence_number, inject_event, read_clipboard_text, virtual_desktop_rect,
    write_clipboard_text,
};
use prdt_input_core::{
    ClipboardError, ClipboardProvider, InjectError, InputInjector, VirtualDesktopGeometry,
};
use prdt_protocol::{InputEvent, MonitorRect};

pub struct UinputInjector;

impl UinputInjector {
    pub fn new() -> Result<Self, InjectError> {
        Ok(Self)
    }
}

impl InputInjector for UinputInjector {
    fn inject(&self, event: InputEvent) -> Result<(), InjectError> {
        inject_event(event).map_err(InjectError::from)
    }

    fn backend_name(&self) -> &'static str {
        "linux-uinput"
    }
}

pub struct X11Clipboard;

impl X11Clipboard {
    pub fn new() -> Self {
        Self
    }
}

impl ClipboardProvider for X11Clipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        read_clipboard_text().map_err(ClipboardError::from)
    }

    fn write_text(&mut self, t: &str) -> Result<(), ClipboardError> {
        write_clipboard_text(t).map_err(ClipboardError::from)
    }

    fn sequence_number(&mut self) -> u64 {
        clipboard_sequence_number() as u64
    }

    fn backend_name(&self) -> &'static str {
        "linux-x11"
    }
}

pub struct X11VirtualDesktop {
    cached: MonitorRect,
}

impl X11VirtualDesktop {
    pub fn new() -> Self {
        Self {
            cached: virtual_desktop_rect(),
        }
    }
}

impl VirtualDesktopGeometry for X11VirtualDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect {
        self.cached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_backend_name_is_linux_uinput() {
        let i = UinputInjector::new().expect("ok");
        assert_eq!(i.backend_name(), "linux-uinput");
    }

    #[test]
    fn clipboard_backend_name_is_linux_x11() {
        let c = X11Clipboard::new();
        assert_eq!(c.backend_name(), "linux-x11");
    }

    #[test]
    fn virtual_desktop_uses_fallback_offline() {
        // Without DISPLAY this returns the fallback.
        let _v = X11VirtualDesktop::new();
        // Just sanity that the call doesn't panic.
    }
}
