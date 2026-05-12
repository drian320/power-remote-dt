//! xdg-desktop-portal ScreenCast capture backend.
//!
//! See `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md`
//! and `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod capturer;
pub mod format;
pub mod session;
pub mod stream;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use format::{
    BuiltParams, NegotiatedFormat, ParseError, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR,
};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use stream::{LoopCommand, PipeWireStream, PipeWireStreamError, PixelFormat, RawFrame};
pub use token::PortalSessionToken;
