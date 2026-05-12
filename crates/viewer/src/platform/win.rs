//! Windows viewer backend. Receives the existing per-codec consumer +
//! renderer enums from lib.rs (T3) and gains factory functions in T4.

#![cfg(windows)]

use anyhow::{Context, Result};
use prdt_input_win::{
    clipboard_sequence_number as _input_win_clipboard_sequence_number,
    read_clipboard_text as _input_win_read_clipboard_text,
    virtual_desktop_rect as _input_win_virtual_desktop_rect,
    write_clipboard_text as _input_win_write_clipboard_text, MAX_CLIPBOARD_BYTES as _INPUT_WIN_MAX,
};
use prdt_media_sw::Openh264Decoder;
#[cfg(prdt_nvdec_bindings)]
use prdt_media_win::NvdecD3d11Consumer;
use prdt_media_win::{
    pick_default_adapter, CpuI420Uploader, D3d11Device, D3d11Texture, MfD3d11Consumer,
    Nv12Renderer, SwapChain,
};
use prdt_protocol::MonitorRect;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use windows::Win32::Foundation::HWND;
use winit::window::Window;

/// Re-exported max clipboard bytes; identical value across OSes.
pub const MAX_CLIPBOARD_BYTES: usize = _INPUT_WIN_MAX;

/// Per-decoder decoded frame. The viewer thread receives one of these per
/// frame and dispatches to the matching renderer. Renamed from
/// `LatestFrame` (lib.rs) — kept identical in shape for T7 compatibility.
pub enum PlatformFrame {
    /// Single NV12 D3D11 texture from `MfD3d11Consumer::take_latest_texture`.
    Nv12(D3d11Texture),
    /// Dual-plane (R8 Y, R8G8 UV) frame from
    /// `NvdecD3d11Consumer::take_latest_dual_plane`. Only constructed when
    /// `prdt_nvdec_bindings` cfg is set.
    ///
    /// Wrapped in `Arc` because the decoder publishes via arc-swap; we
    /// receive the same `Arc` the writer constructed, with no extra clone.
    #[cfg(prdt_nvdec_bindings)]
    DualPlane(Arc<prdt_media_win::DualPlaneFrame>),
}

/// Decoder-selected consumer. Held behind the recv task's
/// `Arc<tokio::sync::Mutex<...>>`. Renamed from `ViewerConsumer` (lib.rs).
pub enum PlatformConsumer {
    Mf(MfD3d11Consumer),
    #[cfg(prdt_nvdec_bindings)]
    Nvdec(NvdecD3d11Consumer),
    /// Software H.264 path: OpenH264 decoder produces I420 on CPU,
    /// `CpuI420Uploader` converts to NV12 and uploads into a D3D11
    /// texture shaped like what `MfD3d11Consumer` returns. The recv
    /// loop carries the most recently-uploaded texture in
    /// `latest_texture` so it can be drained next to the MF case
    /// without changing the renderer.
    Openh264 {
        decoder: Openh264Decoder,
        uploader: CpuI420Uploader,
        latest_texture: Option<D3d11Texture>,
        needs_idr: bool,
    },
}

/// Decoder-selected renderer enum. Private (held inside `PlatformRender`).
/// Renamed from `ViewerRenderer` (lib.rs).
pub(crate) enum WinRenderer {
    Mf(Nv12Renderer),
    #[cfg(prdt_nvdec_bindings)]
    Nvdec(prdt_media_win::DualPlaneYuvRenderer),
}

/// Per-OS render-state. Windows holds D3D11Device + SwapChain + the
/// codec-specific renderer. lib.rs treats this as opaque after T7.
/// Renamed from `ViewerRender` (lib.rs).
pub struct PlatformRender {
    pub(crate) window: Arc<Window>,
    #[allow(dead_code)]
    pub(crate) dev: D3d11Device,
    pub(crate) swap: SwapChain,
    pub(crate) renderer: Option<WinRenderer>,
}

impl PlatformRender {
    /// Borrow the underlying window. Used by lib.rs to call
    /// `request_redraw`, `set_title`, `inner_size`, etc., without leaking
    /// the platform-specific render-state internals.
    pub fn window(&self) -> &Window {
        &self.window
    }
}

