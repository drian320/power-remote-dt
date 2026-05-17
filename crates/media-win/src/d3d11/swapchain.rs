//! DXGI flip-model swapchain bound to an HWND.
//!
//! Configured for a Desktop App viewer: double-buffered, flip-discard,
//! B8G8R8A8_UNORM (8-bit) or R10G10B10A2_UNORM (HDR10), stretch scaling,
//! ignore alpha. The swapchain buffers are created against the supplied
//! [`D3d11Device`]; callers are responsible for keeping that device alive
//! for the lifetime of the swapchain.
//!
//! This wrapper exposes just enough surface area for the Phase 0 viewer
//! binary (Task 5): back-buffer access for the NV12 video-processor
//! renderer, an RTV for optional debug clears, resize on WM_SIZE, and
//! a vsync-toggled `Present`.
//!
//! PR3 additions (behind `media-win-hdr10` feature):
//! - `new_for_hwnd_hdr10`: constructs an R10G10B10A2_UNORM swapchain after
//!   probing for HDR10 display capability and DXGI presentation support.
//! - `set_hdr10_metadata`: calls `IDXGISwapChain4::SetHDRMetaData` on the
//!   first decoded IDR carrying HDR10 SEI and whenever metadata changes.

use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11RenderTargetView, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIFactory2, IDXGISwapChain1, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

#[cfg(feature = "media-win-hdr10")]
use windows::Win32::Graphics::Dxgi::{
    IDXGIFactory6, IDXGIOutput6, IDXGISwapChain3, IDXGISwapChain4,
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
    DXGI_HDR_METADATA_HDR10, DXGI_HDR_METADATA_TYPE_HDR10,
    DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT,
};

