// NVENC structs are ~300+ fields of reserved padding. Filling them field
// by field after Default::default() is clearer than an initializer listing
// every required field — and matches the C sample's memset + assign idiom.
#![allow(clippy::field_reassign_with_default)]

//! Safe Rust wrapper around the NVENC H.265 encoder state machine.
//!
//! Lifecycle:
//! ```text
//!   NvencEncoder::new(device, config)
//!      |-- NvEncodeAPICreateInstance -> NV_ENCODE_API_FUNCTION_LIST
//!      |-- nvEncOpenEncodeSessionEx(D3D11_DEVICE) -> session
//!      |-- nvEncInitializeEncoder(session, params)
//!      `-- nvEncCreateBitstreamBuffer(session)
//!
//!   encoder.encode(&texture, force_idr, timestamp)
//!      |-- RegisterResource / MapInputResource
//!      |-- EncodePicture (synchronous)
//!      |-- LockBitstream -> copy out bytes -> UnlockBitstream
//!      `-- UnmapInputResource / UnregisterResource
//!
//!   drop(encoder)
//!      |-- nvEncDestroyBitstreamBuffer
//!      `-- nvEncDestroyEncoder
//! ```
//!
//! For Phase 0 simplicity the register/map is done per-frame. A later
//! optimization can cache registered inputs across frames (NV sample
//! `AppEncD3D11` uses a small ring of pre-registered shared textures).

use std::ptr;

use windows::core::Interface;

use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::{MediaError, Result};
use crate::nvenc::config::{
    nv_enc_create_bitstream_buffer_ver, nv_enc_lock_bitstream_ver, nv_enc_map_input_resource_ver,
    nv_enc_open_encode_session_ex_params_ver, nv_enc_pic_params_ver, nv_enc_register_resource_ver,
    nv_encode_api_function_list_ver, InitParams, NvencEncoderConfig,
};
use crate::nvenc::ffi;
use crate::nvenc::loader::NvEncLibrary;

pub struct EncodedH265Frame {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp: u64,
}

/// Convert the i32-repr `NVENCSTATUS` returned by NVENC fns to a plain i32
/// for easy logging and comparison. `NV_ENC_SUCCESS == 0`.
fn status_i32(status: ffi::NVENCSTATUS) -> i32 {
    status as i32
}

/// Return `Ok(())` if status == NV_ENC_SUCCESS, otherwise a `MediaError::Other`
/// describing the failed call.
fn check_status(fn_name: &str, status: ffi::NVENCSTATUS) -> Result<()> {
    if status_i32(status) == 0 {
        Ok(())
    } else {
        Err(MediaError::Other(format!(
            "{fn_name} failed: NVENCSTATUS={}",
            status_i32(status)
        )))
    }
}

pub struct NvencEncoder {
    fn_table: ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    bitstream_buffer: ffi::NV_ENC_OUTPUT_PTR,
    #[allow(dead_code)]
    config: NvencEncoderConfig,
    /// Keep init params alive so the `encodeConfig` pointer inside `params`
    /// remains valid for the life of the session (NVENC does not copy it).
    _init_params: InitParams,
    _dev: D3d11Device,
}

impl NvencEncoder {
    pub fn new(dev: &D3d11Device, cfg: &NvencEncoderConfig) -> Result<Self> {
        let lib = NvEncLibrary::load()?;

        unsafe {
            // Step 1: populate function table via NvEncodeAPICreateInstance.
            // Use Default (bindgen write_bytes) rather than mem::zeroed to
            // avoid UB on structs containing unions.
            let mut fn_table: ffi::NV_ENCODE_API_FUNCTION_LIST =
                ffi::NV_ENCODE_API_FUNCTION_LIST::default();
            fn_table.version = nv_encode_api_function_list_ver();
            let raw_status = lib.create_instance(&mut fn_table as *mut _ as *mut _);
            if raw_status != 0 {
                return Err(MediaError::Other(format!(
                    "NvEncodeAPICreateInstance failed: status={raw_status}"
                )));
            }

            // Step 2: open encode session on the D3D11 device.
            let mut session_params: ffi::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS =
                ffi::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS::default();
            session_params.version = nv_enc_open_encode_session_ex_params_ver();
            session_params.deviceType = ffi::NV_ENC_DEVICE_TYPE::NV_ENC_DEVICE_TYPE_DIRECTX;
            session_params.device = dev.device().as_raw() as *mut _;
            session_params.apiVersion = ffi::NVENCAPI_VERSION;

            let open_fn = fn_table.nvEncOpenEncodeSessionEx.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncOpenEncodeSessionEx is null".into())
            })?;
            let mut session: *mut std::ffi::c_void = ptr::null_mut();
            check_status(
                "nvEncOpenEncodeSessionEx",
                open_fn(&mut session_params, &mut session),
            )?;

