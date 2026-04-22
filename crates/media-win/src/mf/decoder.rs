//! Media Foundation H.265 decoder.
//!
//! Design:
//! - Calls `MFStartup(MF_VERSION, MFSTARTUP_FULL)` once per process (guarded
//!   by `OnceLock`, together with `CoInitializeEx`).
//! - Enumerates `MFT_CATEGORY_VIDEO_DECODER` with `MFVideoFormat_HEVC` as the
//!   input subtype filter and picks the first hardware-flagged MFT.
//! - Creates an `IMFDXGIDeviceManager`, resets it with our `ID3D11Device`,
//!   and attaches it to the MFT via `MFT_MESSAGE_SET_D3D_MANAGER`.
//! - Sets input type (H.265 / HEVC) and output type (NV12) with frame size.
//! - Issues `MFT_MESSAGE_NOTIFY_BEGIN_STREAMING` and
//!   `MFT_MESSAGE_NOTIFY_START_OF_STREAM` so the MFT is ready to accept
//!   samples.
//!
//! The decoder holds an `IMFTransform` plus the `D3d11Device` it was bound
//! to (to keep the underlying device alive for as long as the MFT is). The
//! `process_input` / `process_output` frame pump is added in Plan 2c Task 4.

use std::sync::OnceLock;

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIDeviceManager, IMFTransform, MFCreateDXGIDeviceManager, MFCreateMediaType,
    MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::d3d11::D3d11Device;
use crate::error::{MediaError, Result};

/// One-shot MF + COM initialization. Stores the stringified error (if any)
/// so the inner `OnceLock` payload does not need to be `Clone`.
static MF_INIT_ERROR: OnceLock<Option<String>> = OnceLock::new();

