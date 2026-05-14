//! NV12 -> BGRA conversion via [`ID3D11VideoProcessor`].
//!
//! The viewer pipeline receives NV12 textures from the Media Foundation
//! H.265 decoder (Plan 2c) and must draw them into the swapchain's BGRA
//! back-buffer. We could write a custom YUV->RGB pixel shader, but the
//! Windows video-processor path is both shorter and hardware-optimised:
//! BT.709 conversion, scaling, and deinterlacing (if ever needed) are
//! applied by the driver.
//!
//! The renderer is constructed once against a `(input, output)` size
//! pair and re-used every frame. Input / output views are created per
//! `render()` call — that is wasteful (Plan 4 will cache them) but
//! simple and correct for Phase 0.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Resource, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL;

use crate::d3d11::swapchain::SwapChain;
use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::{MediaError, Result};

/// Converts an NV12 texture into a BGRA swapchain back-buffer using
/// `ID3D11VideoProcessor`. Holds the processor + enumerator and the
/// video-device / video-context interfaces for the wrapped device.
pub struct Nv12Renderer {
    #[allow(dead_code)]
    dev: D3d11Device,
    video_dev: ID3D11VideoDevice,
    video_ctx: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
}

impl Nv12Renderer {
    /// Build a video-processor for the given input/output dimensions.
    /// The rate hint is a harmless 60/1 — the processor uses it only
    /// for deinterlace / frame-interpolation caps and we request
    /// neither.
    pub fn new(
        dev: &D3d11Device,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> Result<Self> {
        unsafe {
            let video_dev: ID3D11VideoDevice = dev
                .device()
                .cast()
                .map_err(|e| MediaError::d3d11("ID3D11Device -> ID3D11VideoDevice", e))?;

            // The immediate context is held behind a Mutex; we only need
            // the COM pointer (interfaces are free-threaded for ref-counting
            // but the context itself is still single-threaded at the
            // D3D11 level — callers must avoid concurrent use).
            let video_ctx: ID3D11VideoContext = dev
                .with_context(|c| c.clone())
                .cast()
                .map_err(|e| MediaError::d3d11("ID3D11DeviceContext -> ID3D11VideoContext", e))?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                InputWidth: input_width,
                InputHeight: input_height,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: 60,
                    Denominator: 1,
                },
                OutputWidth: output_width,
                OutputHeight: output_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = video_dev
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| MediaError::d3d11("CreateVideoProcessorEnumerator", e))?;

            let processor = video_dev
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| MediaError::d3d11("CreateVideoProcessor", e))?;

