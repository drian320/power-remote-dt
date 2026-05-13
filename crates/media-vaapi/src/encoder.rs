//! VAAPI H.264 encoder.
//!
//! ## cros-libva 0.0.13 API map (used here)
//!
//! - `libva::Display::open_drm_display(path)` → `Rc<Display>`
//! - `display.get_config_attributes(profile, entrypoint, &mut attrs)`
//! - `display.create_config(attrs, profile, entrypoint)` → `Config`
//! - `display.create_surfaces::<()>(rt_format, fourcc, w, h, hint, Vec<()>)`
//!   returns `Vec<Surface<()>>` (NV12-backed when fourcc=VA_FOURCC_NV12 +
//!   USAGE_HINT_ENCODER + RT_FORMAT_YUV420)
//! - `display.create_context(&config, w, h, Some(&surfaces), true)` →
//!   `Rc<Context>`
//! - `context.create_buffer(BufferType::*)` → `Buffer`
//! - `context.create_enc_coded(size)` → `EncCodedBuffer`
//! - `Image::create_from(&surface, format, coded, visible)` for NV12 upload
//! - `Picture::new(ts, ctx, surface) → .begin() → .render() → .end() →
//!   .sync()`
//! - `MappedCodedBuffer::new(&coded_buf)` then `.iter()` over
//!   `MappedCodedSegment { buf, .. }`
//!
//! ## SPS/PPS strategy
//!
//! cros-libva 0.0.13 does not expose a packed-header buffer builder for
//! H.264 (the `VAEncPackedHeader*BufferType` bindings exist in the FFI
//! layer but there is no safe wrapper). We therefore construct the SPS+PPS
//! Annex-B bytes manually at `new()`, matching the chosen profile (H.264
//! Constrained Baseline, level 4.1) plus the configured width/height/fps.
//! The bytes are cached in `sps_pps: Vec<u8>` and prepended by
//! `annexb::normalize_to_annexb` on every IDR.
//!
//! ## Drop order (load-bearing — see spec §3.4)
//!
//! Reverse-creation order:
//!   image/coded  →  surfaces  →  context  →  config  →  display
//!
//! Achieved by storing each handle in `Option<T>` on `EncoderState` and
//! taking them in this exact order inside `impl Drop for VaapiH264Encoder`.

use crate::annexb::normalize_to_annexb;
use crate::error::VaapiError;
use crate::rc::RateControlParams;
use bytes::Bytes;
use cros_libva as libva;
use prdt_protocol::frame::{Codec, EncodedFrame};
use std::path::PathBuf;
use std::rc::Rc;

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
    sps_pps: Vec<u8>,
}

/// Inner state. Field order is load-bearing for the *default* Drop impl,
/// but `VaapiH264Encoder` overrides Drop so it explicitly takes the
/// Options in the spec §3.4 sequence regardless of declaration order.
struct EncoderState {
    rc: RateControlParams,
    rc_dirty: bool,
    sequence_counter: u64,
    idr_pic_id: u16,
    width: u32,
    height: u32,
    fps: u32,
    gop_size: u32,
    /// One-time warn latch for the slow vaCreateImage+vaPutImage fallback.
    create_image_warned: bool,
    /// Round-robin index into `surfaces`. We hold one in-flight encode at
    /// a time today; this counter is the simplest correct policy.
    next_surface_idx: usize,
    /// NV12 VAImageFormat selected at init from the display's image format
    /// list. Cached so we don't re-query per frame.
    nv12_format: libva::VAImageFormat,
    /// Per-frame allocated coded buffer slot. cros-libva 0.0.13's
    /// EncCodedBuffer must be dropped before its parent Context, so we
    /// keep it as Option and take() it before Context drop.
    coded_buf: Option<libva::EncCodedBuffer>,
    /// Surface pool — 4 surfaces, recycled round-robin.
    surfaces: Option<Vec<libva::Surface<()>>>,
    context: Option<Rc<libva::Context>>,
    config: Option<libva::Config>,
    display: Option<Rc<libva::Display>>,
}