/// Extract the raw Win32 `HWND` from a winit `Window`. Required for
/// `SwapChain::new_for_hwnd`. Migrated verbatim from lib.rs.
pub(crate) fn extract_hwnd(window: &Window) -> Result<HWND> {
    let handle = window.window_handle().context("window_handle()")?.as_raw();
    match handle {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        other => anyhow::bail!("unexpected window handle type: {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Set 1: Clipboard wrappers
// ---------------------------------------------------------------------------

/// Read the user's primary clipboard text channel.
pub fn read_clipboard_text() -> Result<String, super::ClipboardError> {
    _input_win_read_clipboard_text().map_err(|e| match e {
        prdt_input_win::ClipboardError::TooLarge(n) => super::ClipboardError::TooLarge(n),
        prdt_input_win::ClipboardError::NoText => super::ClipboardError::NoText,
        other => super::ClipboardError::Backend(other.to_string()),
    })
}

/// Set the user's primary clipboard text channel.
pub fn write_clipboard_text(text: &str) -> Result<(), super::ClipboardError> {
    _input_win_write_clipboard_text(text).map_err(|e| match e {
        prdt_input_win::ClipboardError::TooLarge(n) => super::ClipboardError::TooLarge(n),
        prdt_input_win::ClipboardError::NoText => super::ClipboardError::NoText,
        other => super::ClipboardError::Backend(other.to_string()),
    })
}

/// Cheap monotonic counter that bumps on any system clipboard change.
pub fn clipboard_sequence_number() -> u32 {
    _input_win_clipboard_sequence_number()
}

/// Return the host's combined virtual desktop rectangle in screen-space coords.
#[allow(dead_code)] // exposed via `platform::virtual_desktop_rect`; reserved for L2 multi-monitor
pub fn virtual_desktop_rect() -> MonitorRect {
    _input_win_virtual_desktop_rect()
}

// ---------------------------------------------------------------------------
// Set 2: Renderer build / present / resize
// ---------------------------------------------------------------------------

/// Build the per-OS render state. lib.rs calls this in `resumed()`.
pub fn build_render(
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> Result<PlatformRender, super::RenderError> {
    let adapter = pick_default_adapter()
        .map_err(|e| super::RenderError::Init(format!("pick_default_adapter: {e}")))?;
    let dev = D3d11Device::create(&adapter)
        .map_err(|e| super::RenderError::Init(format!("D3d11Device::create: {e}")))?;
    let hwnd = extract_hwnd(&window)
        .map_err(|e| super::RenderError::Init(format!("extract_hwnd: {e}")))?;
    let swap = SwapChain::new_for_hwnd(&dev, hwnd, width.max(1), height.max(1))
        .map_err(|e| super::RenderError::Init(format!("SwapChain::new_for_hwnd: {e}")))?;
    Ok(PlatformRender {
        window,
        dev,
        swap,
        renderer: None,
    })
}

/// Resize the swapchain + the held codec-specific renderer.
pub fn resize_renderer(
    r: &mut PlatformRender,
    width: u32,
    height: u32,
) -> Result<(), super::RenderError> {
    r.swap
        .resize(width.max(1), height.max(1))
        .map_err(|e| super::RenderError::Present(format!("SwapChain::resize: {e}")))?;
    if let Some(rn) = r.renderer.as_mut() {
        match rn {
            WinRenderer::Mf(rmf) => {
                rmf.resize_output(width.max(1), height.max(1));
            }
            #[cfg(prdt_nvdec_bindings)]
            WinRenderer::Nvdec(_) => {
                // DualPlaneYuvRenderer is dimension-agnostic.
            }
        }
    }
    Ok(())
}

/// Present a single decoded frame. Returns `Err(RenderError::DeviceLost)`
/// on D3D11 device-removed; lib.rs maps that to `should_exit = true`.
pub fn present_frame(
    r: &mut PlatformRender,
    f: &PlatformFrame,
    decoder_label: &str,
    _shared: &crate::ViewerShared,
) -> Result<(), super::RenderError> {
    let needs_new = match (f, r.renderer.as_ref()) {
        (PlatformFrame::Nv12(nv12), Some(WinRenderer::Mf(rmf))) => {
            rmf.input_size() != (nv12.width(), nv12.height())
        }
        (_, None) => true,
        #[allow(unreachable_patterns)]
        _ => true,
    };
    if needs_new {
        let (iw, ih) = match f {
            PlatformFrame::Nv12(nv12) => (nv12.width(), nv12.height()),
            #[cfg(prdt_nvdec_bindings)]
            PlatformFrame::DualPlane(dp) => (dp.width, dp.height),
        };
        let new_renderer = if decoder_label == "nvdec" {
            #[cfg(prdt_nvdec_bindings)]
            {
                let rn = prdt_media_win::DualPlaneYuvRenderer::new(&r.dev).map_err(|e| {
                    super::RenderError::Init(format!("DualPlaneYuvRenderer::new: {e}"))
                })?;
                WinRenderer::Nvdec(rn)
            }
            #[cfg(not(prdt_nvdec_bindings))]
            {
                let rn = Nv12Renderer::new(&r.dev, iw, ih, r.swap.width(), r.swap.height())
                    .map_err(|e| super::RenderError::Init(format!("Nv12Renderer::new: {e}")))?;
                WinRenderer::Mf(rn)
            }
        } else {
            let rn = Nv12Renderer::new(&r.dev, iw, ih, r.swap.width(), r.swap.height())
                .map_err(|e| super::RenderError::Init(format!("Nv12Renderer::new: {e}")))?;
            WinRenderer::Mf(rn)
        };
        r.renderer = Some(new_renderer);
    }

    if let Some(rn) = r.renderer.as_ref() {
        #[allow(unreachable_patterns)]
        match (rn, f) {
            (WinRenderer::Mf(rmf), PlatformFrame::Nv12(nv12_tex)) => {
                rmf.render(nv12_tex, &r.swap).map_err(|e| {
                    super::RenderError::Present(format!("Nv12Renderer::render: {e}"))
                })?;
            }
            #[cfg(prdt_nvdec_bindings)]
            (WinRenderer::Nvdec(rnv), PlatformFrame::DualPlane(dpl)) => {
                rnv.render(dpl.as_ref(), &r.swap).map_err(|e| {
                    super::RenderError::Present(format!("DualPlaneYuvRenderer::render: {e}"))
                })?;
            }
            _ => {
                tracing::warn!("internal: renderer/frame variant mismatch");
            }
        }
    }

    match r.swap.present(true) {
        Ok(()) => Ok(()),
        Err(e) if e.is_device_removed() => Err(super::RenderError::DeviceLost(format!(
            "D3D11 device removed: {e}"
        ))),
        Err(e) => Err(super::RenderError::Present(format!(
            "SwapChain::present: {e}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Set 3: Consumer builder
// ---------------------------------------------------------------------------

use prdt_protocol::frame::Codec;

/// Build the per-codec consumer for the negotiated codec + decoder choice.
/// Mirrors the existing lib.rs decode-init code path; T7 will route lib.rs
/// through this factory.
pub fn build_consumer(
    decoder_arg: &str,
    codec: Codec,
    width: u32,
    height: u32,
    dev: &D3d11Device,
) -> Result<PlatformConsumer, super::ConsumerError> {
    match (decoder_arg, codec) {
        ("openh264", Codec::H264) | ("auto", Codec::H264) => {
            let dec = Openh264Decoder::new()
                .map_err(|e| super::ConsumerError::Init(format!("Openh264Decoder::new: {e}")))?;
            let uploader = CpuI420Uploader::new(dev, width, height)
                .map_err(|e| super::ConsumerError::Init(format!("CpuI420Uploader::new: {e}")))?;
            Ok(PlatformConsumer::Openh264 {
                decoder: dec,
                uploader,
                latest_texture: None,
                needs_idr: true,
            })
        }
        ("mf", Codec::H265) | ("auto", Codec::H265) => {
            let mf = MfD3d11Consumer::new(dev, width, height)
                .map_err(|e| super::ConsumerError::Init(format!("MfD3d11Consumer::new: {e}")))?;
            Ok(PlatformConsumer::Mf(mf))
        }
        #[cfg(prdt_nvdec_bindings)]
        ("nvdec", Codec::H265) => {
            let nv = NvdecD3d11Consumer::new(dev, width, height)
                .map_err(|e| super::ConsumerError::Init(
                    format!("NvdecD3d11Consumer::new: {e}"),
                ))?;
            Ok(PlatformConsumer::Nvdec(nv))
        }
        #[cfg(not(prdt_nvdec_bindings))]
        ("nvdec", Codec::H265) => Err(super::ConsumerError::Init(
            "nvdec requested but built without prdt_nvdec_bindings cfg".into(),
        )),
        (other_decoder, other_codec) => Err(super::ConsumerError::Init(format!(
            "unsupported decoder/codec combination on Windows: decoder={other_decoder}, codec={other_codec:?}"
        ))),
    }
}
