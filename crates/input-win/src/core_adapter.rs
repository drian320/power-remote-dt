//! Adapter shim: implements `prdt_input_core` traits on the existing
//! `SendInputInjector` (input injection), the function-style clipboard
//! API (wrapped in a stateful struct), and `virtual_desktop_rect()`.
//!
//! L0 only — host / viewer code is not yet rewired to consume the
//! `prdt_input_core` traits. This module exists so the trait surface
//! is exercised on Windows (smoke tests below) and so the L1 Linux
//! work has a precedent to mirror.

use prdt_input_core::{
    ClipboardError as CoreClipboardError, ClipboardProvider, InjectError as CoreInjectError,
    InputInjector, VirtualDesktopGeometry,
};
use prdt_protocol::{InputEvent, MonitorRect};

use crate::clipboard::{
    clipboard_sequence_number, read_clipboard_text, write_clipboard_text, ClipboardError,
};
use crate::desktop::virtual_desktop_rect;
use crate::injector::{InjectError as WinInjectError, SendInputInjector};

impl InputInjector for SendInputInjector {
    fn inject(&self, event: InputEvent) -> Result<(), CoreInjectError> {
        SendInputInjector::inject(self, event).map_err(map_inject_err)
    }

    fn backend_name(&self) -> &'static str {
        "send-input"
    }
}

fn map_inject_err(err: WinInjectError) -> CoreInjectError {
    match err {
        WinInjectError::SendInput(s) => CoreInjectError::Backend(s),
    }
}

/// Adapter wrapping the function-style Windows clipboard API behind
/// the cross-platform `prdt_input_core::ClipboardProvider` trait. Holds
/// no per-instance state — every call delegates straight to the
/// underlying Win32 helpers.
#[derive(Debug, Default)]
pub struct Win32Clipboard;

impl Win32Clipboard {
    pub fn new() -> Self {
        Self
    }
}

impl ClipboardProvider for Win32Clipboard {
    fn read_text(&mut self) -> Result<String, CoreClipboardError> {
        read_clipboard_text().map_err(map_clipboard_err)
    }

    fn write_text(&mut self, text: &str) -> Result<(), CoreClipboardError> {
        write_clipboard_text(text).map_err(map_clipboard_err)
    }

    fn sequence_number(&mut self) -> u64 {
        clipboard_sequence_number() as u64
    }

    fn backend_name(&self) -> &'static str {
        "win32-clipboard"
    }
}

fn map_clipboard_err(err: ClipboardError) -> CoreClipboardError {
    match err {
        ClipboardError::OpenFailed => {
            CoreClipboardError::Backend("OpenClipboard failed after retries".into())
        }
        ClipboardError::Windows(s) => CoreClipboardError::Backend(s),
        ClipboardError::TooLarge(n) => CoreClipboardError::TooLarge(n),
        ClipboardError::NoText => CoreClipboardError::NoText,
    }
}

#[derive(Default)]
pub struct Win32VirtualDesktop;

impl Win32VirtualDesktop {
    pub fn new() -> Self {
        Self
    }
}

impl VirtualDesktopGeometry for Win32VirtualDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect {
        virtual_desktop_rect()
    }
}
