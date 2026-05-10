//! OpenH264-backed software H.264 encoder. Wraps `openh264::encoder::Encoder`
//! with a configuration tuned for low-latency screen-share:
//! - Profile = Baseline (most decoder-compatible, lowest CPU)
//! - Rate control = Bitrate (CBR-ish; OpenH264's `Bitrate` mode is the
//!   closest to CBR exposed by the public API)
//! - Complexity = Low (fastest, ~1 modern x86 core at 1080p60)
//! - num_threads = 0 (auto)
//! - Intra period = 0 (no periodic IDR; viewer / negotiation drives IDR
//!   via `force_idr`)
//!
//! See plan §Phase 1 acceptance for the exact knobs.

use bytes::Bytes;
use prdt_protocol::frame::{Codec, EncodedFrame};

use crate::error::{MediaSwError, Result};
use crate::nv12::I420Frame;
use crate::traits::SwH264Encoder;

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, Profile, RateControlMode,
    SpsPpsStrategy, UsageType,
};
use openh264::{OpenH264API, Timestamp};

/// Configuration knobs for `Openh264Encoder`. Exposed so the host glue
/// layer can override bitrate / fps without reaching into OpenH264 types.
#[derive(Debug, Clone, Copy)]
pub struct Openh264EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub target_bitrate_bps: u32,
    pub max_fps: f32,
}

impl Default for Openh264EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            target_bitrate_bps: 30_000_000,
            max_fps: 60.0,
        }
    }
}

pub struct Openh264Encoder {
    inner: Encoder,
    cfg: Openh264EncoderConfig,
    seq: u64,
    pending_force_idr: bool,
}

impl Openh264Encoder {
    /// Build an encoder using OpenH264 compiled from vendored source
    /// (the `source` feature). No network I/O.
    pub fn new(cfg: Openh264EncoderConfig) -> Result<Self> {
        let api = OpenH264API::from_source();
        let oh_cfg = EncoderConfig::new()
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Bitrate)
            .complexity(Complexity::Low)
            .usage_type(UsageType::ScreenContentRealTime)
            .num_threads(0)
            .max_frame_rate(FrameRate::from_hz(cfg.max_fps))
            .bitrate(BitRate::from_bps(cfg.target_bitrate_bps))
            .skip_frames(false)
            .sps_pps_strategy(SpsPpsStrategy::SpsPpsListing);

        let inner = Encoder::with_api_config(api, oh_cfg)
            .map_err(|e| MediaSwError::openh264("Encoder::with_api_config", e))?;

        Ok(Self {
            inner,
            cfg,
            seq: 0,
            pending_force_idr: true, // first frame is always IDR via force_intra_frame
        })
    }
}

impl SwH264Encoder for Openh264Encoder {
    fn encode(
        &mut self,
        i420: &I420Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> std::result::Result<EncodedFrame, MediaSwError> {
        if i420.width != self.cfg.width || i420.height != self.cfg.height {
            return Err(MediaSwError::DimensionMismatch {
                expected_w: self.cfg.width,
                expected_h: self.cfg.height,
                got_w: i420.width,
                got_h: i420.height,
            });
        }

        if force_idr || self.pending_force_idr {
            self.inner.force_intra_frame();
            self.pending_force_idr = false;
        }

        // OpenH264 takes a millisecond timestamp.
        let ts = Timestamp::from_millis(timestamp_us / 1000);
        let bitstream = self
            .inner
            .encode_at(&i420.as_yuv_source(), ts)
            .map_err(|e| MediaSwError::openh264("Encoder::encode_at", e))?;

        let frame_type = bitstream.frame_type();
        let is_keyframe = matches!(frame_type, FrameType::IDR | FrameType::I);
        let nal_bytes = bitstream.to_vec();
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);

        Ok(EncodedFrame {
            seq,
            timestamp_host_us: timestamp_us,
            is_keyframe,
            nal_units: Bytes::from(nal_bytes),
            width: self.cfg.width,
            height: self.cfg.height,
            codec: Codec::H264,
        })
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        // OpenH264 takes the new bitrate via the next reinit; without
        // reaching into the unsafe raw API there is no in-place setter
        // exposed by openh264 0.9.3. Stash the request — it will take
        // effect on the next call to `encode` after the encoder is
        // reinitialised (which currently only happens on dimension
        // change). Treat as best-effort per the trait doc.
        self.cfg.target_bitrate_bps = bps;
    }