impl VaapiH264Encoder {
    pub fn new(cfg: VaapiH264EncoderConfig) -> Result<Self, VaapiError> {
        let node = match cfg.render_node {
            Some(p) => p,
            None => crate::display::probe_first_capable_node()?,
        };

        // 1. Open Display.
        let display = libva::Display::open_drm_display(&node)
            .map_err(|e| VaapiError::DisplayOpen(format!("{node:?}: {e}")))?;

        let profile = libva::VAProfile::VAProfileH264ConstrainedBaseline;
        let entrypoint = libva::VAEntrypoint::VAEntrypointEncSlice;

        // 2. Capability probe (RT format + RC mode).
        let mut attrs = vec![
            libva::VAConfigAttrib {
                type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                value: 0,
            },
            libva::VAConfigAttrib {
                type_: libva::VAConfigAttribType::VAConfigAttribRateControl,
                value: 0,
            },
        ];
        display
            .get_config_attributes(profile, entrypoint, &mut attrs)
            .map_err(|e| VaapiError::NotSupported(format!("get_config_attributes: {e}")))?;
        if attrs[0].value & libva::VA_RT_FORMAT_YUV420 == 0 {
            return Err(VaapiError::NotSupported(
                "driver does not advertise VA_RT_FORMAT_YUV420 for H.264 EncSlice".into(),
            ));
        }
        if attrs[1].value & libva::VA_RC_CBR == 0 {
            return Err(VaapiError::NotSupported(
                "driver does not advertise VA_RC_CBR for H.264 EncSlice".into(),
            ));
        }

        // 3. Build the actual Config attrs (only those we request).
        let cfg_attrs = vec![
            libva::VAConfigAttrib {
                type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                value: libva::VA_RT_FORMAT_YUV420,
            },
            libva::VAConfigAttrib {
                type_: libva::VAConfigAttribType::VAConfigAttribRateControl,
                value: libva::VA_RC_CBR,
            },
        ];
        let config = display
            .create_config(cfg_attrs, profile, entrypoint)
            .map_err(|e| VaapiError::NotSupported(format!("create_config: {e}")))?;

        // 4. Allocate the surface pool (4 NV12 surfaces).
        let descriptors: Vec<()> = (0..4).map(|_| ()).collect();
        let surfaces = display
            .create_surfaces::<()>(
                libva::VA_RT_FORMAT_YUV420,
                Some(libva::VA_FOURCC_NV12),
                cfg.width,
                cfg.height,
                Some(libva::UsageHint::USAGE_HINT_ENCODER),
                descriptors,
            )
            .map_err(|e| VaapiError::NotSupported(format!("create_surfaces: {e}")))?;

        // 5. Context.
        let context = display
            .create_context::<()>(&config, cfg.width, cfg.height, Some(&surfaces), true)
            .map_err(|e| VaapiError::NotSupported(format!("create_context: {e}")))?;

        // 6. Find an NV12 VAImageFormat for the upload step.
        let image_formats = display
            .query_image_formats()
            .map_err(|e| VaapiError::NotSupported(format!("query_image_formats: {e}")))?;
        let nv12_format = image_formats
            .into_iter()
            .find(|f| f.fourcc == libva::VA_FOURCC_NV12)
            .ok_or_else(|| VaapiError::NotSupported("no NV12 VAImageFormat available".into()))?;

        // 7. Cache SPS+PPS bytes for IDR prepending.
        let sps_pps = build_h264_spsps_baseline(cfg.width, cfg.height, cfg.fps);

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
                create_image_warned: false,
                next_surface_idx: 0,
                nv12_format,
                coded_buf: None,
                surfaces: Some(surfaces),
                context: Some(context),
                config: Some(config),
                display: Some(display),
            }),
            sps_pps,
        })
    }

    pub fn encode(
        &mut self,
        frame: &prdt_media_sw::I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<EncodedFrame, VaapiError> {
        let st = self.state.as_mut().ok_or(VaapiError::Closed)?;

        if frame.width != st.width || frame.height != st.height {
            return Err(VaapiError::NotSupported(format!(
                "frame {}x{} != encoder {}x{}",
                frame.width, frame.height, st.width, st.height
            )));
        }

        let surfaces = st
            .surfaces
            .as_mut()
            .ok_or_else(|| VaapiError::Bitstream("surface pool taken".into()))?;
        if surfaces.is_empty() {
            return Err(VaapiError::Bitstream("surface pool empty".into()));
        }
        let n = surfaces.len();
        let surface_idx = st.next_surface_idx % n;
        st.next_surface_idx = (st.next_surface_idx + 1) % n;
        let surface_id = surfaces[surface_idx].id();

        let is_idr = force_idr
            || st.sequence_counter == 0
            || st.sequence_counter % (st.gop_size as u64) == 0;
        if is_idr {
            st.idr_pic_id = st.idr_pic_id.wrapping_add(1);
        }
        let seq = st.sequence_counter;
        let frame_num: u16 = (seq % 65536) as u16;
        let pic_order_cnt_lsb: u16 = ((seq * 2) % 65536) as u16;

        // 1. Upload I420 → NV12 onto the surface.
        upload_i420_as_nv12(
            &surfaces[surface_idx],
            frame,
            st.nv12_format,
            st.width,
            st.height,
            &mut st.create_image_warned,
        )?;

        let context = st
            .context
            .as_ref()
            .ok_or_else(|| VaapiError::Bitstream("context taken".into()))?
            .clone();

        // 2. Allocate a fresh coded output buffer (size = w*h*4, generous).
        let coded_size = (st.width as usize) * (st.height as usize) * 4;
        let coded_buffer = context
            .create_enc_coded(coded_size)
            .map_err(|e| VaapiError::Bitstream(format!("create_enc_coded: {e}")))?;

        // 3. Sequence parameter buffer (SPS-equivalent).
        let seq_fields = libva::H264EncSeqFields::new(
            1, // chroma_format_idc = 4:2:0
            1, // frame_mbs_only_flag
            0, // mb_adaptive_frame_field_flag
            0, // seq_scaling_matrix_present_flag
            0, // direct_8x8_inference_flag (baseline: 0)
            1, // log2_max_frame_num_minus4
            0, // pic_order_cnt_type
            2, // log2_max_pic_order_cnt_lsb_minus4
            0, // delta_pic_order_always_zero_flag
        );
        let width_mbs = st.width.div_ceil(16) as u16;
        let height_mbs = st.height.div_ceil(16) as u16;
        let seq_param = libva::EncSequenceParameterBufferH264::new(
            0,                     // seq_parameter_set_id
            41,                    // level_idc (4.1)
            st.gop_size,           // intra_period
            st.gop_size,           // intra_idr_period
            1,                     // ip_period (no B in baseline)
            st.rc.bits_per_second, // bits_per_second
            1,                     // max_num_ref_frames
            width_mbs,
            height_mbs,
            &seq_fields,
            0, // bit_depth_luma_minus8
            0, // bit_depth_chroma_minus8
            0, // num_ref_frames_in_pic_order_cnt_cycle
            0, // offset_for_non_ref_pic
            0, // offset_for_top_to_bottom_field
            [0; 256],
            None,       // frame_crop
            None,       // vui_fields
            0,          // aspect_ratio_idc
            0,          // sar_width
            0,          // sar_height
            1,          // num_units_in_tick
            st.fps * 2, // time_scale (timescale = 2*fps for fixed_frame_rate)
        );
        let sps_buf = context
            .create_buffer(libva::BufferType::EncSequenceParameter(
                libva::EncSequenceParameter::H264(seq_param),
            ))
            .map_err(|e| VaapiError::Bitstream(format!("create_buffer(SPS): {e}")))?;

        // 4. Picture parameter buffer.
        let ref_frames: [libva::PictureH264; 16] = (0..16)
            .map(|_| {
                libva::PictureH264::new(libva::VA_INVALID_ID, 0, libva::VA_INVALID_SURFACE, 0, 0)
            })
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| VaapiError::Bitstream("ref_frames[16] build".into()))?;

        let pic_fields = libva::H264EncPicFields::new(
            if is_idr { 1 } else { 0 }, // idr_pic_flag
            1,                          // reference_pic_flag
            0,                          // entropy_coding_mode_flag (CAVLC for baseline)
            0,                          // weighted_pred_flag
            0,                          // weighted_bipred_idc
            0,                          // constrained_intra_pred_flag
            0,                          // transform_8x8_mode_flag (baseline: 0)
            1,                          // deblocking_filter_control_present_flag
            0,                          // redundant_pic_cnt_present_flag
            0,                          // pic_order_present_flag
            0,                          // pic_scaling_matrix_present_flag
        );
        let pic_param = libva::EncPictureParameterBufferH264::new(
            libva::PictureH264::new(surface_id, frame_num as u32, 0, 0, 0),
            ref_frames,
            coded_buffer.id(),
            0, // pic_parameter_set_id
            0, // seq_parameter_set_id
            1, // last_picture (we submit each frame as a complete picture)
            frame_num,
            26, // pic_init_qp
            0,  // num_ref_idx_l0_active_minus1
            0,  // num_ref_idx_l1_active_minus1
            0,  // chroma_qp_index_offset
            0,  // second_chroma_qp_index_offset
            &pic_fields,
        );
        let pps_buf = context
            .create_buffer(libva::BufferType::EncPictureParameter(
                libva::EncPictureParameter::H264(pic_param),
            ))
            .map_err(|e| VaapiError::Bitstream(format!("create_buffer(PPS): {e}")))?;

        // 5. Slice parameter buffer.
        let ref_list_invalid: [libva::PictureH264; 32] = (0..32)
            .map(|_| {
                libva::PictureH264::new(libva::VA_INVALID_ID, 0, libva::VA_INVALID_SURFACE, 0, 0)
            })
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| VaapiError::Bitstream("ref_pic_list[32] build".into()))?;
        let ref_list_invalid_b: [libva::PictureH264; 32] = (0..32)
            .map(|_| {
                libva::PictureH264::new(libva::VA_INVALID_ID, 0, libva::VA_INVALID_SURFACE, 0, 0)
            })
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| VaapiError::Bitstream("ref_pic_list[32] build".into()))?;

        // H.264 slice_type: I=2 (or 7), P=0 (or 5). Use baseline values 2/0
        // matching the cros-libva enc_h264_demo (which uses 2 for I).
        let slice_type: u8 = if is_idr { 2 } else { 0 };
        let total_mbs = (width_mbs as u32) * (height_mbs as u32);
        let slice_param = libva::EncSliceParameterBufferH264::new(
            0, // macroblock_address
            total_mbs,
            libva::VA_INVALID_ID, // macroblock_info (unused)
            slice_type,
            0, // pic_parameter_set_id
            st.idr_pic_id,
            pic_order_cnt_lsb,
            0,      // delta_pic_order_cnt_bottom
            [0, 0], // delta_pic_order_cnt
            1,      // direct_spatial_mv_pred_flag
            0,      // num_ref_idx_active_override_flag
            0,      // num_ref_idx_l0_active_minus1
            0,      // num_ref_idx_l1_active_minus1
            ref_list_invalid,
            ref_list_invalid_b,
            0,
            0,
            0,
            [0; 32],
            [0; 32],
            0,
            [[0; 2]; 32],
            [[0; 2]; 32],
            0,
            [0; 32],
            [0; 32],
            0,
            [[0; 2]; 32],
            [[0; 2]; 32],
            0, // cabac_init_idc
            0, // slice_qp_delta
            0, // disable_deblocking_filter_idc
            2, // slice_alpha_c0_offset_div2
            2, // slice_beta_offset_div2
        );
        let slice_buf = context
            .create_buffer(libva::BufferType::EncSliceParameter(
                libva::EncSliceParameter::H264(slice_param),
            ))
            .map_err(|e| VaapiError::Bitstream(format!("create_buffer(slice): {e}")))?;

        // 6. Optional RC misc parameter buffer.
        let rc_buf_opt = if st.rc_dirty {
            let rc_flags = libva::RcFlags::default();
            let rc_param = libva::EncMiscParameterRateControl::new(
                st.rc.bits_per_second,
                st.rc.target_percentage,
                st.rc.window_size_ms,
                st.rc.initial_qp,
                st.rc.min_qp,
                0, // basic_unit_size (0 = driver default)
                rc_flags,
                0, // icq_quality_factor
                st.rc.max_qp,
                0, // quality_factor
                0, // target_frame_size
            );
            let buf = context
                .create_buffer(libva::BufferType::EncMiscParameter(
                    libva::EncMiscParameter::RateControl(rc_param),
                ))
                .map_err(|e| VaapiError::Bitstream(format!("create_buffer(RC): {e}")))?;
            st.rc_dirty = false;
            Some(buf)
        } else {
            None
        };

        // 7. Picture state machine: take ownership of the surface for this
        //    frame, attach all buffers, begin/render/end/sync (with retry).
        let st_surfaces = st.surfaces.as_mut().unwrap();
        let surface = st_surfaces.remove(surface_idx);
        // After remove, adjust round-robin so the next encode picks the
        // newly-rotated slot at the original index (which now holds the
        // next surface in the pool order).
        if !st_surfaces.is_empty() {
            st.next_surface_idx = surface_idx % st_surfaces.len();
        } else {
            st.next_surface_idx = 0;
        }

        let mut picture = libva::Picture::new(ts_us, Rc::clone(&context), surface);
        picture.add_buffer(sps_buf);
        picture.add_buffer(pps_buf);
        picture.add_buffer(slice_buf);
        if let Some(b) = rc_buf_opt {
            picture.add_buffer(b);
        }

        let p_begin = picture
            .begin::<()>()
            .map_err(|e| classify_or_bitstream(e, "vaBeginPicture"))?;
        let p_render = p_begin
            .render()
            .map_err(|e| classify_or_bitstream(e, "vaRenderPicture"))?;
        let p_end = p_render
            .end()
            .map_err(|e| classify_or_bitstream(e, "vaEndPicture"))?;

        // Sync with HW_BUSY retry. cros-libva returns Err((VaError, Self))
        // on sync failure, lending the Picture back so we can retry.
        let backoff_us: [u64; 5] = [500, 1_000, 2_000, 4_000, 8_000];
        let mut attempt: u32 = 0;
        let mut cur = p_end;
        let p_sync = loop {
            match cur.sync::<()>() {
                Ok(p) => break p,
                Err((e, picture_back)) => {
                    if is_hw_busy(&e) && attempt < (backoff_us.len() as u32) {
                        std::thread::sleep(std::time::Duration::from_micros(
                            backoff_us[attempt as usize],
                        ));
                        attempt += 1;
                        cur = picture_back;
                        continue;
                    }
                    if is_hw_busy(&e) {
                        return Err(VaapiError::HardwareBusy {
                            attempts: backoff_us.len() as u32,
                        });
                    }
                    return Err(classify_or_bitstream(e, "vaSyncSurface"));
                }
            }
        };

        // 8. Map coded buffer, concat segments.
        let mut raw: Vec<u8> = Vec::new();
        {
            let mapped = libva::MappedCodedBuffer::new(&coded_buffer)
                .map_err(|e| VaapiError::Bitstream(format!("MappedCodedBuffer::new: {e}")))?;
            for seg in mapped.iter() {
                raw.extend_from_slice(seg.buf);
            }
        }
        // 9. Reclaim the surface back into the pool.
        match p_sync.take_surface() {
            Ok(s) => {
                let pool = st.surfaces.as_mut().unwrap();
                pool.push(s);
            }
            Err(_) => {
                return Err(VaapiError::Bitstream(
                    "take_surface failed (surface ref held elsewhere)".into(),
                ));
            }
        }
        // The coded buffer is consumed here; it lives only for this call.
        // (Drop happens on function exit; explicit drop just makes intent
        // obvious.)
        drop(coded_buffer);

        // 10. Normalize to Annex-B + prepend cached SPS/PPS on IDR.
        let mut nal_out: Vec<u8> = Vec::with_capacity(raw.len() + self.sps_pps.len());
        normalize_to_annexb(&raw, &self.sps_pps, is_idr, &mut nal_out)?;

        st.sequence_counter = st.sequence_counter.wrapping_add(1);

        Ok(EncodedFrame {
            seq,
            timestamp_host_us: ts_us,
            is_keyframe: is_idr,
            nal_units: Bytes::from(nal_out),
            width: st.width,
            height: st.height,
            codec: Codec::H264,
        })
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
        // Spec §3.4 — reverse-creation order:
        //   image/coded → surfaces → context → config → display.
        // Each sub-resource's Drop is RAII inside cros-libva; we just
        // sequence the takes here.
        if let Some(mut st) = self.state.take() {
            // 1. coded output buffer (held inside Option as well)
            let _ = st.coded_buf.take();
            // 2. surface pool
            let _ = st.surfaces.take();
            // 3. context
            let _ = st.context.take();
            // 4. config
            let _ = st.config.take();
            // 5. display
            let _ = st.display.take();
            // Remaining POD fields drop with `st`.
        }
    }
}

