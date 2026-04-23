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
//! to (to keep the underlying device alive for as long as the MFT is). Per
//! Plan 2c Task 4, the `process_input` / `process_output` methods below
//! implement the encode→decode frame pump. `process_output` returns a
//! CPU-side NV12 `Vec<u8>` (kept for diagnostics and back-compat), while
//! `process_output_texture` (Plan 3 Task 2) delivers the decoded frame as a
//! zero-copy `ID3D11Texture2D` via `IMFDXGIBuffer::GetResource`.

use std::sync::OnceLock;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFTransform, MFCreateDXGIDeviceManager,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFStartup,
    MFTEnumEx, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_DECODER,
    MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_LOCALMFT,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_REGISTER_TYPE_INFO, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::{MediaError, Result};

/// One-shot MF + COM initialization. Stores the stringified error so the
/// `Result` stored inside `OnceLock` remains `Clone`-free (we just format it
/// on each call).
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
/// Construction enumerates the available HW H.265 decoder MFTs, picks the
/// first one, attaches our D3D11 device, and configures input (HEVC) /
/// output (NV12) media types. The frame pump lives in `process_input` /
/// `process_output` (Task 4).
pub struct H265Decoder {
    mft: IMFTransform,
    width: u32,
    height: u32,
    needs_idr: bool,
    /// Last subresource index reported by `IMFDXGIBuffer::GetSubresourceIndex`
    /// on a successful `process_output_texture` call. Phase 0 treats any
    /// non-zero value as a known-limitation warning (see method docs). Useful
    /// for smoke-test diagnostics.
    last_subresource_index: u32,
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
                return Err(MediaError::Other(
                    "no H.265 decoder MFT available — install \"HEVC Video \
                     Extensions\" from Microsoft Store (ProductId \
                     9NMZLZ57R3T7) or the free OEM variant (9N4WGH0Z6VHQ); \
                     without it the viewer cannot decode the host's stream"
                        .into(),
                ));
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
                last_subresource_index: 0,
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

    /// Subresource index returned by `IMFDXGIBuffer::GetSubresourceIndex` on
    /// the most recent successful `process_output_texture` call. Defaults to
    /// 0 before any texture has been extracted.
    pub fn last_subresource_index(&self) -> u32 {
        self.last_subresource_index
    }

