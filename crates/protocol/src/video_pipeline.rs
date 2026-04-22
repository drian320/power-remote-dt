//! Primary traits for the video pipeline: `VideoProducer` on the host side
//! (capture + encode) and `VideoConsumer` on the viewer side (decode + render).
//!
//! These are the public boundaries between the media pipeline and the rest
//! of the system (per spec §2.3). Internal sub-traits (DesktopCapture,
//! VideoEncoder, VideoDecoder, VideoRenderer) are crate-private inside
//! `media-win` / future `media-linux`.

use crate::EncodedFrame;

#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("capture: {0}")]
    Capture(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("other: {0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("decode: {0}")]
    Decode(String),
    #[error("render: {0}")]
    Render(String),
    #[error("other: {0}")]
    Other(String),
}

/// Captures the desktop, encodes frames, and emits `EncodedFrame` on demand.
/// Implementations: `DxgiNvencProducer` (Windows). Future: `WaylandVaapiProducer` (Linux), etc.
#[async_trait::async_trait]
pub trait VideoProducer: Send {
    /// Return the next encoded frame. Blocks until one is available.
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError>;

    /// Request an IDR (keyframe) at the next encode opportunity. Idempotent
    /// within the rate-limit window defined in spec §4.3.
    fn request_idr(&mut self);

    /// Update the target bitrate in bits per second. Honoured best-effort.
    fn set_target_bitrate(&mut self, bps: u32);
}

/// Accepts `EncodedFrame` on the viewer, decodes, and hands the decoded
/// frame to the render layer (via its own internals).
#[async_trait::async_trait]
pub trait VideoConsumer: Send {
    /// Submit an encoded frame for decoding and (eventual) display.
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError>;

    /// Whether the consumer needs an IDR (because of a decode failure, a
    /// stream discontinuity, or a fresh session). The caller forwards this
    /// as `ControlMessage::RequestIdr` to the host.
    fn needs_idr(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        assert_eq!(
            ProducerError::Capture("DXGI lost".into()).to_string(),
            "capture: DXGI lost"
        );
        assert_eq!(
            ConsumerError::Decode("MF_E_INVALIDMEDIATYPE".into()).to_string(),
            "decode: MF_E_INVALIDMEDIATYPE"
        );
    }
}
