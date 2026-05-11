//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod clock;
pub mod control;
pub mod error;
pub mod frame;
pub mod input;
pub mod video_pipeline;
pub mod wire;

pub use clock::now_monotonic_us;

pub use control::{
    AuthMethod, ControlMessage, HelloRejectCode, MonitorRect, PermissionSet, MAX_AUTH_PAYLOAD_BYTES,
};
pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
pub use input::{InputEvent, MouseButton};
pub use video_pipeline::{ConsumerError, ProducerError, VideoConsumer, VideoProducer};
pub use wire::{
    decode_control, encode_control, packet_flags, video_flags, AudioPacket, InputPacket,
    PacketHeader, PacketType, VideoPacket, AUDIO_PAYLOAD_HDR_LEN, DEFAULT_CHUNK_PAYLOAD_LEN,
    HEADER_LEN, INPUT_PAYLOAD_HDR_LEN, MAGIC, PROTOCOL_VERSION, VIDEO_PAYLOAD_HDR_LEN,
};
