use crate::{CaptureError, DecodeError, EncodeError, EncodedPacket};

/// Pulls one frame from a screen-capture backend. The `Frame` associated
/// type is OS-specific (e.g. `D3d11Texture` on Windows, a DMA-BUF FD or
/// CPU `BgraFrame` on Linux). The producer-level pipeline keeps the
/// concrete type erased behind `prdt_protocol::VideoProducer`; this
/// trait is for the inner capture component.
pub trait Capturer: Send {
    type Frame;

    fn next_frame(&mut self) -> Result<Self::Frame, CaptureError>;
}

/// Encodes one captured frame into one `EncodedPacket`. The `Frame`
/// associated type matches the paired `Capturer::Frame` (Capturer and
/// Encoder are typically constructed together for one backend).
pub trait Encoder: Send {
    type Frame;

    fn encode(
        &mut self,
        frame: &Self::Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError>;

    /// Adjust the target output bitrate in bits per second. Best-effort —
    /// the encoder clamps to its supported range; no error is returned.
    fn set_target_bitrate(&mut self, bps: u32);
    fn backend_name(&self) -> &'static str;
}

/// Decodes one encoded packet to a backend-specific decoded frame
/// (typically a GPU texture or CPU YUV buffer). Like `Capturer`, the
/// `Frame` type is OS-/backend-specific.
pub trait Decoder: Send {
    type Frame;

    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<Self::Frame>, DecodeError>;
    fn backend_name(&self) -> &'static str;
}
