//! Cross-platform media abstractions used by host (capture/encode) and
//! viewer (decode/render). OS-neutral by design — this crate must NOT
//! depend on `prdt-media-win`, `windows`, X11, Wayland, or any GPU SDK.
//!
//! L1+ Linux backends (`prdt-media-linux`) and the existing Windows
//! backend (`prdt-media-win`) implement the traits in this crate via
//! per-OS adapter modules.

#![forbid(unsafe_code)]

pub mod annexb;
pub mod error;
pub mod frame;
pub mod traits;

pub use annexb::normalize_to_annexb;
pub use error::{AnnexBError, CaptureError, DecodeError, EncodeError};
pub use frame::EncodedPacket;
pub use traits::{Capturer, Decoder, Encoder};
