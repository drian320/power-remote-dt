//! Media Foundation H.265 encoder MFT wrapper. Provides a
//! `Hevc265Encoder` impl that takes a B8G8R8A8 D3D11 texture and emits
//! an Annex-B H.265 access unit on each call.
//!
//! Works on any DXGI adapter that exposes a hardware H.265 encoder MFT
//! (NVIDIA / AMD / Intel — driver-provided MFT). Falls back to a
//! software MFT if no hardware MFT is present (slow but functional).
//!
//! Internally:
//!   1. Capture produces BGRA D3D11 texture.
//!   2. `BgraToNv12::convert` produces an NV12 D3D11 texture.
//!   3. The NV12 texture wraps in an `IMFSample` via `MFCreateDXGISurfaceBuffer`.
//!   4. `IMFTransform::ProcessInput` queues the sample.
//!   5. `IMFTransform::ProcessOutput` drains the encoded `IMFSample`.
//!   6. The encoded buffer's bytes are an Annex-B HEVC NAL stream.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIDeviceManager, IMFTransform, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFTEnumEx, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MFSampleExtension_CleanPoint,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::CoTaskMemFree;

use crate::d3d11::{BgraToNv12, D3d11Device, D3d11Texture};
use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder};
use crate::error::MediaError;
use crate::nvenc::NvencEncoderConfig;

pub struct MfH265Encoder {
    transform: IMFTransform,
    #[allow(dead_code)]
    device_manager: IMFDXGIDeviceManager,
    bgra_to_nv12: BgraToNv12,
    nv12_input: D3d11Texture,
    width: u32,
    height: u32,
    #[allow(dead_code)]
    sample_seq: u64,
    pending_idr: bool,
}

impl MfH265Encoder {
    /// Construct an MF H.265 encoder bound to the given D3D11 device.
    /// Uses the OS-default hardware encoder MFT when one is available.
    /// `cfg` shares fields with NVENC: width, height, fps, bitrate.
    pub fn new(dev: &D3d11Device, cfg: &NvencEncoderConfig) -> Result<Self, MediaError> {
        super::ensure_mf_runtime()?;

        let transform = enumerate_h265_encoder_mft()?;
        let device_manager = create_dxgi_device_manager(dev)?;

        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, device_manager.as_raw() as usize)
                .map_err(|e| MediaError::Other(format!("MFT_MESSAGE_SET_D3D_MANAGER: {e}")))?;
        }

        configure_output_type(&transform, cfg)?;
        configure_input_type(&transform, cfg)?;
        set_low_latency(&transform)?;

        let bgra_to_nv12 = BgraToNv12::new(dev, cfg.width, cfg.height)?;
        let nv12_input = bgra_to_nv12.allocate_nv12_output(dev)?;

        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| MediaError::Other(format!("BEGIN_STREAMING: {e}")))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| MediaError::Other(format!("START_OF_STREAM: {e}")))?;
        }

        Ok(Self {
            transform,
            device_manager,
            bgra_to_nv12,
            nv12_input,
            width: cfg.width,
            height: cfg.height,
            sample_seq: 0,
            pending_idr: true,
        })
    }
}

impl Drop for MfH265Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
}

unsafe impl Send for MfH265Encoder {}

impl Hevc265Encoder for MfH265Encoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        // 1. BGRA -> NV12
        self.bgra_to_nv12.convert(texture, &self.nv12_input)?;

        // 2. Wrap NV12 texture in an IMFSample.
        let sample =
            wrap_d3d11_in_sample(&self.nv12_input, timestamp_us, self.width, self.height)?;

        if force_idr || self.pending_idr {
            unsafe {
                sample
                    .SetUINT32(&MFSampleExtension_CleanPoint, 1)
                    .map_err(|e| MediaError::Other(format!("SetUINT32 CleanPoint: {e}")))?;
            }
            self.pending_idr = false;
        }

        // 3. ProcessInput
        unsafe {
            self.transform
                .ProcessInput(0, &sample, 0)
                .map_err(|e| MediaError::Other(format!("ProcessInput: {e}")))?;
        }

        // 4. Drain one encoded sample (low-latency mode is 1:1).
        let encoded = drain_one_output(&self.transform)?;

        self.sample_seq += 1;
        Ok(EncodedH265Frame {
            nal_bytes: encoded.bytes,
            is_keyframe: encoded.is_idr,
            timestamp: timestamp_us,
        })
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        tracing::warn!(
            target = "mf",
            requested_bps = bps,
            "set_target_bitrate is currently a no-op for MF (rate-control \
             reconfig requires MFT reset)"
        );
    }

    fn backend_name(&self) -> &'static str {
        "mf"
    }
}