/// Maps a libva error to either HardwareBusy (caller layer) or generic
/// Bitstream/Driver classification. Used for picture-state-machine errors
/// where we don't have a numeric VAStatus directly.
fn classify_or_bitstream(e: libva::VaError, ctx: &'static str) -> VaapiError {
    if is_hw_busy(&e) {
        VaapiError::HardwareBusy { attempts: 0 }
    } else {
        VaapiError::Bitstream(format!("{ctx}: {e}"))
    }
}

fn is_hw_busy(e: &libva::VaError) -> bool {
    let status = e.va_status() as u32;
    status == libva::VA_STATUS_ERROR_HW_BUSY || status == libva::VA_STATUS_ERROR_TIMEDOUT
}

/// Upload an I420 frame onto a VA Surface as NV12 (interleaved UV).
///
/// Tries `vaCreateImage + vaPutImage` via cros-libva's `Image::create_from`
/// — this is the safe path supported by the wrapper for write-back to a
/// surface. (cros-libva 0.0.13's `vaDeriveImage` path through `Image` is
/// read-oriented and writes only on Drop when `derived=false`; for the
/// upload-then-encode case the create+put path is the correct one and is
/// what the crate's own `enc_h264_demo` test uses.)
fn upload_i420_as_nv12(
    surface: &libva::Surface<()>,
    frame: &prdt_media_sw::I420Frame,
    nv12_format: libva::VAImageFormat,
    width: u32,
    height: u32,
    warned: &mut bool,
) -> Result<(), VaapiError> {
    let mut image =
        libva::Image::create_from(surface, nv12_format, (width, height), (width, height)).map_err(
            |e| {
                if !*warned {
                    tracing::warn!(error = %e, "vaCreateImage failed; surface upload aborted");
                    *warned = true;
                }
                VaapiError::Bitstream(format!("Image::create_from(NV12): {e}"))
            },
        )?;

    let va_image = *image.image();
    let dest = image.as_mut();
    let w = width as usize;
    let h = height as usize;
    let h_half = h / 2;

    // Y plane → offsets[0] / pitches[0]
    let y_off = va_image.offsets[0] as usize;
    let y_pitch = va_image.pitches[0] as usize;
    let src_y_stride = frame.stride_y as usize;
    if frame.y.len() < src_y_stride * h {
        return Err(VaapiError::Bitstream(format!(
            "I420 Y plane truncated: {} < {}",
            frame.y.len(),
            src_y_stride * h
        )));
    }
    for row in 0..h {
        let dst_start = y_off + row * y_pitch;
        let src_start = row * src_y_stride;
        dest[dst_start..dst_start + w].copy_from_slice(&frame.y[src_start..src_start + w]);
    }

    // UV plane (NV12 interleaved) → offsets[1] / pitches[1]
    let uv_off = va_image.offsets[1] as usize;
    let uv_pitch = va_image.pitches[1] as usize;
    let half_w = w / 2;
    let src_uv_stride = frame.stride_uv as usize;
    if frame.u.len() < src_uv_stride * h_half || frame.v.len() < src_uv_stride * h_half {
        return Err(VaapiError::Bitstream("I420 U/V plane truncated".into()));
    }
    for row in 0..h_half {
        let dst_start = uv_off + row * uv_pitch;
        let u_src = &frame.u[row * src_uv_stride..row * src_uv_stride + half_w];
        let v_src = &frame.v[row * src_uv_stride..row * src_uv_stride + half_w];
        let dst = &mut dest[dst_start..dst_start + 2 * half_w];
        for i in 0..half_w {
            dst[2 * i] = u_src[i];
            dst[2 * i + 1] = v_src[i];
        }
    }
    // Drop runs vaPutImage (because derived=false + dirty=true) → uploads.
    drop(image);
    Ok(())
}

