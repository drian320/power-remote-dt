//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod error;
pub mod frame;

pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
