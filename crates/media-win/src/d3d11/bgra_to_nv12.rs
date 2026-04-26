//! D3D11 VideoProcessor wrapper that converts a B8G8R8A8_UNORM source
//! texture into an NV12 destination texture, both on the same D3D11
//! device. Used by the MF H.265 encoder when the encoder MFT only
//! accepts NV12 input (true for AMD/Intel/NVIDIA-driver MFTs).
//!
//! Performs colour space conversion BT.709 limited-range; sRGB output
//! semantics. Fully GPU; no CPU readback.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL;

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::MediaError;

pub struct BgraToNv12 {
    width: u32,
    height: u32,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl BgraToNv12 {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        unsafe {
            let video_device: ID3D11VideoDevice = dev
                .device()
                .cast()
                .map_err(|e| MediaError::Other(format!("ID3D11VideoDevice cast: {e}")))?;

            let video_context: ID3D11VideoContext = dev.with_context(|ctx| {
                ctx.cast::<ID3D11VideoContext>()
                    .map_err(|e| MediaError::Other(format!("ID3D11VideoContext cast: {e}")))
            })?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorEnumerator: {e}")))?;

            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessor: {e}")))?;

            Ok(Self {
                width,
                height,
                video_device,
                video_context,
                enumerator,
                processor,
            })
        }
    }

    /// Allocate an NV12 D3D11 texture compatible as the output destination.
    pub fn allocate_nv12_output(&self, dev: &D3d11Device) -> Result<D3d11Texture, MediaError> {
        D3d11Texture::new_default(dev, self.width, self.height, TextureFormat::Nv12)
            .map_err(|e| MediaError::Other(format!("allocate_nv12_output: {e}")))
    }

    /// Convert one frame: BGRA `src` → NV12 `dst`.
    pub fn convert(&self, src: &D3d11Texture, dst: &D3d11Texture) -> Result<(), MediaError> {
        unsafe {
            let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    src.raw(),
                    &self.enumerator,
                    &in_view_desc,
                    Some(&mut in_view),
                )
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorInputView: {e}")))?;
            let in_view =
                in_view.ok_or_else(|| MediaError::Other("input view null".into()))?;

            let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VPOV {
                        MipSlice: 0,
                    },
                },
            };
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_device
                .CreateVideoProcessorOutputView(
                    dst.raw(),
                    &self.enumerator,
                    &out_view_desc,
                    Some(&mut out_view),
                )
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorOutputView: {e}")))?;
            let out_view =
                out_view.ok_or_else(|| MediaError::Other("output view null".into()))?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(in_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let streams = [stream];

            self.video_context
                .VideoProcessorBlt(&self.processor, &out_view, 0, &streams)
                .map_err(|e| MediaError::Other(format!("VideoProcessorBlt: {e}")))?;
        }
        Ok(())
    }
}

unsafe impl Send for BgraToNv12 {}
