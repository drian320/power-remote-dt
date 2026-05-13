//! VAAPI H.264 encoder.

use crate::error::VaapiError;
use crate::rc::RateControlParams;
use std::path::PathBuf;

pub struct VaapiH264EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,
    pub render_node: Option<PathBuf>,
}

impl Default for VaapiH264EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            initial_bitrate_bps: 5_000_000,
            gop_size: 60,
            render_node: None,
        }
    }
}

pub struct VaapiH264Encoder {
    state: Option<EncoderState>,
    #[allow(dead_code)] // filled in T7
    sps_pps: Vec<u8>,
}

#[allow(dead_code)] // most fields wired in T7 (encode loop)
struct EncoderState {
    rc: RateControlParams,
    rc_dirty: bool,
    sequence_counter: u64,
    idr_pic_id: u16,
    width: u32,
    height: u32,
    fps: u32,
    gop_size: u32,
    // ⚠️  Field order is load-bearing — Drop runs in declaration order.
    // See spec §3.4: image/coded → surfaces → context → config → display.
    // Each Option<...> is taken in reverse and dropped explicitly in
    // impl Drop for VaapiH264Encoder.
}

impl VaapiH264Encoder {
    pub fn new(cfg: VaapiH264EncoderConfig) -> Result<Self, VaapiError> {
        let _node = match cfg.render_node {
            Some(p) => p,
            None => crate::display::probe_first_capable_node()?,
        };
        // T7 implementer: open libva Display, create Config (H264
        // ConstrainedBaseline + EncSlice + RTFormat YUV420 + RateControl
        // CBR), create Context, allocate Surface pool, capture SPS/PPS
        // via packed-header probe or manual prepend.
        //
        // For T6 we return a partially-initialized encoder so the public
        // API surface compiles; encode() returns NotSupported.
        Ok(Self {
            state: Some(EncoderState {
                rc: RateControlParams::cbr_baseline(cfg.initial_bitrate_bps),
                rc_dirty: true,
                sequence_counter: 0,
                idr_pic_id: 0,
                width: cfg.width,
                height: cfg.height,
                fps: cfg.fps,
                gop_size: cfg.gop_size,
            }),
            sps_pps: Vec::new(),
        })
    }

    pub fn encode(
        &mut self,
        _frame: &prdt_media_sw::I420Frame,
        _force_idr: bool,
        _ts_us: u64,
    ) -> Result<prdt_protocol::frame::EncodedFrame, VaapiError> {
        // T7 implements the loop.
        Err(VaapiError::NotSupported(
            "encode loop not yet implemented (T7)".into(),
        ))
    }

    pub fn set_target_bitrate(&mut self, bps: u32) -> Result<(), VaapiError> {
        let Some(s) = self.state.as_mut() else {
            return Err(VaapiError::Closed);
        };
        if s.rc.bits_per_second != bps {
            s.rc = RateControlParams::cbr_baseline(bps);
            s.rc_dirty = true;
        }
        Ok(())
    }

    pub fn backend_name(&self) -> &'static str {
        "vaapi-h264-cbr-baseline"
    }
}

impl Drop for VaapiH264Encoder {
    fn drop(&mut self) {
        // Explicit teardown — T7 fills in the actual sub-drops once real
        // libva resources are held. For T6 the state struct holds only
        // POD, so the default Drop is fine.
        let _ = self.state.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_targets_1080p60_5mbps_cbr() {
        let c = VaapiH264EncoderConfig::default();
        assert_eq!((c.width, c.height), (1920, 1080));
        assert_eq!(c.fps, 60);
        assert_eq!(c.initial_bitrate_bps, 5_000_000);
    }

    #[test]
    fn new_returns_no_render_node_in_container() {
        // The container has no /dev/dri/* — encoder construction must
        // surface NoRenderNode (or NotSupported) instead of panicking.
        let r = VaapiH264Encoder::new(VaapiH264EncoderConfig::default());
        assert!(matches!(
            r,
            Err(VaapiError::NoRenderNode) | Err(VaapiError::NotSupported(_))
        ));
    }

    #[test]
    fn set_target_bitrate_marks_dirty_and_rejects_when_closed() {
        // Construct an encoder bypassing the constructor (test-only) so
        // we can exercise set_target_bitrate logic without VAAPI runtime.
        let mut enc = VaapiH264Encoder {
            state: Some(EncoderState {
                rc: RateControlParams::cbr_baseline(5_000_000),
                rc_dirty: false,
                sequence_counter: 0,
                idr_pic_id: 0,
                width: 1920,
                height: 1080,
                fps: 60,
                gop_size: 60,
            }),
            sps_pps: Vec::new(),
        };
        enc.set_target_bitrate(8_000_000).expect("ok");
        assert!(enc.state.as_ref().unwrap().rc_dirty);
        assert_eq!(enc.state.as_ref().unwrap().rc.bits_per_second, 8_000_000);

        // Close + verify
        enc.state = None;
        let r = enc.set_target_bitrate(10_000_000);
        assert_eq!(r, Err(VaapiError::Closed));
    }
}
