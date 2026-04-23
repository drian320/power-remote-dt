//! Audio capture + encode + playback for power-remote-dt.
//! Windows host uses WASAPI loopback; viewer uses default output device.
//! Opus codec (48 kHz stereo, ~64 kbps).

pub mod capture;
pub mod error;
pub mod opus_codec;
pub mod playback;

pub use capture::LoopbackCapture;
pub use error::AudioError;
pub use opus_codec::{
    OpusDecoder, OpusEncoder, OPUS_BITRATE, OPUS_CHANNELS, OPUS_FRAME_MS, OPUS_FRAME_SAMPLES,
    OPUS_SAMPLE_RATE,
};
pub use playback::AudioPlayback;
