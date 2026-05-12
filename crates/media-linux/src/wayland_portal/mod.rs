//! xdg-desktop-portal ScreenCast capture backend.
//!
//! See `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod token;

// Re-exports filled in T4-T6.
pub use token::PortalSessionToken;