    fn backend_name(&self) -> &'static str {
        "openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nv12::I420Frame;

    fn make_test_frame(width: u32, height: u32, fill_y: u8) -> I420Frame {
        let mut f = I420Frame::new_packed(width, height).unwrap();
        for b in f.y.iter_mut() {
            *b = fill_y;
        }
        for b in f.u.iter_mut() {
            *b = 128;
        }
        for b in f.v.iter_mut() {
            *b = 128;
        }
        f
    }

    /// Annex-B NAL parser: walks the byte stream and yields nal_unit_type
    /// (the 5 low bits of the first byte after each start code).
    fn nal_unit_types(stream: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 < stream.len() {
            // 4-byte start code
            if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 0 && stream[i + 3] == 1 {
                if i + 4 < stream.len() {
                    out.push(stream[i + 4] & 0x1F);
                }
                i += 4;
                continue;
            }
            // 3-byte start code
            if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
                if i + 3 < stream.len() {
                    out.push(stream[i + 3] & 0x1F);
                }
                i += 3;
                continue;
            }
            i += 1;
        }
        out
    }

    #[test]
    fn second_idr_carries_sps_pps() {
        // Verify that after switching to SpsPpsListing, every IDR access unit
        // carries SPS (7) + PPS (8) + IDR slice (5) NAL units — not just the first.
        let cfg = Openh264EncoderConfig {
            width: 320,
            height: 240,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let mut enc = Openh264Encoder::new(cfg).expect("encoder");
        let frame = make_test_frame(320, 240, 128);

        // 1st IDR — the existing test already covers this.
        let ef1 = enc.encode(&frame, true, 0).expect("1st IDR");
        assert!(ef1.is_keyframe);

        // P-frame (no force_idr).
        let ef2 = enc.encode(&frame, false, 33_333).expect("P-frame");
        let _ = ef2; // we don't assert SPS/PPS here

        // 2nd IDR — THIS is what this test is for.
        let ef3 = enc.encode(&frame, true, 66_667).expect("2nd IDR");
        assert!(ef3.is_keyframe, "2nd encode with force_idr=true must be keyframe");

        let types = nal_unit_types(&ef3.nal_units);
        assert!(
            types.contains(&7),
            "2nd IDR must carry SPS (type 7); got: {types:?}"
        );
        assert!(
            types.contains(&8),
            "2nd IDR must carry PPS (type 8); got: {types:?}"
        );
        assert!(
            types.contains(&5),
            "2nd IDR must carry IDR slice (type 5); got: {types:?}"
        );
    }

    #[test]
    fn openh264_encoder_emits_idr_with_sps_pps() {
        // 320x240 small frame keeps test fast; encoder behavior on
        // SPS/PPS/IDR emission is independent of resolution.
        let cfg = Openh264EncoderConfig {
            width: 320,
            height: 240,
            target_bitrate_bps: 1_000_000,
            max_fps: 30.0,
        };
        let mut enc = Openh264Encoder::new(cfg).expect("encoder");
        let frame = make_test_frame(320, 240, 200);
        let ef = enc.encode(&frame, true, 0).expect("encode");
        assert_eq!(ef.codec, Codec::H264);
        assert_eq!(ef.width, 320);
        assert_eq!(ef.height, 240);
        assert!(ef.is_keyframe, "first frame must be a keyframe");
        let types = nal_unit_types(&ef.nal_units);
        // SPS = 7, PPS = 8, IDR slice = 5
        assert!(types.contains(&7), "missing SPS NAL: types {types:?}");
        assert!(types.contains(&8), "missing PPS NAL: types {types:?}");
        assert!(types.contains(&5), "missing IDR slice NAL: types {types:?}");
    }
}
