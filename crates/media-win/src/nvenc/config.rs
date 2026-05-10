// See encoder.rs for rationale; same applies here.
#![allow(clippy::field_reassign_with_default)]

//! NVENC H.265 low-latency encoder configuration.
//!
//! Produces a pre-filled `NV_ENC_INITIALIZE_PARAMS` for the power-remote-dt
//! use case: HEVC Main Profile, P1 preset, ultra-low-latency tuning, CBR.
//!
//! The generated bindgen output does NOT expose the `NV_ENC_*_VER` constants
//! as pub constants (they are C macros referenced only in doc strings).
//! This module reimplements the `NVENCAPI_STRUCT_VERSION(ver)` macro in Rust
//! so we can stamp the correct `version` field on every NVENC input struct.
//!
//! Authoritative values from `nvEncodeAPI.h` (SDK 13.0.37):
//!   NVENCAPI_VERSION                         = MAJOR(13) | (MINOR(0) << 24) = 13
//!   NVENCAPI_STRUCT_VERSION(v)               = NVENCAPI_VERSION | (v << 16) | (0x7 << 28)
//!   NV_ENC_INITIALIZE_PARAMS_VER             = VER(7) | (1<<31)
//!   NV_ENC_CONFIG_VER                        = VER(9) | (1<<31)
//!   NV_ENC_RC_PARAMS_VER                     = VER(1)
//!   NV_ENC_PIC_PARAMS_VER                    = VER(7) | (1<<31)
//!   NV_ENC_LOCK_BITSTREAM_VER                = VER(2) | (1<<31)
//!   NV_ENC_MAP_INPUT_RESOURCE_VER            = VER(4)
//!   NV_ENC_REGISTER_RESOURCE_VER             = VER(5)
//!   NV_ENC_CREATE_BITSTREAM_BUFFER_VER       = VER(1)
//!   NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER = VER(1)
//!   NV_ENCODE_API_FUNCTION_LIST_VER          = VER(2)

#[cfg(prdt_nvenc_bindings)]
use crate::nvenc::ffi;

// NVENC ships `static const GUID ...` definitions inside its header, so the
// bindgen-generated `extern "C" { pub static NV_ENC_... }` symbols are NOT
// exported by the DLL and would fail to link. We re-declare the GUIDs we
// actually use as plain Rust constants, matching the values in
// `nvEncodeAPI.h`:
//
//   NV_ENC_CODEC_HEVC_GUID = 790cdc88-4522-4d7b-9425-bda9975f7603
//   NV_ENC_PRESET_P1_GUID  = fc0a8d3e-45f8-4cf8-80c7-29887159.0ebf
#[cfg(prdt_nvenc_bindings)]
pub(crate) const NV_ENC_CODEC_HEVC_GUID: ffi::GUID = ffi::GUID {
    Data1: 0x790cdc88,
    Data2: 0x4522,
    Data3: 0x4d7b,
    Data4: [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
};

#[cfg(prdt_nvenc_bindings)]
pub(crate) const NV_ENC_PRESET_P1_GUID: ffi::GUID = ffi::GUID {
    Data1: 0xfc0a8d3e,
    Data2: 0x45f8,
    Data3: 0x4cf8,
    Data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};

#[derive(Debug, Clone, Copy)]
pub struct NvencEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub bitrate_bps: u32,
    /// GOP length (I-frame interval). For low-latency streaming, 1 second
    /// worth of frames is a reasonable default (e.g., 60 @ 60fps).
    pub gop_length: u32,
}

impl Default for NvencEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps: 50_000_000,
            gop_length: 60,
        }
    }
}

/// Owned pair of initialize-params + encode-config. The params struct
/// contains a raw pointer into `config`, so both must live together.
#[cfg(prdt_nvenc_bindings)]
pub struct InitParams {
    pub params: ffi::NV_ENC_INITIALIZE_PARAMS,
    pub config: Box<ffi::NV_ENC_CONFIG>,
}

