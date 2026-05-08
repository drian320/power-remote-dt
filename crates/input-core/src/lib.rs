//! Cross-platform input + clipboard + virtual-desktop abstractions.
//!
//! OS-neutral. The Windows backend (`prdt-input-win`) and the future
//! Linux backend (`prdt-input-linux`) implement these traits via
//! per-OS adapters. Host / viewer code switches to these traits in a
//! follow-up plan; L0 only introduces the abstraction.

#![forbid(unsafe_code)]

pub mod error;
pub mod traits;

pub use error::{ClipboardError, InjectError};
pub use traits::{ClipboardProvider, InputInjector, VirtualDesktopGeometry};
