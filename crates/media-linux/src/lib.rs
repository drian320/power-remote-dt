//! Linux media backend — XShm capture + OpenH264 SW encode/decode +
//! VideoProducer adapter. See `docs/superpowers/specs/2026-05-09-l1-linux-poc-design.md`.
//!
//! The crate compiles to an empty library on non-Linux targets.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

pub mod error;
pub mod frame;
pub mod x11_capture;
pub mod sw_pipeline;
// Subsequent tasks will add:
//   pub mod i420_to_bgra;
//   pub mod linux_sw_producer;
//   pub mod core_adapter;

pub use error::LinuxMediaError;
pub use frame::BgraFrame;
