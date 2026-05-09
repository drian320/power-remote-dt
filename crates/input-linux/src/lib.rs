//! Linux input backend — uinput inject + X11 clipboard + RandR
//! geometry + L0 trait adapters.
//!
//! See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod error;
pub mod uinput_injector;
pub mod x11_clipboard;
pub mod x11_geometry;
pub mod core_adapter;

pub use error::LinuxInputError;

// Free-function production surface (host imports these via cfg):
pub use uinput_injector::inject_event;

pub use x11_clipboard::{
    clipboard_sequence_number, read_clipboard_text, write_clipboard_text, MAX_CLIPBOARD_BYTES,
};

pub use x11_geometry::virtual_desktop_rect;
