//! xdg-desktop-portal ScreenCast capture backend.
//!
//! See `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod capturer;
pub mod session;
pub mod stream;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use stream::{LoopCommand, PipeWireStream, PipeWireStreamError, PixelFormat, RawFrame};
pub use token::PortalSessionToken;
