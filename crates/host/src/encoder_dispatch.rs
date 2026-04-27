//! Host-level dispatch over the two encoder backends:
//! - `Hw`: media-win's `HwHevcEncoder` (NVENC or MF MFT, both H.265).
//! - `SwH264`: media-sw's `Openh264Encoder` (CPU, H.264).
//!
//! Lives in the host crate (not media-win) because `media-sw` must not
//! depend on `windows`/`media-win` (Linux-buildability principle from
//! the plan §1.4). This is the smallest neutral place that already
//! depends on both media crates.

use prdt_media_sw::Openh264Encoder;
use prdt_media_win::{Hevc265Encoder, HwHevcEncoder};

#[allow(unused_imports)]
use prdt_media_sw::Openh264EncoderConfig;

/// Runtime-dispatched video encoder used to construct the right producer
/// in `run_host`. Phase 4 will use the `is_h264()` discriminator to fork
/// producer construction; Phase 2 just wires up the type.
pub enum VideoEncoderBackend {
    Hw(HwHevcEncoder),
    SwH264(Openh264Encoder),
}

impl VideoEncoderBackend {
    pub fn backend_name(&self) -> &'static str {
        match self {
            Self::Hw(e) => e.backend_name(),
            Self::SwH264(_) => "openh264",
        }
    }

    #[allow(dead_code)]
    pub fn is_h264(&self) -> bool {
        matches!(self, Self::SwH264(_))
    }

    /// Best-effort target-bitrate update. For OpenH264 the new value is
    /// stashed in cfg and takes effect on encoder reinit (see media-sw
    /// `Openh264Encoder::set_target_bitrate` doc).
    #[allow(dead_code)]
    pub fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            Self::Hw(e) => e.set_target_bitrate(bps),
            Self::SwH264(e) => {
                use prdt_media_sw::SwH264Encoder;
                e.set_target_bitrate(bps);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sw_backend_name_is_openh264() {
        let cfg = Openh264EncoderConfig {
            width: 320,
            height: 240,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let enc = Openh264Encoder::new(cfg).expect("encoder");
        let backend = VideoEncoderBackend::SwH264(enc);
        assert_eq!(backend.backend_name(), "openh264");
        assert!(backend.is_h264());
    }
}