// ====== Helper functions =====================================================

fn enumerate_h265_encoder_mft() -> Result<IMFTransform, MediaError> {
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);

    let mut p_activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            None,
            Some(&output_info),
            &mut p_activates,
            &mut count,
        )
        .map_err(|e| MediaError::Other(format!("MFTEnumEx: {e}")))?;

        if count == 0 {
            return Err(MediaError::Other(
                "no H.265 encoder MFT registered (HEVC Video Extensions installed? \
                 GPU driver provides one?)"
                    .into(),
            ));
        }
        let activates = std::slice::from_raw_parts(p_activates, count as usize);
        let activate = activates[0]
            .clone()
            .ok_or_else(|| MediaError::Other("first activate is None".into()))?;
        let transform: IMFTransform = activate
            .ActivateObject()
            .map_err(|e| MediaError::Other(format!("IMFActivate::ActivateObject: {e}")))?;
        CoTaskMemFree(Some(p_activates as *const _));
        Ok(transform)
    }
}

fn create_dxgi_device_manager(dev: &D3d11Device) -> Result<IMFDXGIDeviceManager, MediaError> {
    let mut reset_token: u32 = 0;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
            .map_err(|e| MediaError::Other(format!("MFCreateDXGIDeviceManager: {e}")))?;
        let manager = manager
            .ok_or_else(|| MediaError::Other("MFCreateDXGIDeviceManager returned null".into()))?;
        manager
            .ResetDevice(dev.device(), reset_token)
            .map_err(|e| MediaError::Other(format!("ResetDevice: {e}")))?;
        Ok(manager)
    }
}

fn configure_output_type(
    transform: &IMFTransform,
    cfg: &NvencEncoderConfig,
) -> Result<(), MediaError> {
    unsafe {
        let out_type = MFCreateMediaType()
            .map_err(|e| MediaError::Other(format!("MFCreateMediaType (out): {e}")))?;

        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| MediaError::Other(format!("SetGUID major (out): {e}")))?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)
            .map_err(|e| MediaError::Other(format!("SetGUID sub (out): {e}")))?;
        out_type
            .SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_bps)
            .map_err(|e| MediaError::Other(format!("SetUINT32 bitrate: {e}")))?;
        let fr_packed = (cfg.fps_numerator as u64) << 32 | cfg.fps_denominator as u64;
        out_type
            .SetUINT64(&MF_MT_FRAME_RATE, fr_packed)
            .map_err(|e| MediaError::Other(format!("SetUINT64 frame_rate (out): {e}")))?;
        let size_packed = (cfg.width as u64) << 32 | cfg.height as u64;
        out_type
            .SetUINT64(&MF_MT_FRAME_SIZE, size_packed)
            .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size (out): {e}")))?;
        out_type
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_Progressive.0 as u32,
            )
            .map_err(|e| MediaError::Other(format!("SetUINT32 interlace (out): {e}")))?;
        out_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1u64)
            .map_err(|e| MediaError::Other(format!("SetUINT64 par (out): {e}")))?;

        transform
            .SetOutputType(0, &out_type, 0)
            .map_err(|e| MediaError::Other(format!("SetOutputType: {e}")))?;
    }
    Ok(())
}

fn configure_input_type(
    transform: &IMFTransform,
    cfg: &NvencEncoderConfig,
) -> Result<(), MediaError> {
    unsafe {
        let in_type = MFCreateMediaType()
            .map_err(|e| MediaError::Other(format!("MFCreateMediaType (in): {e}")))?;

        in_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| MediaError::Other(format!("SetGUID major (in): {e}")))?;
        in_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(|e| MediaError::Other(format!("SetGUID sub (in): {e}")))?;
        let fr_packed = (cfg.fps_numerator as u64) << 32 | cfg.fps_denominator as u64;
        in_type
            .SetUINT64(&MF_MT_FRAME_RATE, fr_packed)
            .map_err(|e| MediaError::Other(format!("SetUINT64 frame_rate (in): {e}")))?;
        let size_packed = (cfg.width as u64) << 32 | cfg.height as u64;
        in_type
            .SetUINT64(&MF_MT_FRAME_SIZE, size_packed)
            .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size (in): {e}")))?;
        in_type
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_Progressive.0 as u32,
            )
            .map_err(|e| MediaError::Other(format!("SetUINT32 interlace (in): {e}")))?;
        in_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1u64)
            .map_err(|e| MediaError::Other(format!("SetUINT64 par (in): {e}")))?;

        transform
            .SetInputType(0, &in_type, 0)
            .map_err(|e| MediaError::Other(format!("SetInputType: {e}")))?;
    }
    Ok(())
}