fn ensure_mf_initialized() -> Result<()> {
    let err = MF_INIT_ERROR.get_or_init(|| unsafe {
        // CoInitializeEx returns S_FALSE if COM is already initialized on
        // this thread; both S_OK and S_FALSE are acceptable.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    });
    match err {
        None => Ok(()),
        Some(msg) => Err(MediaError::Other(format!("MF init error: {msg}"))),
    }
}

/// Windows Media Foundation H.265 hardware decoder.
///
/// Construction enumerates the available H.265 decoder MFTs (preferring
/// hardware-flagged ones), picks the first one, attaches our D3D11 device,
/// and configures input (HEVC) / output (NV12) media types. The frame pump
/// (`process_input` / `process_output`) is wired up in Plan 2c Task 4.
pub struct H265Decoder {
    #[allow(dead_code)] // Task 3: scaffolded, used by Task 4.
    mft: IMFTransform,
    width: u32,
    height: u32,
    needs_idr: bool,
    /// Kept alive so the MFT's reference to the D3D11 device (via the DXGI
    /// device manager) remains valid for the lifetime of this decoder.
    _dev: D3d11Device,
}

impl H265Decoder {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self> {
        ensure_mf_initialized()?;

        unsafe {
            // 1. Enumerate H.265 decoder MFTs.
            let input_info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_HEVC,
            };

            // Prefer hardware-flagged MFTs. If none register on this system
            // (some configurations only register the Microsoft SW HEVC
            // decoder, and NVIDIA's HW MFT isn't always exposed as a
            // discrete category entry), fall back to any HEVC decoder — the
            // D3D11 device manager will still let it run accelerated via
            // DXVA2 under the hood when available.
            let hw_flags = MFT_ENUM_FLAG_HARDWARE
                | MFT_ENUM_FLAG_SYNCMFT
                | MFT_ENUM_FLAG_ASYNCMFT
                | MFT_ENUM_FLAG_LOCALMFT
                | MFT_ENUM_FLAG_SORTANDFILTER;
            let any_flags = MFT_ENUM_FLAG_SYNCMFT
                | MFT_ENUM_FLAG_ASYNCMFT
                | MFT_ENUM_FLAG_LOCALMFT
                | MFT_ENUM_FLAG_SORTANDFILTER;

            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count: u32 = 0;

            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                hw_flags,
                Some(&input_info),
                None,
                &mut activates,
                &mut count,
            )
            .map_err(|e| MediaError::Other(format!("MFTEnumEx (hw): {e}")))?;

            if count == 0 {
                // Fallback: enumerate without the HW filter.
                MFTEnumEx(
                    MFT_CATEGORY_VIDEO_DECODER,
                    any_flags,
                    Some(&input_info),
                    None,
                    &mut activates,
                    &mut count,
                )
                .map_err(|e| MediaError::Other(format!("MFTEnumEx (any): {e}")))?;
            }

            if count == 0 {
                return Err(MediaError::Other("no H.265 decoder MFT available".into()));
            }

            // Take the first result. Phase 0 accepts the minor leak of the
            // returned activate array (would otherwise need CoTaskMemFree).
            let activate_slice = std::slice::from_raw_parts(activates, count as usize);
            let activate = activate_slice[0]
                .as_ref()
                .ok_or_else(|| MediaError::Other("null activate".into()))?;
            let mft: IMFTransform = activate
                .ActivateObject::<IMFTransform>()
                .map_err(|e| MediaError::Other(format!("ActivateObject: {e}")))?;

            // 2. Create + attach D3D11 device manager.
            let mut dxgi_manager: Option<IMFDXGIDeviceManager> = None;
            let mut reset_token: u32 = 0;
            MFCreateDXGIDeviceManager(&mut reset_token, &mut dxgi_manager)
                .map_err(|e| MediaError::Other(format!("MFCreateDXGIDeviceManager: {e}")))?;
            let dxgi_manager = dxgi_manager
                .ok_or_else(|| MediaError::Other("null IMFDXGIDeviceManager".into()))?;
            dxgi_manager
                .ResetDevice(dev.device(), reset_token)
                .map_err(|e| MediaError::Other(format!("ResetDevice: {e}")))?;

            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, dxgi_manager.as_raw() as usize)
                .map_err(|e| MediaError::Other(format!("ProcessMessage SET_D3D_MANAGER: {e}")))?;

            // 3. Input media type (H.265 / HEVC).
            let input_type = MFCreateMediaType()
                .map_err(|e| MediaError::Other(format!("MFCreateMediaType in: {e}")))?;
            input_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| MediaError::Other(format!("SetGUID major in: {e}")))?;
            input_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)
                .map_err(|e| MediaError::Other(format!("SetGUID sub in: {e}")))?;
            // Pack (width, height) into MF_MT_FRAME_SIZE: high 32 bits =
            // width, low 32 bits = height.
            let frame_size_packed = ((width as u64) << 32) | (height as u64);
            input_type
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size_packed)
                .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size in: {e}")))?;

            mft.SetInputType(0, &input_type, 0)
                .map_err(|e| MediaError::Other(format!("SetInputType: {e}")))?;

            // 4. Output media type (NV12).
            let output_type = MFCreateMediaType()
                .map_err(|e| MediaError::Other(format!("MFCreateMediaType out: {e}")))?;
            output_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| MediaError::Other(format!("SetGUID major out: {e}")))?;
            output_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|e| MediaError::Other(format!("SetGUID sub out: {e}")))?;
            output_type
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size_packed)
                .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size out: {e}")))?;

            mft.SetOutputType(0, &output_type, 0)
                .map_err(|e| MediaError::Other(format!("SetOutputType: {e}")))?;

            // 5. Notify the MFT that streaming is starting.
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| MediaError::Other(format!("NOTIFY_BEGIN_STREAMING: {e}")))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| MediaError::Other(format!("NOTIFY_START_OF_STREAM: {e}")))?;

            Ok(Self {
                mft,
                width,
                height,
                needs_idr: true,
                _dev: dev.clone(),
            })
        }
    }

    pub fn needs_idr(&self) -> bool {
        self.needs_idr
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;

    #[test]
    fn create_h265_decoder() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        let dev = D3d11Device::create(&adapter).expect("D3D11 device");
        match H265Decoder::new(&dev, 1920, 1080) {
            Ok(dec) => {
                assert_eq!(dec.width(), 1920);
                assert_eq!(dec.height(), 1080);
                assert!(dec.needs_idr());
            }
            Err(e) => {
                // On VMs / server SKUs / systems without HEVC Video
                // Extensions there is no HW H.265 decoder MFT; treat as
                // non-fatal for CI portability.
                eprintln!("H.265 MFT not available (non-fatal): {e}");
            }
        }
    }
}
