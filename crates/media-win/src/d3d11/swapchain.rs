//! DXGI flip-model swapchain bound to an HWND.
//!
//! Configured for a Desktop App viewer: double-buffered, flip-discard,
//! B8G8R8A8_UNORM, stretch scaling, ignore alpha. The swapchain buffers
//! are created against the supplied [`D3d11Device`]; callers are
//! responsible for keeping that device alive for the lifetime of the
//! swapchain.
//!
//! This wrapper exposes just enough surface area for the Phase 0 viewer
//! binary (Task 5): back-buffer access for the NV12 video-processor
//! renderer, an RTV for optional debug clears, resize on WM_SIZE, and
//! a vsync-toggled `Present`.

use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11RenderTargetView, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIFactory2, IDXGISwapChain1, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::d3d11::D3d11Device;
use crate::error::{MediaError, Result};

/// HWND-bound DXGI flip-model swapchain + its back-buffer RTV.
///
/// Holds a strong reference to the [`D3d11Device`] so the underlying
/// `ID3D11Device` outlives the swapchain's internal references.
pub struct SwapChain {
    dev: D3d11Device,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    width: u32,
    height: u32,
}

impl SwapChain {
    /// Create a flip-discard swapchain bound to `hwnd` and sized to
    /// `width`x`height` pixels.
    pub fn new_for_hwnd(dev: &D3d11Device, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))
                .map_err(|e| MediaError::dxgi("CreateDXGIFactory2", e))?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
            };

            let swap: IDXGISwapChain1 = factory
                .CreateSwapChainForHwnd(dev.device(), hwnd, &desc, None, None)
                .map_err(|e| MediaError::dxgi("CreateSwapChainForHwnd", e))?;

            let mut out = Self {
                dev: dev.clone(),
                swap,
                rtv: None,
                width,
                height,
            };
            out.create_rtv()?;
            Ok(out)
        }
    }

    fn create_rtv(&mut self) -> Result<()> {
        unsafe {
            let backbuffer: ID3D11Texture2D = self
                .swap
                .GetBuffer(0)
                .map_err(|e| MediaError::dxgi("GetBuffer(0) for RTV", e))?;
            let resource: ID3D11Resource = backbuffer
                .cast()
                .map_err(|e| MediaError::d3d11("Texture2D -> Resource cast", e))?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            self.dev
                .device()
                .CreateRenderTargetView(&resource, None, Some(&mut rtv))
                .map_err(|e| MediaError::d3d11("CreateRenderTargetView", e))?;
            self.rtv = rtv;
            Ok(())
        }
    }

    /// Borrow the cached back-buffer RTV, if present.
    pub fn rtv(&self) -> Option<&ID3D11RenderTargetView> {
        self.rtv.as_ref()
    }

    /// Fetch a fresh reference to buffer 0 (the back-buffer) as
    /// `ID3D11Texture2D`. Each call adds a ref-count, so the result
    /// should not be cached across `Present` calls in flip-discard
    /// mode (the back-buffer identity is stable, but keeping an
    /// outstanding reference can confuse DXGI's buffer rotation).
    pub fn backbuffer(&self) -> Result<ID3D11Texture2D> {
        unsafe {
            self.swap
                .GetBuffer(0)
                .map_err(|e| MediaError::dxgi("GetBuffer(0) backbuffer", e))
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Resize the swapchain buffers. A no-op if the requested size is
    /// already current. Releases and re-creates the back-buffer RTV.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == self.width && height == self.height {
            return Ok(());
        }
        unsafe {
            // Must drop any view of the back-buffer before ResizeBuffers.
            self.rtv = None;
            self.swap
                .ResizeBuffers(
                    0,
                    width,
                    height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .map_err(|e| MediaError::dxgi("ResizeBuffers", e))?;
            self.width = width;
            self.height = height;
            self.create_rtv()?;
        }
        Ok(())
    }

    /// Present the current back-buffer. `vsync=true` gates to the next
    /// vertical blank; `vsync=false` returns immediately (tearing
    /// allowed depending on windowed vs. fullscreen state).
    ///
    /// Returns `MediaError::DeviceRemoved` specifically on
    /// `DXGI_ERROR_DEVICE_REMOVED` / `DEVICE_RESET` / `DEVICE_HUNG`, with
    /// `reason` populated from `GetDeviceRemovedReason`. Upstream callers
    /// can branch on `is_device_removed()` to decide whether a retry in
    /// the same device is futile (it is — the device must be recreated).
    pub fn present(&self, vsync: bool) -> Result<()> {
        const DXGI_ERROR_DEVICE_REMOVED: u32 = 0x887A_0005;
        const DXGI_ERROR_DEVICE_HUNG: u32 = 0x887A_0006;
        const DXGI_ERROR_DEVICE_RESET: u32 = 0x887A_0007;

        unsafe {
            let sync_interval = if vsync { 1 } else { 0 };
            let hr = self.swap.Present(sync_interval, DXGI_PRESENT(0));
            if hr.is_err() {
                let code = hr.0 as u32;
                if matches!(
                    code,
                    DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_HUNG | DXGI_ERROR_DEVICE_RESET
                ) {
                    // GetDeviceRemovedReason returns Result<()> whose Err
                    // carries the actual removed-reason HRESULT. On the
                    // (unlikely) Ok path, fall back to the Present HRESULT.
                    let reason = match self.dev.device().GetDeviceRemovedReason() {
                        Err(e) => e.code().0 as u32,
                        Ok(()) => code,
                    };
                    return Err(MediaError::DeviceRemoved {
                        context: "Present",
                        reason,
                    });
                }
                return Err(MediaError::Dxgi {
                    context: "Present",
                    hresult: code,
                });
            }
        }
        Ok(())
    }
}

// The `IDXGISwapChain1` COM pointer is thread-safe to hold; presenting
// concurrently with the immediate context in use is the caller's
// responsibility (same rule as the underlying device context).
unsafe impl Send for SwapChain {}
unsafe impl Sync for SwapChain {}
