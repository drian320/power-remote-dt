//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod control;
pub mod error;
pub mod frame;
pub mod input;
pub mod wire;

pub use control::ControlMessage;
pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
pub use input::{InputEvent, MouseButton};
pub use wire::{
    PacketHeader, PacketType, DEFAULT_CHUNK_PAYLOAD_LEN, HEADER_LEN, MAGIC, PROTOCOL_VERSION,
};