            // Guard: if anything below fails, destroy the session we just
            // opened so we don't leak it.
            let destroy_fn = fn_table.nvEncDestroyEncoder;
            let mut session_guard = SessionGuard {
                session: Some(session),
                destroy_fn,
            };

            // Step 3a: Query the preset config so we use the codec-specific
            // defaults as our baseline. This mirrors the NvEncoder sample's
            // CreateDefaultEncoderParams flow; skipping this is the #1
            // cause of NV_ENC_ERR_INVALID_PARAM during InitializeEncoder
            // because HEVC-specific encodeCodecConfig fields stay zeroed.
            let mut init_params = InitParams::new_hevc_ull(cfg);

            let preset_fn = fn_table.nvEncGetEncodePresetConfigEx.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncGetEncodePresetConfigEx is null".into())
            })?;
            let mut preset_cfg: ffi::NV_ENC_PRESET_CONFIG = ffi::NV_ENC_PRESET_CONFIG::default();
            preset_cfg.version = crate::nvenc::config::nv_enc_preset_config_ver();
            preset_cfg.presetCfg.version = crate::nvenc::config::nv_enc_config_ver_public();
            check_status(
                "nvEncGetEncodePresetConfigEx",
                preset_fn(
                    session,
                    crate::nvenc::config::NV_ENC_CODEC_HEVC_GUID,
                    crate::nvenc::config::NV_ENC_PRESET_P1_GUID,
                    ffi::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                    &mut preset_cfg,
                ),
            )?;
            // Overlay preset defaults onto our config, then re-apply our
            // user-facing overrides (CBR bitrate, GOP length, frameIntervalP,
            // struct version).
            *init_params.config = preset_cfg.presetCfg;
            init_params.config.version = crate::nvenc::config::nv_enc_config_ver_public();
            init_params.config.rcParams.rateControlMode =
                ffi::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
            init_params.config.rcParams.averageBitRate = cfg.bitrate_bps;
            init_params.config.rcParams.maxBitRate = cfg.bitrate_bps;
            init_params.config.rcParams.vbvBufferSize = cfg.bitrate_bps / cfg.fps_numerator.max(1);
            init_params.config.rcParams.vbvInitialDelay = init_params.config.rcParams.vbvBufferSize;
            init_params.config.gopLength = cfg.gop_length;
            init_params.config.frameIntervalP = 1;
            // After overwriting encodeConfig, we must reinstall the pointer
            // inside the init params (the Box itself is still alive).
            init_params.params.encodeConfig = &mut *init_params.config as *mut _;

            // Step 3b: initialize the encoder.
            let init_fn = fn_table.nvEncInitializeEncoder.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncInitializeEncoder is null".into())
            })?;
            check_status(
                "nvEncInitializeEncoder",
                init_fn(session, &mut init_params.params),
            )?;

            // Step 4: create one reusable output bitstream buffer. NVENC
            // manages capacity internally; we just get a handle.
            let mut buf_params: ffi::NV_ENC_CREATE_BITSTREAM_BUFFER =
                ffi::NV_ENC_CREATE_BITSTREAM_BUFFER::default();
            buf_params.version = nv_enc_create_bitstream_buffer_ver();

            let create_buf_fn = fn_table.nvEncCreateBitstreamBuffer.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncCreateBitstreamBuffer is null".into())
            })?;
            check_status(
                "nvEncCreateBitstreamBuffer",
                create_buf_fn(session, &mut buf_params),
            )?;

            // All constructors succeeded: disarm the guard and hand the
            // session over to the encoder struct.
            session_guard.session = None;

            Ok(Self {
                fn_table,
                session,
                bitstream_buffer: buf_params.bitstreamBuffer,
                config: *cfg,
                _init_params: init_params,
                _dev: dev.clone(),
            })
        }
    }

    /// Encode one D3D11 texture into a standalone H.265 access unit.
    ///
    /// `force_idr == true` emits an IDR frame plus VPS/SPS/PPS headers.
    /// In sync mode the call returns only after encode is complete and
    /// the bitstream is ready to be locked.
    pub fn encode(
        &self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp: u64,
    ) -> Result<EncodedH265Frame> {
        unsafe {
            // ---- RegisterResource ---------------------------------------
            let mut reg: ffi::NV_ENC_REGISTER_RESOURCE = ffi::NV_ENC_REGISTER_RESOURCE::default();
            reg.version = nv_enc_register_resource_ver();
            reg.resourceType = ffi::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            reg.width = texture.width();
            reg.height = texture.height();
            reg.pitch = 0; // required 0 for DirectX resources
            reg.resourceToRegister = texture.raw().as_raw() as *mut _;
            // DXGI_FORMAT_B8G8R8A8_UNORM matches NV_ENC_BUFFER_FORMAT_ARGB
            // (word-order ARGB means byte order B,G,R,A in memory).
            reg.bufferFormat = ffi::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB;

            let register_fn = self.fn_table.nvEncRegisterResource.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncRegisterResource is null".into())
            })?;
            check_status("nvEncRegisterResource", register_fn(self.session, &mut reg))?;

            let _reg_guard = RegGuard {
                fn_table: &self.fn_table,
                session: self.session,
                resource: reg.registeredResource,
            };

            // ---- MapInputResource ---------------------------------------
            let mut map: ffi::NV_ENC_MAP_INPUT_RESOURCE = ffi::NV_ENC_MAP_INPUT_RESOURCE::default();
            map.version = nv_enc_map_input_resource_ver();
            map.registeredResource = reg.registeredResource;
            let map_fn = self.fn_table.nvEncMapInputResource.ok_or_else(|| {
                MediaError::Other("fn_table.nvEncMapInputResource is null".into())
            })?;
            check_status("nvEncMapInputResource", map_fn(self.session, &mut map))?;

            let _map_guard = MapGuard {
                fn_table: &self.fn_table,
                session: self.session,
                mapped: map.mappedResource,
            };

            // ---- EncodePicture ------------------------------------------
            let mut pic: ffi::NV_ENC_PIC_PARAMS = ffi::NV_ENC_PIC_PARAMS::default();
            pic.version = nv_enc_pic_params_ver();
            pic.inputWidth = texture.width();
            pic.inputHeight = texture.height();
            pic.inputPitch = texture.width(); // unused for DirectX inputs
            pic.inputBuffer = map.mappedResource;
            pic.outputBitstream = self.bitstream_buffer;
            pic.bufferFmt = ffi::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_ARGB;
            pic.pictureStruct = ffi::NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME;
            pic.inputTimeStamp = timestamp;
            if force_idr {
                pic.encodePicFlags = (ffi::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR as u32)
                    | (ffi::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_OUTPUT_SPSPPS as u32);
                pic.pictureType = ffi::NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR;
            }

            let encode_fn = self
                .fn_table
                .nvEncEncodePicture
                .ok_or_else(|| MediaError::Other("fn_table.nvEncEncodePicture is null".into()))?;
            let status = encode_fn(self.session, &mut pic);
            let status_code = status_i32(status);
            // In sync mode NV_ENC_SUCCESS is expected. NV_ENC_ERR_NEED_MORE_INPUT(17)
            // is tolerated but would indicate B-frame buffering (disabled here).
            if status_code != 0 && status_code != 17 {
                return Err(MediaError::Other(format!(
                    "nvEncEncodePicture failed: NVENCSTATUS={status_code}"
                )));
            }

            // ---- LockBitstream / copy / UnlockBitstream -----------------
            let mut lock: ffi::NV_ENC_LOCK_BITSTREAM = ffi::NV_ENC_LOCK_BITSTREAM::default();
            lock.version = nv_enc_lock_bitstream_ver();
            lock.outputBitstream = self.bitstream_buffer;
            let lock_fn = self
                .fn_table
                .nvEncLockBitstream
                .ok_or_else(|| MediaError::Other("fn_table.nvEncLockBitstream is null".into()))?;
            check_status("nvEncLockBitstream", lock_fn(self.session, &mut lock))?;

            let nal_bytes = std::slice::from_raw_parts(
                lock.bitstreamBufferPtr as *const u8,
                lock.bitstreamSizeInBytes as usize,
            )
            .to_vec();
            let is_keyframe = matches!(
                lock.pictureType,
                ffi::NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR | ffi::NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_I
            );

            let unlock_fn = self
                .fn_table
                .nvEncUnlockBitstream
                .ok_or_else(|| MediaError::Other("fn_table.nvEncUnlockBitstream is null".into()))?;
            check_status(
                "nvEncUnlockBitstream",
                unlock_fn(self.session, self.bitstream_buffer),
            )?;

            Ok(EncodedH265Frame {
                nal_bytes,
                is_keyframe,
                timestamp,
            })
        }
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.bitstream_buffer.is_null() {
                if let Some(destroy_buf) = self.fn_table.nvEncDestroyBitstreamBuffer {
                    destroy_buf(self.session, self.bitstream_buffer);
                }
            }
            if !self.session.is_null() {
                if let Some(destroy) = self.fn_table.nvEncDestroyEncoder {
                    destroy(self.session);
                }
            }
        }
    }
}