#[cfg(feature = "media-win-hdr10")]
use prdt_media_core::Hdr10Metadata;

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
    /// True when the swapchain was created with `new_for_hwnd_hdr10` and is
    /// presenting in `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`.
    is_hdr10: bool,
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
                is_hdr10: false,
            };
            out.create_rtv()?;
            Ok(out)
        }
    }

    /// Create an HDR10 flip-discard swapchain (`DXGI_FORMAT_R10G10B10A2_UNORM`)
    /// bound to `hwnd`. Performs two capability probes before construction:
    ///
    /// 1. `IDXGIOutput6::GetDesc1()` — the primary output must report
    ///    `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020` with `MaxLuminance > 0`.
    /// 2. `IDXGISwapChain3::CheckColorSpaceSupport()` — the probe swapchain
    ///    must advertise the `PRESENT` flag for the same color space.
    ///
    /// Returns `MediaError::HdrUnavailable` (no silent fallback) if either
    /// probe fails. Callers that want SDR downscale must opt in via the
    /// `media-win-hdr-to-sdr-fallback` feature (PR3 Step 4 change #6).
    #[cfg(feature = "media-win-hdr10")]
    pub fn new_for_hwnd_hdr10(
        dev: &D3d11Device,
        hwnd: HWND,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        unsafe {
            // ── Probe 1: HDR display attached? ──────────────────────────────
            // Use IDXGIFactory6 to enumerate adapters by GPU preference, then
            // walk each adapter's outputs looking for one that reports
            // DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 with MaxLuminance > 0.
            let factory6: IDXGIFactory6 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))
                .map_err(|e| MediaError::dxgi("CreateDXGIFactory2 (HDR probe)", e))?;

            let mut hdr_display_found = false;
            let mut adapter_idx = 0u32;
            loop {
                // SAFETY: EnumAdapterByGpuPreference fills an out-param on success;
                // DXGI_ERROR_NOT_FOUND (0x887A0002) signals end-of-list.
                let adapter_result = factory6
                    .EnumAdapterByGpuPreference::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>(
                        adapter_idx,
                        DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                    );
                let adapter = match adapter_result {
                    Ok(a) => a,
                    Err(_) => break, // DXGI_ERROR_NOT_FOUND or no more adapters
                };

                let mut output_idx = 0u32;
                loop {
                    // SAFETY: EnumOutputs returns E_INVALIDARG when out of range.
                    let output = match adapter.EnumOutputs(output_idx) {
                        Ok(o) => o,
                        Err(_) => break,
                    };
                    // SAFETY: IDXGIOutput6 is a COM interface supported since DXGI 1.6.
                    if let Ok(output6) = output.cast::<IDXGIOutput6>() {
                        // SAFETY: GetDesc1 fills a DXGI_OUTPUT_DESC1 on success.
                        if let Ok(desc) = output6.GetDesc1() {
                            if desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
                                && desc.MaxLuminance > 0.0
                            {
                                hdr_display_found = true;
                            }
                        }
                    }
                    output_idx += 1;
                    if hdr_display_found {
                        break;
                    }
                }
                adapter_idx += 1;
                if hdr_display_found {
                    break;
                }
            }

            if !hdr_display_found {
                return Err(MediaError::HdrUnavailable {
                    reason: "no HDR10 display attached \
                             (no output reports DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 \
                             with MaxLuminance > 0)"
                        .into(),
                });
            }

            // ── Probe 2: swapchain color-space presentation support ──────────
            // Create a probe swapchain with the target format and query whether
            // the color space can be presented.
            let factory2: IDXGIFactory2 = factory6
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGIFactory6 -> IDXGIFactory2", e))?;

            let probe_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_R10G10B10A2_UNORM,
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

            let probe_swap: IDXGISwapChain1 = factory2
                .CreateSwapChainForHwnd(dev.device(), hwnd, &probe_desc, None, None)
                .map_err(|e| MediaError::dxgi("CreateSwapChainForHwnd (HDR probe)", e))?;

            // SAFETY: IDXGISwapChain3 is supported since Windows 10 RS2; the
            // probe swapchain was created against a DXGI 1.2+ factory.
            let probe3: IDXGISwapChain3 = probe_swap
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGISwapChain1 -> IDXGISwapChain3 (probe)", e))?;

            let mut support_flags = 0u32;
            // SAFETY: CheckColorSpaceSupport writes the support bitmask into
            // the provided u32 out-param.
            probe3
                .CheckColorSpaceSupport(
                    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
                    &mut support_flags,
                )
                .map_err(|e| MediaError::dxgi("CheckColorSpaceSupport", e))?;

            if support_flags & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32 == 0 {
                return Err(MediaError::HdrUnavailable {
                    reason: "swapchain does not advertise PRESENT support for \
                             DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 \
                             (driver or compositor too old / HDR toggle off / EDID rejected)"
                        .into(),
                });
            }

            // ── Construct the real HDR10 swapchain ───────────────────────────
            // Reuse the probe swapchain rather than creating a second one.
            // Cast to IDXGISwapChain3 and set the color space before caching
            // the RTV so the first present is already in the correct color space.
            // SAFETY: SetColorSpace1 on IDXGISwapChain3 is safe after the
            // CheckColorSpaceSupport PRESENT flag confirmed above.
            probe3
                .SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                .map_err(|e| MediaError::dxgi("SetColorSpace1", e))?;

            let mut out = Self {
                dev: dev.clone(),
                swap: probe_swap,
                rtv: None,
                width,
                height,
                is_hdr10: true,
            };
            out.create_rtv()?;
            Ok(out)
        }
    }

    /// True when this swapchain presents in
    /// `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020` (HDR10 mode).
    pub fn is_hdr10(&self) -> bool {
        self.is_hdr10
    }

    /// Set HDR10 mastering display metadata on the swapchain via
    /// `IDXGISwapChain4::SetHDRMetaData`. Call once on the first decoded IDR
    /// that carries HDR10 SEI, and again whenever the metadata changes.
    ///
    /// A no-op (with a `tracing::warn`) when called on an 8-bit swapchain
    /// (`is_hdr10() == false`).
    #[cfg(feature = "media-win-hdr10")]
    pub fn set_hdr10_metadata(&self, m: &Hdr10Metadata) -> Result<()> {
        if !self.is_hdr10 {
            tracing::warn!("set_hdr10_metadata called on an 8-bit swapchain; ignoring");
            return Ok(());
        }
        unsafe {
            // SAFETY: IDXGISwapChain4 is available on Windows 10 RS1+.
            // The swapchain was constructed via new_for_hwnd_hdr10 which
            // already confirmed IDXGISwapChain3 is available (superset).
            let swap4: IDXGISwapChain4 = self
                .swap
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGISwapChain1 -> IDXGISwapChain4", e))?;

            // Convert Hdr10Metadata into the DXGI struct layout.
            // Field order matches DXGI_HDR_METADATA_HDR10:
            //   RedPrimary[2], GreenPrimary[2], BluePrimary[2],
            //   WhitePoint[2], MaxMasteringLuminance, MinMasteringLuminance,
            //   MaxContentLightLevel, MaxFrameAverageLightLevel.
            let blob = DXGI_HDR_METADATA_HDR10 {
                RedPrimary: [m.display_primaries[0].0, m.display_primaries[0].1],
                GreenPrimary: [m.display_primaries[1].0, m.display_primaries[1].1],
                BluePrimary: [m.display_primaries[2].0, m.display_primaries[2].1],
                WhitePoint: [m.white_point.0, m.white_point.1],
                MaxMasteringLuminance: m.max_mastering_luminance,
                MinMasteringLuminance: m.min_mastering_luminance,
                MaxContentLightLevel: m.max_content_light_level,
                MaxFrameAverageLightLevel: m.max_frame_average_light_level,
            };

            // SAFETY: SetHDRMetaData reads exactly `size` bytes from the pointer.
            // `blob` is valid for the duration of this call.
            swap4
                .SetHDRMetaData(
                    DXGI_HDR_METADATA_TYPE_HDR10,
                    std::mem::size_of::<DXGI_HDR_METADATA_HDR10>() as u32,
                    Some(&blob as *const DXGI_HDR_METADATA_HDR10 as *const std::ffi::c_void),
                )
                .map_err(|e| MediaError::dxgi("SetHDRMetaData", e))?;
        }
        Ok(())
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
            let fmt = if self.is_hdr10 {
                DXGI_FORMAT_R10G10B10A2_UNORM
            } else {
                DXGI_FORMAT_B8G8R8A8_UNORM
            };
            self.swap
                .ResizeBuffers(0, width, height, fmt, DXGI_SWAP_CHAIN_FLAG(0))
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
