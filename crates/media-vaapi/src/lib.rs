//! VAAPI H.264 encoder backend for Linux HW codec path.
//!
//! See `docs/superpowers/specs/2026-05-13-p5c1-vaapi-h264-encoder-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod annexb;
pub mod display;
pub mod encoder;
pub mod error;
pub mod frame_input;
pub mod rc;

pub use encoder::{VaapiH264Encoder, VaapiH264EncoderConfig};
pub use error::VaapiError;
pub use frame_input::FrameInput;
