//! Linux input backend — empty skeleton for L1.
//!
//! This crate compiles to an empty library on non-Linux targets. On
//! Linux it will provide input injection (uinput, libei,
//! xdg-desktop-portal RemoteDesktop), clipboard sync (wl-clipboard /
//! arboard / portal Clipboard), and virtual-desktop geometry queries.
//!
//! L0 deliverable: crate exists and is wired into the workspace.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

// Intentionally empty in L0. L1+ will add:
//   pub mod uinput_injector;
//   pub mod libei_injector;
//   pub mod xtest_injector;
//   pub mod wl_clipboard;
//   pub mod x11_clipboard;
//   pub mod core_adapter;  // impls of prdt_input_core traits
