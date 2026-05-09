//! Platform-specific host backends. The cfg-aliased re-exports below give
//! `lib.rs` a single, OS-transparent symbol set. See spec §5.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("input dispatch backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard backend error: {0}")]
    Backend(String),
    #[error("no text content available")]
    NoText,
    #[error("clipboard payload too large: {0} bytes")]
    TooLarge(usize),
}

#[cfg(windows)]
pub mod win;

#[cfg(target_os = "linux")]
pub mod linux;

// Cfg-aliased re-exports added in T4/T5 once both modules' factory
// surfaces exist. For now, leave only the error types public from
// this module.
