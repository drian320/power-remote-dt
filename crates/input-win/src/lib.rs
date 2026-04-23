//! Windows input capture (RawInput via winit) and injection (SendInput).

#![cfg(windows)]

pub mod capturer;
pub mod clipboard;
pub mod desktop;
pub mod injector;

pub use capturer::RawInputCapturer;
pub use clipboard::{
    read_clipboard_text, write_clipboard_text, ClipboardError, MAX_CLIPBOARD_BYTES,
};
pub use desktop::virtual_desktop_rect;
pub use injector::{InjectError, SendInputInjector};
