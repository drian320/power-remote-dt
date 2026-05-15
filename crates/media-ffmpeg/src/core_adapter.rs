use prdt_media_core::{EncodeError, EncodedPacket, Encoder};
use prdt_media_sw::I420Frame;

use crate::hevc_vaapi_encoder::HevcVaapiFfmpegEncoder;

pub struct HevcVaapiFfmpegEncoderAdapter(pub HevcVaapiFfmpegEncoder);

// SAFETY: HevcVaapiFfmpegEncoder owns all its FFmpeg resources exclusively via
// NonNull pointers. It is never aliased and the caller ensures it is only used
// from one thread at a time (the encoder pipeline always runs single-threaded).
unsafe impl Send for HevcVaapiFfmpegEncoderAdapter {}

impl Encoder for HevcVaapiFfmpegEncoderAdapter {
    type Frame = I420Frame;

    fn encode(
        &mut self,
        frame: &I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        self.0.encode(frame, force_idr, ts_us)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if let Err(e) = self.0.set_target_bitrate(bps) {
            // Rate-limited (1/min) warn-log per plan Critic fold-in 6.
            self.0.maybe_warn_bitrate_failure(&e, bps);
        }
    }

    fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_satisfies_encoder_trait_bound() {
        // Compile-time assertion: HevcVaapiFfmpegEncoderAdapter implements Encoder<Frame=I420Frame>.
        fn _accepts_encoder<E: Encoder<Frame = I420Frame>>(_e: &mut E) {}
        let _ = std::marker::PhantomData::<HevcVaapiFfmpegEncoderAdapter>;
    }
}