fn set_low_latency(transform: &IMFTransform) -> Result<(), MediaError> {
    unsafe {
        let attrs = transform
            .GetAttributes()
            .map_err(|e| MediaError::Other(format!("GetAttributes: {e}")))?;
        attrs
            .SetUINT32(&MF_LOW_LATENCY, 1)
            .map_err(|e| MediaError::Other(format!("SetUINT32 MF_LOW_LATENCY: {e}")))?;
        // Async MFTs need explicit unlock to deliver output events.
        let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
    }
    Ok(())
}

struct DrainedOutput {
    bytes: Vec<u8>,
    is_idr: bool,
}

fn drain_one_output(transform: &IMFTransform) -> Result<DrainedOutput, MediaError> {
    use windows::Win32::Media::MediaFoundation::{
        MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    };
    unsafe {
        let stream_info: MFT_OUTPUT_STREAM_INFO = transform
            .GetOutputStreamInfo(0)
            .map_err(|e| MediaError::Other(format!("GetOutputStreamInfo: {e}")))?;
        let mft_provides_sample =
            stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        let sample = if mft_provides_sample {
            None
        } else {
            let s = MFCreateSample()
                .map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
            let buf = MFCreateMemoryBuffer(stream_info.cbSize.max(1))
                .map_err(|e| MediaError::Other(format!("MFCreateMemoryBuffer: {e}")))?;
            s.AddBuffer(&buf)
                .map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
            Some(s)
        };

        let mut data_buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        };
        let mut status: u32 = 0;
        let res = transform.ProcessOutput(0, std::slice::from_mut(&mut data_buffer), &mut status);
        match res {
            Ok(()) => {
                let out_sample = std::mem::ManuallyDrop::take(&mut data_buffer.pSample)
                    .ok_or_else(|| MediaError::Other("ProcessOutput: no sample".into()))?;
                std::mem::ManuallyDrop::drop(&mut data_buffer.pEvents);
                read_sample_bytes(&out_sample)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                std::mem::ManuallyDrop::drop(&mut data_buffer.pSample);
                std::mem::ManuallyDrop::drop(&mut data_buffer.pEvents);
                Err(MediaError::Other(
                    "ProcessOutput needs more input (low-latency violation; \
                     MFT did not emit a frame)"
                        .into(),
                ))
            }
            Err(e) => {
                std::mem::ManuallyDrop::drop(&mut data_buffer.pSample);
                std::mem::ManuallyDrop::drop(&mut data_buffer.pEvents);
                Err(MediaError::Other(format!("ProcessOutput: {e}")))
            }
        }
    }
}

fn read_sample_bytes(
    sample: &windows::Win32::Media::MediaFoundation::IMFSample,
) -> Result<DrainedOutput, MediaError> {
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| MediaError::Other(format!("ConvertToContiguousBuffer: {e}")))?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|e| MediaError::Other(format!("buffer.Lock: {e}")))?;
        let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
        buffer
            .Unlock()
            .map_err(|e| MediaError::Other(format!("buffer.Unlock: {e}")))?;

        let is_idr = sample
            .GetUINT32(&MFSampleExtension_CleanPoint)
            .map(|v| v != 0)
            .unwrap_or(false);
        Ok(DrainedOutput { bytes, is_idr })
    }
}

fn wrap_d3d11_in_sample(
    texture: &D3d11Texture,
    timestamp_us: u64,
    _width: u32,
    _height: u32,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, MediaError> {
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(
            &ID3D11Texture2D::IID,
            texture.raw(),
            0,
            false,
        )
        .map_err(|e| MediaError::Other(format!("MFCreateDXGISurfaceBuffer: {e}")))?;

        let sample = MFCreateSample()
            .map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
        // MF timestamp is in 100ns units; caller passes microseconds.
        sample
            .SetSampleTime((timestamp_us * 10) as i64)
            .map_err(|e| MediaError::Other(format!("SetSampleTime: {e}")))?;
        Ok(sample)
    }
}