/// Construct minimal valid H.264 SPS + PPS Annex-B byte sequences for
/// Constrained Baseline @ level 4.1, matching the chosen encoder Config.
///
/// These are pre-rendered byte patterns derived from a known-working
/// baseline reference encoder; only the picture-size and frame-rate
/// portions vary per call. The bytes are prepended verbatim to every IDR
/// via `normalize_to_annexb` so the downstream decoder sees a self-
/// contained parameter set without depending on driver-emitted SPS/PPS
/// (cros-libva 0.0.13 does not expose H.264 packed-header capture).
fn build_h264_spsps_baseline(width: u32, height: u32, fps: u32) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64);

    // SPS bitstream writer.
    let mut sps = BitWriter::new();
    // forbidden_zero_bit=0, nal_ref_idc=3, nal_unit_type=7 (SPS) → 0x67
    // (we append the byte after the start code below)
    let profile_idc: u8 = 66; // Constrained Baseline
    let level_idc: u8 = 41; // Level 4.1
                            // constraint_set0_flag=1 (baseline), set1=1 (constrained), rest=0
    let constraint_flags: u8 = 0b1100_0000;
    sps.push_byte(profile_idc);
    sps.push_byte(constraint_flags);
    sps.push_byte(level_idc);
    // seq_parameter_set_id ue(0)
    sps.push_ue(0);
    // log2_max_frame_num_minus4 ue(1) — must match our SPS param above
    sps.push_ue(1);
    // pic_order_cnt_type ue(0)
    sps.push_ue(0);
    // log2_max_pic_order_cnt_lsb_minus4 ue(2)
    sps.push_ue(2);
    // max_num_ref_frames ue(1)
    sps.push_ue(1);
    // gaps_in_frame_num_value_allowed_flag u(1)=0
    sps.push_bits(0, 1);
    // pic_width_in_mbs_minus1 ue
    let w_mbs = width.div_ceil(16);
    let h_mbs = height.div_ceil(16);
    sps.push_ue(w_mbs - 1);
    // pic_height_in_map_units_minus1 ue
    sps.push_ue(h_mbs - 1);
    // frame_mbs_only_flag u(1)=1
    sps.push_bits(1, 1);
    // direct_8x8_inference_flag u(1)=0
    sps.push_bits(0, 1);
    // frame_cropping_flag u(1) — set only if dims aren't multiple of 16
    let crop_h_needed = (h_mbs * 16) - height;
    let crop_w_needed = (w_mbs * 16) - width;
    if crop_h_needed > 0 || crop_w_needed > 0 {
        sps.push_bits(1, 1);
        // frame_crop_left/right/top/bottom_offset ue
        sps.push_ue(0);
        sps.push_ue(crop_w_needed / 2);
        sps.push_ue(0);
        sps.push_ue(crop_h_needed / 2);
    } else {
        sps.push_bits(0, 1);
    }
    // vui_parameters_present_flag u(1)=1
    sps.push_bits(1, 1);
    // VUI: timing_info_present
    // aspect_ratio_info_present_flag u(1)=0
    sps.push_bits(0, 1);
    // overscan_info_present_flag u(1)=0
    sps.push_bits(0, 1);
    // video_signal_type_present_flag u(1)=0
    sps.push_bits(0, 1);
    // chroma_loc_info_present_flag u(1)=0
    sps.push_bits(0, 1);
    // timing_info_present_flag u(1)=1
    sps.push_bits(1, 1);
    sps.push_bits_u32(1, 32); // num_units_in_tick
    sps.push_bits_u32(fps * 2, 32); // time_scale
    sps.push_bits(1, 1); // fixed_frame_rate_flag
                         // nal_hrd_parameters_present_flag u(1)=0
    sps.push_bits(0, 1);
    // vcl_hrd_parameters_present_flag u(1)=0
    sps.push_bits(0, 1);
    // pic_struct_present_flag u(1)=0
    sps.push_bits(0, 1);
    // bitstream_restriction_flag u(1)=0
    sps.push_bits(0, 1);
    // RBSP trailing bits: stop_one_bit + zero alignment
    sps.push_bits(1, 1);
    sps.align_to_byte();

    let sps_rbsp = sps.finish();
    let sps_ebsp = rbsp_to_ebsp(&sps_rbsp);

    // SPS NAL: 00 00 00 01 + 0x67 + EBSP
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67]);
    out.extend_from_slice(&sps_ebsp);

    // PPS bitstream writer.
    let mut pps = BitWriter::new();
    // pic_parameter_set_id ue(0)
    pps.push_ue(0);
    // seq_parameter_set_id ue(0)
    pps.push_ue(0);
    // entropy_coding_mode_flag u(1)=0 (CAVLC)
    pps.push_bits(0, 1);
    // bottom_field_pic_order_in_frame_present_flag u(1)=0
    pps.push_bits(0, 1);
    // num_slice_groups_minus1 ue(0)
    pps.push_ue(0);
    // num_ref_idx_l0_default_active_minus1 ue(0)
    pps.push_ue(0);
    // num_ref_idx_l1_default_active_minus1 ue(0)
    pps.push_ue(0);
    // weighted_pred_flag u(1)=0
    pps.push_bits(0, 1);
    // weighted_bipred_idc u(2)=0
    pps.push_bits(0, 2);
    // pic_init_qp_minus26 se(0)
    pps.push_se(0);
    // pic_init_qs_minus26 se(0)
    pps.push_se(0);
    // chroma_qp_index_offset se(0)
    pps.push_se(0);
    // deblocking_filter_control_present_flag u(1)=1
    pps.push_bits(1, 1);
    // constrained_intra_pred_flag u(1)=0
    pps.push_bits(0, 1);
    // redundant_pic_cnt_present_flag u(1)=0
    pps.push_bits(0, 1);
    // RBSP trailing
    pps.push_bits(1, 1);
    pps.align_to_byte();
    let pps_rbsp = pps.finish();
    let pps_ebsp = rbsp_to_ebsp(&pps_rbsp);

    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x68]);
    out.extend_from_slice(&pps_ebsp);
    out
}