// NVENC sessions are bound to a D3D11 device; the NVENC driver documentation
// states a single session may be used from multiple threads provided the
// client serializes access. Our `&self` encode path offers no such
// serialization, so tight parallel use needs an external Mutex. For
// Send/Sync purposes we advertise both: the underlying handles are thread-
// safe to move.
unsafe impl Send for NvencEncoder {}
unsafe impl Sync for NvencEncoder {}

// ---------------------------------------------------------------------
// RAII guards for the per-encode register/map pair.
// ---------------------------------------------------------------------

struct SessionGuard {
    session: Option<*mut std::ffi::c_void>,
    destroy_fn: ffi::PNVENCDESTROYENCODER,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            unsafe {
                if let Some(f) = self.destroy_fn {
                    f(s);
                }
            }
        }
    }
}

struct RegGuard<'a> {
    fn_table: &'a ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    resource: ffi::NV_ENC_REGISTERED_PTR,
}

impl Drop for RegGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(f) = self.fn_table.nvEncUnregisterResource {
                f(self.session, self.resource);
            }
        }
    }
}

struct MapGuard<'a> {
    fn_table: &'a ffi::NV_ENCODE_API_FUNCTION_LIST,
    session: *mut std::ffi::c_void,
    mapped: ffi::NV_ENC_INPUT_PTR,
}