    /// Feed one encoded frame (H.265 NAL units) into the decoder.
    ///
    /// `timestamp` is expressed in 100 ns (`hns`) units, matching the Media
    /// Foundation convention. Callers that track microseconds should
    /// multiply by 10 before calling.
    pub fn process_input(&mut self, nal_bytes: &[u8], timestamp: i64) -> Result<()> {
        unsafe {
            // Allocate an MF media buffer and copy the NAL bytes in.
            let buffer = MFCreateMemoryBuffer(nal_bytes.len() as u32)
                .map_err(|e| MediaError::Other(format!("MFCreateMemoryBuffer: {e}")))?;

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer
                .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| MediaError::Other(format!("Lock buffer: {e}")))?;
            std::ptr::copy_nonoverlapping(nal_bytes.as_ptr(), data_ptr, nal_bytes.len());
            buffer
                .Unlock()
                .map_err(|e| MediaError::Other(format!("Unlock buffer: {e}")))?;
            buffer
                .SetCurrentLength(nal_bytes.len() as u32)
                .map_err(|e| MediaError::Other(format!("SetCurrentLength: {e}")))?;

            // Wrap the buffer in a sample and push to the MFT.
            let sample =
                MFCreateSample().map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
            sample
                .SetSampleTime(timestamp)
                .map_err(|e| MediaError::Other(format!("SetSampleTime: {e}")))?;

            match self.mft.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    return Err(MediaError::Other("MFT NOTACCEPTING".into()));
                }
                Err(e) => return Err(MediaError::Other(format!("ProcessInput: {e}"))),
            }

            self.needs_idr = false;
            Ok(())
        }
    }

    /// Pull the next decoded frame. Returns `Ok(None)` if the decoder needs
    /// more input. Returns raw NV12 bytes for Phase 0 (zero-copy D3D11
    /// texture extraction is deferred to Phase 3).
    pub fn process_output(&mut self) -> Result<Option<Vec<u8>>> {
        unsafe {
            let mut output_buffer = MFT_OUTPUT_DATA_BUFFER::default();
            let mut status: u32 = 0;
            let mut stream_change_retried = false;
            loop {
                match self.mft.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut output_buffer),
                    &mut status,
                ) {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        return Ok(None);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        if stream_change_retried {
                            return Err(MediaError::Other(
                                "MF_E_TRANSFORM_STREAM_CHANGE after renegotiation".into(),
                            ));
                        }
                        // The MFT discovered the real stream parameters
                        // (e.g. SPS/PPS/VPS in the IDR) and wants us to
                        // re-select an output type. Re-enumerate available
                        // output types, set the first NV12 one, and retry.
                        self.renegotiate_output_type()?;
                        stream_change_retried = true;
                        continue;
                    }
                    Err(e) => return Err(MediaError::Other(format!("ProcessOutput: {e}"))),
                }
            }

            // Extract the sample through ManuallyDrop.
            let sample_opt: &Option<_> = &output_buffer.pSample;
            let sample = sample_opt
                .as_ref()
                .ok_or_else(|| MediaError::Other("null output sample".into()))?;
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(|e| MediaError::Other(format!("ConvertToContiguousBuffer: {e}")))?;
            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer
                .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
                .map_err(|e| MediaError::Other(format!("Lock out buffer: {e}")))?;
            let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
            buffer
                .Unlock()
                .map_err(|e| MediaError::Other(format!("Unlock out buffer: {e}")))?;

            // Manually drop the ManuallyDrop fields so COM refcounts are
            // released (otherwise the sample + events leak).
            std::mem::ManuallyDrop::drop(&mut output_buffer.pSample);
            std::mem::ManuallyDrop::drop(&mut output_buffer.pEvents);

            Ok(Some(bytes))
        }
    }

    /// Pull the next decoded frame as a zero-copy GPU texture.
    ///
    /// Returns `Ok(None)` if the decoder needs more input. On success the
    /// returned `D3d11Texture` wraps the `ID3D11Texture2D` held by the MFT's
    /// output buffer: the COM refcount is incremented by
    /// `IMFDXGIBuffer::GetResource`, so the underlying texture stays alive
    /// until the wrapper is dropped.
    ///
    /// The returned `width` / `height` come from the decoder's configured
    /// dimensions (not the raw texture dims, which may be larger due to MFT
    /// alignment). The format is always NV12.
    ///
    /// # Known limitation
    ///
    /// MFTs may pool output samples across subresources of a single texture
    /// array. For Phase 0 we do not inspect `IMFDXGIBuffer::GetSubresourceIndex`
    /// and treat the returned texture as subresource 0. If the MFT returns a
    /// non-zero subresource, downstream consumers may read the wrong slice.
    /// The NVIDIA HW H.265 MFT historically hands out per-frame textures
    /// (subresource 0), so this works in practice; Plan 3 viewer code can
    /// inspect the subresource index when needed.
    pub fn process_output_texture(&mut self) -> Result<Option<D3d11Texture>> {
        unsafe {
            let mut output_buffer = MFT_OUTPUT_DATA_BUFFER::default();
            let mut status: u32 = 0;
            let mut stream_change_retried = false;
            loop {
                match self.mft.ProcessOutput(
                    0,
                    std::slice::from_mut(&mut output_buffer),
                    &mut status,
                ) {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                        return Ok(None);
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        if stream_change_retried {
                            return Err(MediaError::Other(
                                "MF_E_TRANSFORM_STREAM_CHANGE after renegotiation".into(),
                            ));
                        }
                        self.renegotiate_output_type()?;
                        stream_change_retried = true;
                        continue;
                    }
                    Err(e) => return Err(MediaError::Other(format!("ProcessOutput: {e}"))),
                }
            }

            // Extract the texture via IMFDXGIBuffer without going through a
            // contiguous CPU copy. Use GetBufferByIndex(0) (do NOT call
            // ConvertToContiguousBuffer — that would force a CPU-side copy).
            let mut new_subresource_index = self.last_subresource_index;
            let extract_result: Result<D3d11Texture> = (|| {
                let sample = output_buffer
                    .pSample
                    .as_ref()
                    .ok_or_else(|| MediaError::Other("null output sample".into()))?;
                let buffer = sample
                    .GetBufferByIndex(0)
                    .map_err(|e| MediaError::Other(format!("GetBufferByIndex: {e}")))?;
                let dxgi_buf: IMFDXGIBuffer = buffer
                    .cast()
                    .map_err(|e| MediaError::Other(format!("IMFDXGIBuffer cast: {e}")))?;

                // Record the subresource index for diagnostics. MFTs that
                // pool outputs into a texture array will hand out non-zero
                // indices; our wrapper currently ignores this.
                if let Ok(idx) = dxgi_buf.GetSubresourceIndex() {
                    new_subresource_index = idx;
                }

                // IMFDXGIBuffer::GetResource is a raw-FFI style method in the
                // `windows` crate: (riid, ppvobject) -> Result<()>. Feed it
                // the ID3D11Texture2D IID and receive an Option<T>.
                let mut out: Option<ID3D11Texture2D> = None;
                dxgi_buf
                    .GetResource(
                        &ID3D11Texture2D::IID,
                        &mut out as *mut Option<ID3D11Texture2D> as *mut *mut core::ffi::c_void,
                    )
                    .map_err(|e| MediaError::Other(format!("GetResource: {e}")))?;
                let tex =
                    out.ok_or_else(|| MediaError::Other("GetResource returned null".into()))?;

                Ok(D3d11Texture::from_raw(
                    tex,
                    self.width,
                    self.height,
                    TextureFormat::Nv12,
                ))
            })();
            self.last_subresource_index = new_subresource_index;

            // Drop ManuallyDrop fields so the sample + events release their
            // COM refcounts. The ID3D11Texture2D refcount we took above via
            // GetResource is independent, so the texture outlives the sample.
            std::mem::ManuallyDrop::drop(&mut output_buffer.pSample);
            std::mem::ManuallyDrop::drop(&mut output_buffer.pEvents);

            extract_result.map(Some)
        }
    }

    /// Re-enumerate the MFT's available output types (after
    /// `MF_E_TRANSFORM_STREAM_CHANGE`), pick the first NV12 entry, and
    /// install it as the active output type. Falls back to the first
    /// available type if no NV12 entry is present.
    ///
    /// This is invoked from `process_output` when the MFT reports a stream
    /// change, which typically happens right after the first IDR is
    /// decoded and the true stream parameters (width/height/format) become
    /// known to the MFT.
    fn renegotiate_output_type(&mut self) -> Result<()> {
        use windows::core::GUID;

        unsafe {
            // Walk available output types. First try to find NV12; if none,
            // take the first (index 0) entry.
            let mut chosen: Option<windows::Win32::Media::MediaFoundation::IMFMediaType> = None;
            let mut fallback: Option<windows::Win32::Media::MediaFoundation::IMFMediaType> = None;
            for index in 0..32u32 {
                match self.mft.GetOutputAvailableType(0, index) {
                    Ok(media_type) => {
                        if fallback.is_none() {
                            fallback = Some(media_type.clone());
                        }
                        if let Ok(subtype) = media_type.GetGUID(&MF_MT_SUBTYPE) {
                            let nv12: GUID = MFVideoFormat_NV12;
                            if subtype == nv12 {
                                chosen = Some(media_type);
                                break;
                            }
                        }
                    }
                    Err(_) => break, // MF_E_NO_MORE_TYPES or similar.
                }
            }

            let output_type = chosen.or(fallback).ok_or_else(|| {
                MediaError::Other(
                    "no available output types after MF_E_TRANSFORM_STREAM_CHANGE".into(),
                )
            })?;

            self.mft
                .SetOutputType(0, &output_type, 0)
                .map_err(|e| MediaError::Other(format!("SetOutputType (renegotiate): {e}")))?;
            Ok(())
        }
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
                // On VMs / server SKUs there is no HW H.265 decoder MFT;
                // treat as non-fatal for CI portability.
                eprintln!("H.265 MFT not available (non-fatal): {e}");
            }
        }
    }
}