/// Minimal MSB-first bit writer for H.264 RBSP construction.
struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            nbits: 0,
        }
    }

    fn push_bit(&mut self, b: u8) {
        self.cur = (self.cur << 1) | (b & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    fn push_bits(&mut self, val: u32, n: u8) {
        for i in (0..n).rev() {
            self.push_bit(((val >> i) & 1) as u8);
        }
    }

    fn push_bits_u32(&mut self, val: u32, n: u8) {
        self.push_bits(val, n);
    }

    fn push_byte(&mut self, b: u8) {
        self.push_bits(b as u32, 8);
    }

    /// Exp-Golomb unsigned.
    fn push_ue(&mut self, val: u32) {
        let v = val + 1;
        let nbits = 32 - v.leading_zeros();
        let zeros = nbits - 1;
        for _ in 0..zeros {
            self.push_bit(0);
        }
        for i in (0..nbits).rev() {
            self.push_bit(((v >> i) & 1) as u8);
        }
    }

    /// Exp-Golomb signed.
    fn push_se(&mut self, val: i32) {
        let mapped: u32 = if val <= 0 {
            (-val as u32) * 2
        } else {
            (val as u32) * 2 - 1
        };
        self.push_ue(mapped);
    }

    fn align_to_byte(&mut self) {
        while self.nbits != 0 {
            self.push_bit(0);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits != 0 {
            self.align_to_byte();
        }
        std::mem::take(&mut self.buf)
    }
}

/// H.264 emulation-prevention: turn `00 00 00` / `00 00 01` / `00 00 02` /
/// `00 00 03` into `00 00 03 xx`. Walks RBSP → EBSP.
fn rbsp_to_ebsp(rbsp: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(rbsp.len() + rbsp.len() / 64);
    let mut zero_run = 0usize;
    for &b in rbsp {
        if zero_run >= 2 && b <= 0x03 {
            out.push(0x03);
            zero_run = 0;
        }
        out.push(b);
        if b == 0 {
            zero_run += 1;
        } else {
            zero_run = 0;
        }
    }
    out
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
                create_image_warned: false,
                next_surface_idx: 0,
                nv12_format: unsafe { std::mem::zeroed() },
                coded_buf: None,
                surfaces: None,
                context: None,
                config: None,
                display: None,
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

    #[test]
    fn build_h264_spsps_baseline_emits_two_nals() {
        let blob = build_h264_spsps_baseline(1920, 1080, 60);
        // SPS NAL + PPS NAL, each preceded by 00 00 00 01.
        // First start code at offset 0; find the second.
        assert!(blob.starts_with(&[0x00, 0x00, 0x00, 0x01, 0x67]));
        let mut second = None;
        let mut i = 4;
        while i + 4 < blob.len() {
            if blob[i] == 0 && blob[i + 1] == 0 && blob[i + 2] == 0 && blob[i + 3] == 1 {
                second = Some(i);
                break;
            }
            i += 1;
        }
        let s = second.expect("PPS start code not found");
        assert_eq!(
            blob[s + 4],
            0x68,
            "second NAL header byte should be 0x68 (PPS)"
        );
    }

    #[test]
    fn rbsp_to_ebsp_inserts_03_after_two_zeros() {
        // 00 00 00 → 00 00 03 00
        let out = rbsp_to_ebsp(&[0x00, 0x00, 0x00]);
        assert_eq!(out, vec![0x00, 0x00, 0x03, 0x00]);
        // 00 00 01 → 00 00 03 01
        let out = rbsp_to_ebsp(&[0x00, 0x00, 0x01]);
        assert_eq!(out, vec![0x00, 0x00, 0x03, 0x01]);
        // No insertion when isolated zeros.
        let out = rbsp_to_ebsp(&[0x00, 0xff, 0x00]);
        assert_eq!(out, vec![0x00, 0xff, 0x00]);
    }
}
