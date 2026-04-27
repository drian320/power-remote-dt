//! Cross-platform software H.264 encode/decode via OpenH264.
//!
//! Built from vendored OpenH264 source (the `source` feature of the
//! `openh264` crate) so there is no build-time network I/O and the
//! binary stays BSD-2-Clause clean for redistribution. See the
//! `software-codec-openh264-complete` ADR for the license posture.
//!
//! This crate is intentionally pure-Rust at the public API: it has no
//! dependency on the `windows` crate, so it builds on Linux today even
//! though Linux capture is a follow-up phase.

pub mod decoder;
pub mod encoder;
pub mod error;
pub mod nv12;
pub mod traits;

pub use decoder::Openh264Decoder;
pub use encoder::{Openh264Encoder, Openh264EncoderConfig};
pub use error::{MediaSwError, Result};
pub use nv12::{I420Frame, bgra_to_i420, i420_to_nv12, make_counter_i420};
pub use traits::{SwH264Decoder, SwH264Encoder};
