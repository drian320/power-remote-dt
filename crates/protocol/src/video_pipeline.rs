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
    /// Backend permanently lost its device (driver crash, GPU hot-unplug,
    /// adapter removed). Carries a stable `backend` identifier and a free-form
    /// `reason`. PolicyDriven matches on this to trigger failover.
    #[error("device lost on {backend}: {reason}")]
    DeviceLost { backend: String, reason: String },
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

    /// Return a short, stable identifier for the capture+encode backend in
    /// use (e.g. `"nvenc-h265"`, `"openh264-sw"`, `"linux-x11shm-openh264"`).
    /// Used for logging/metrics; must be `'static` so callers can record it
    /// without cloning.
    fn backend_name(&self) -> &'static str;
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

    #[test]
    fn device_lost_display() {
        let e = ProducerError::DeviceLost {
            backend: "nvenc-h265".into(),
            reason: "DXGI_ERROR_DEVICE_REMOVED".into(),
        };
        assert_eq!(
            e.to_string(),
            "device lost on nvenc-h265: DXGI_ERROR_DEVICE_REMOVED",
        );
    }

    #[test]
    fn video_producer_trait_has_backend_name() {
        struct Stub;
        #[async_trait::async_trait]
        impl VideoProducer for Stub {
            async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError> {
                unreachable!()
            }
            fn request_idr(&mut self) {}
            fn set_target_bitrate(&mut self, _bps: u32) {}
            fn backend_name(&self) -> &'static str {
                "stub"
            }
        }
        let s: Box<dyn VideoProducer> = Box::new(Stub);
        assert_eq!(s.backend_name(), "stub");
    }
}
