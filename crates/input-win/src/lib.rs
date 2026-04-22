//! Windows input capture (RawInput via winit) and injection (SendInput).

#![cfg(windows)]

pub mod capturer;
pub mod injector;

pub use capturer::RawInputCapturer;
pub use injector::{InjectError, SendInputInjector};
