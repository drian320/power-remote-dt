//! Linux media backend — empty skeleton for L1.
//!
//! This crate compiles to an empty library on non-Linux targets. On
//! Linux it will provide screen-capture (X11 / xdg-desktop-portal +
//! PipeWire) and encode/decode (VAAPI / NVENC Linux / software)
//! implementations of `prdt_media_core` traits in L1+.
//!
//! L0 deliverable: crate exists and is wired into the workspace so
//! the L1 implementer has a place to write code without restructuring
//! the workspace mid-flight.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

// Intentionally empty in L0. L1 will add:
//   pub mod x11_capture;
//   pub mod portal_capture;
//   pub mod vaapi_encode;
//   pub mod nvenc_linux;
//   pub mod ffmpeg_decode;
//   pub mod core_adapter;  // impls of prdt_media_core traits
