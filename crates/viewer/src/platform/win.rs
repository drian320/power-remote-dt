//! Windows viewer backend. Receives the existing per-codec consumer +
//! renderer enums from lib.rs (T3) and gains factory functions in T4.

#![cfg(windows)]

use anyhow::{Context, Result};
use prdt_media_sw::Openh264Decoder;
#[cfg(prdt_nvdec_bindings)]
use prdt_media_win::NvdecD3d11Consumer;
use prdt_media_win::{
    CpuI420Uploader, D3d11Device, D3d11Texture, MfD3d11Consumer, Nv12Renderer, SwapChain,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use windows::Win32::Foundation::HWND;
use winit::window::Window;

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

/// Extract the raw Win32 `HWND` from a winit `Window`. Required for
/// `SwapChain::new_for_hwnd`. Migrated verbatim from lib.rs.
pub(crate) fn extract_hwnd(window: &Window) -> Result<HWND> {
    let handle = window.window_handle().context("window_handle()")?.as_raw();
    match handle {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        other => anyhow::bail!("unexpected window handle type: {:?}", other),
    }
}