#[cfg(prdt_nvenc_bindings)]
impl InitParams {
    /// Build an initialized `NV_ENC_INITIALIZE_PARAMS` for HEVC + P1 + ULL.
    ///
    /// Caller must ensure `cfg.width` and `cfg.height` are multiples of 2 for HEVC.
    pub fn new_hevc_ull(cfg: &NvencEncoderConfig) -> Self {
        // NV_ENC_CONFIG and NV_ENC_INITIALIZE_PARAMS both contain unions
        // and cannot be `mem::zeroed()` safely. The bindgen-generated
        // `Default` impls use `ptr::write_bytes(..., 0, 1)` which *is*
        // sound and matches the NVIDIA sample code's `memset` pattern.
        let mut config: Box<ffi::NV_ENC_CONFIG> = Box::default();
        config.version = nv_enc_config_ver();
        config.rcParams.version = nv_enc_rc_params_ver();
        config.rcParams.rateControlMode = ffi::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
        config.rcParams.averageBitRate = cfg.bitrate_bps;
        config.rcParams.maxBitRate = cfg.bitrate_bps;
        // VBV buffer = 1 frame at target bitrate for low-latency.
        config.rcParams.vbvBufferSize = cfg.bitrate_bps / cfg.fps_numerator.max(1);
        config.rcParams.vbvInitialDelay = config.rcParams.vbvBufferSize;
        config.gopLength = cfg.gop_length;
        config.frameIntervalP = 1; // IPP only, no B-frames

        let mut params: ffi::NV_ENC_INITIALIZE_PARAMS = ffi::NV_ENC_INITIALIZE_PARAMS::default();
        params.version = nv_enc_initialize_params_ver();
        params.encodeGUID = NV_ENC_CODEC_HEVC_GUID;
        params.presetGUID = NV_ENC_PRESET_P1_GUID;
        params.encodeWidth = cfg.width;
        params.encodeHeight = cfg.height;
        params.darWidth = cfg.width;
        params.darHeight = cfg.height;
        params.frameRateNum = cfg.fps_numerator;
        params.frameRateDen = cfg.fps_denominator;
        params.enableEncodeAsync = 0; // synchronous for Phase 0
        params.enablePTD = 1; // Picture-Type Decision by NVENC
        params.tuningInfo = ffi::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        params.encodeConfig = &mut *config as *mut _;
        // Emit VPS+SPS+PPS with every IDR access unit (NVENC SDK 13, nvEncodeAPI.h).
        // Value 1 = always prepend parameter sets to IDR NALs.
        params.enableRepeatSPSPPS = 1;

        InitParams { params, config }
    }
}

// ---------------------------------------------------------------------
// NVENC struct version helpers.
//
// `NVENCAPI_STRUCT_VERSION(v)` from the C header:
//   ((uint32_t)NVENCAPI_VERSION | ((v) << 16) | (0x7 << 28))
// where
//   NVENCAPI_VERSION = (NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24))
//
// bindgen exposes the inputs (NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION,
// NVENCAPI_VERSION) but not the per-struct `_VER` constants themselves, so we
// reimplement the macro in Rust.

#[cfg(prdt_nvenc_bindings)]
const fn nv_enc_struct_version(struct_ver: u32) -> u32 {
    // NVENCAPI_VERSION is already MAJOR | (MINOR << 24) per bindgen output.
    ffi::NVENCAPI_VERSION | (struct_ver << 16) | (0x7u32 << 28)
}

#[cfg(prdt_nvenc_bindings)]
pub(crate) fn nv_enc_initialize_params_ver() -> u32 {
    nv_enc_struct_version(7) | (1 << 31)
}

#[cfg(prdt_nvenc_bindings)]
pub(crate) fn nv_enc_config_ver() -> u32 {
    nv_enc_struct_version(9) | (1 << 31)
}

/// Public alias so encoder.rs can reference the same value without
/// duplicating the constant.
#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_config_ver_public() -> u32 {
    nv_enc_config_ver()
}

#[cfg(prdt_nvenc_bindings)]
pub(crate) fn nv_enc_rc_params_ver() -> u32 {
    nv_enc_struct_version(1)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_pic_params_ver() -> u32 {
    nv_enc_struct_version(7) | (1 << 31)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_register_resource_ver() -> u32 {
    nv_enc_struct_version(5)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_map_input_resource_ver() -> u32 {
    nv_enc_struct_version(4)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_create_bitstream_buffer_ver() -> u32 {
    nv_enc_struct_version(1)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_lock_bitstream_ver() -> u32 {
    nv_enc_struct_version(2) | (1 << 31)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_open_encode_session_ex_params_ver() -> u32 {
    nv_enc_struct_version(1)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_encode_api_function_list_ver() -> u32 {
    nv_enc_struct_version(2)
}

#[cfg(prdt_nvenc_bindings)]
pub fn nv_enc_preset_config_ver() -> u32 {
    nv_enc_struct_version(5) | (1 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(prdt_nvenc_bindings)]
    #[test]
    fn struct_version_encodes_expected_bits() {
        let v = nv_enc_struct_version(1);
        // Low byte: major(13).
        assert_eq!(v & 0xff, 13);
        // Bits 16..23 hold struct_ver.
        assert_eq!((v >> 16) & 0xff, 1);
        // Top nibble is always 0x7.
        assert_eq!((v >> 28) & 0xf, 0x7);
    }

    #[cfg(prdt_nvenc_bindings)]
    #[test]
    fn build_init_params_hevc_ull() {
        let cfg = NvencEncoderConfig::default();
        let ip = InitParams::new_hevc_ull(&cfg);
        assert_eq!(ip.params.version, nv_enc_initialize_params_ver());
        assert_eq!(ip.config.version, nv_enc_config_ver());
        assert_eq!(ip.params.encodeWidth, 1920);
        assert_eq!(ip.params.encodeHeight, 1080);
        assert_eq!(ip.params.frameRateNum, 60);
        assert_eq!(ip.params.enablePTD, 1);
        assert_eq!(ip.params.enableEncodeAsync, 0);
        assert_eq!(
            ip.params.tuningInfo as i32,
            ffi::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY as i32
        );
        assert_eq!(ip.config.gopLength, 60);
        assert_eq!(ip.config.frameIntervalP, 1);
    }
}