impl Drop for MapGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            if let Some(f) = self.fn_table.nvEncUnmapInputResource {
                f(self.session, self.mapped);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;
    use crate::synthetic::make_counter_texture;

    #[test]
    fn create_and_destroy_encoder() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipping: no adapter ({e})");
                return;
            }
        };
        if !adapter.is_nvidia() {
            eprintln!("skipping: non-NVIDIA adapter ({})", adapter.name);
            return;
        }
        let dev = D3d11Device::create(&adapter).expect("D3D11 device");
        let cfg = NvencEncoderConfig {
            width: 1920,
            height: 1080,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps: 20_000_000,
            gop_length: 60,
        };
        match NvencEncoder::new(&dev, &cfg) {
            Ok(enc) => drop(enc),
            Err(e) => panic!("encoder creation: {e}"),
        }
    }

    #[test]
    fn encode_single_frame_produces_bytes() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipping: no adapter ({e})");
                return;
            }
        };
        if !adapter.is_nvidia() {
            eprintln!("skipping: non-NVIDIA adapter ({})", adapter.name);
            return;
        }
        let dev = D3d11Device::create(&adapter).expect("D3D11 device");
        let cfg = NvencEncoderConfig {
            width: 256,
            height: 256,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps: 5_000_000,
            gop_length: 60,
        };
        let enc = NvencEncoder::new(&dev, &cfg).expect("encoder");
        let tex = make_counter_texture(&dev, 256, 256, 0).expect("texture");
        let frame = enc.encode(&tex, true, 0).expect("encode");
        eprintln!(
            "first IDR frame: {} bytes, is_keyframe={}",
            frame.nal_bytes.len(),
            frame.is_keyframe
        );
        assert!(!frame.nal_bytes.is_empty(), "expected non-empty NAL output");
        assert!(frame.is_keyframe, "first frame should be keyframe");
        // H.265 Annex-B NAL must start with 00 00 00 01 or 00 00 01.
        let has_start_code =
            frame.nal_bytes.starts_with(&[0, 0, 0, 1]) || frame.nal_bytes.starts_with(&[0, 0, 1]);
        assert!(
            has_start_code,
            "missing NAL start code: {:02x?}",
            &frame.nal_bytes[..8.min(frame.nal_bytes.len())]
        );
    }
}