            Ok(Self {
                dev: dev.clone(),
                video_dev,
                video_ctx,
                enumerator,
                processor,
                input_width,
                input_height,
                output_width,
                output_height,
            })
        }
    }

    /// Update the cached output size. The video-processor itself is
    /// size-agnostic — the content descriptor dimensions are only
    /// used to pick rate-conversion caps, and the actual Blt rects
    /// come from the processor's per-call state (which we leave at
    /// its "full-frame" default).
    pub fn resize_output(&mut self, w: u32, h: u32) {
        self.output_width = w;
        self.output_height = h;
    }

    pub fn input_size(&self) -> (u32, u32) {
        (self.input_width, self.input_height)
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.output_width, self.output_height)
    }

    /// Render the NV12 `input` into the `swap` back-buffer. The input
    /// texture's subresource 0 is used (MFT-provided textures are
    /// single-subresource NV12 surfaces).
    pub fn render(&self, input: &D3d11Texture, swap: &SwapChain) -> Result<()> {
        unsafe {
            // --- Output view on the swapchain backbuffer ---
            let output_tex = swap.backbuffer()?;
            let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let out_res: ID3D11Resource = output_tex
                .cast()
                .map_err(|e| MediaError::d3d11("output Texture2D -> Resource", e))?;
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_dev
                .CreateVideoProcessorOutputView(
                    &out_res,
                    &self.enumerator,
                    &out_view_desc,
                    Some(&mut out_view),
                )
                .map_err(|e| MediaError::d3d11("CreateVideoProcessorOutputView", e))?;
            let out_view = out_view
                .ok_or_else(|| MediaError::Other("null VideoProcessorOutputView".into()))?;

            // --- Input view on the NV12 texture ---
            let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let in_res: ID3D11Resource = input
                .raw()
                .cast()
                .map_err(|e| MediaError::d3d11("input Texture2D -> Resource", e))?;
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            if let Err(e) = self.video_dev.CreateVideoProcessorInputView(
                &in_res,
                &self.enumerator,
                &in_view_desc,
                Some(&mut in_view),
            ) {
                // issue #19 Bug 4 re-diagnosis: a prior fix removed
                // RENDER_TARGET from the input texture's bind flags but the
                // error persisted, so that hypothesis was wrong. Dump the
                // *actual* texture desc, the video-processor content desc,
                // and the driver's format-support bits so the next smoke
                // pinpoints the real cause instead of guessing again.
                let diag = self.diagnose_input_view_failure(input.raw());
                tracing::error!(target: "prdt_media_win::nv12_renderer", "{diag}");
                return Err(MediaError::Other(format!(
                    "CreateVideoProcessorInputView failed ({e}); {diag}"
                )));
            }
            let in_view =
                in_view.ok_or_else(|| MediaError::Other("null VideoProcessorInputView".into()))?;

            // --- Build the stream descriptor ---
            //
            // `D3D11_VIDEO_PROCESSOR_STREAM` wraps its `pInputSurface` /
            // `pInputSurfaceRight` fields in `ManuallyDrop<Option<_>>`
            // because the struct is declared `repr(C)` for the ABI and
            // does not auto-drop its COM pointers. We construct a local
            // instance, hand the borrowed `in_view` COM reference to it,
            // run the Blt, and then explicitly release the `ManuallyDrop`
            // slots before the struct is dropped at end-of-scope.
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
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

            let blt_result = self.video_ctx.VideoProcessorBlt(
                &self.processor,
                &out_view,
                0,
                std::slice::from_ref(&stream),
            );

            // Release the COM pointers we parked in `ManuallyDrop` — must
            // run whether the Blt succeeded or failed, otherwise the
            // `in_view` refcount leaks.
            let _ = std::mem::ManuallyDrop::into_inner(std::mem::replace(
                &mut stream.pInputSurface,
                std::mem::ManuallyDrop::new(None),
            ));
            let _ = std::mem::ManuallyDrop::into_inner(std::mem::replace(
                &mut stream.pInputSurfaceRight,
                std::mem::ManuallyDrop::new(None),
            ));

            blt_result.map_err(|e| MediaError::d3d11("VideoProcessorBlt", e))?;
        }
        Ok(())
    }

    /// Diagnostic dump for a `CreateVideoProcessorInputView` failure
    /// (issue #19 Bug 4 re-diagnosis). Reports the *actual* input texture
    /// desc, this renderer's video-processor content desc, and the
    /// driver's format-support bits for the texture's format — enough to
    /// distinguish a bind-flag problem, a dimension mismatch, and an
    /// unsupported-format problem without another guess-and-smoke cycle.
    fn diagnose_input_view_failure(
        &self,
        tex: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    ) -> String {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let support_str = match unsafe { self.enumerator.CheckVideoProcessorFormat(desc.Format) } {
            Ok(bits) => {
                let input_ok = bits & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0;
                format!("format_support=0x{bits:08X} input_supported={input_ok}")
            }
            Err(e) => format!("CheckVideoProcessorFormat error: {e}"),
        };
        format!(
            "Bug4 probe: input_tex{{ Width:{} Height:{} Format:{} MipLevels:{} ArraySize:{} \
             Usage:{} BindFlags:0x{:X} CPUAccessFlags:0x{:X} MiscFlags:0x{:X} \
             SampleDesc{{Count:{},Quality:{}}} }} content_desc{{ InputWidth:{} InputHeight:{} \
             OutputWidth:{} OutputHeight:{} }} in_view_desc{{ FourCC:0 \
             ViewDimension:TEXTURE2D MipSlice:0 ArraySlice:0 }} {}",
            desc.Width,
            desc.Height,
            desc.Format.0,
            desc.MipLevels,
            desc.ArraySize,
            desc.Usage.0,
            desc.BindFlags,
            desc.CPUAccessFlags,
            desc.MiscFlags,
            desc.SampleDesc.Count,
            desc.SampleDesc.Quality,
            self.input_width,
            self.input_height,
            self.output_width,
            self.output_height,
            support_str,
        )
    }
}

// All members are free-threaded COM pointers plus the D3d11Device
// Arc wrapper. Thread-safety of the underlying immediate context is the
// caller's responsibility (same rule as `D3d11Device`).
unsafe impl Send for Nv12Renderer {}
unsafe impl Sync for Nv12Renderer {}

#[cfg(test)]
mod tests {
    use super::*;

    // Construction-only smoke test. Full render() exercise requires a
    // live HWND + swapchain, which is covered by the viewer binary in
    // Task 5.
    #[test]
    fn construct_video_processor_on_default_device() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let r = Nv12Renderer::new(&dev, 1920, 1080, 1920, 1080)
            .expect("Nv12Renderer on default adapter");
        assert_eq!(r.input_size(), (1920, 1080));
        assert_eq!(r.output_size(), (1920, 1080));
    }
}
