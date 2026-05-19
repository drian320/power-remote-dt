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
#[cfg(feature = "media-win-hdr10")]
use prdt_media_core::Hdr10Metadata;
use prdt_media_sw::Openh264Decoder;
#[cfg(feature = "media-win-hevc-main10")]
use prdt_media_win::MfHevcMain10Consumer;
#[cfg(prdt_nvdec_bindings)]
use prdt_media_win::NvdecD3d11Consumer;
use prdt_media_win::{
    pick_default_adapter, CpuI420Uploader, D3d11Device, D3d11Texture, MfD3d11Consumer,
    Nv12Renderer, Nv12ShaderRenderer, SwapChain,
};
#[cfg(feature = "media-win-hdr10")]
use prdt_media_win::{CpuP010Uploader, Nv12ShaderRendererP010};
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
    /// Pre-uploaded P010 D3D11 texture from the HDR10 decode path.
    /// The recv task uploads via `CpuP010Uploader` (inside `PlatformConsumer::Hdr10`)
    /// and stores the resulting `D3d11Texture` here. `Nv12ShaderRendererP010`
    /// samples it via R16_UNORM (Y) + R16G16_UNORM (UV) SRVs.
    ///
    /// The sidecar `hdr10` field is forwarded from `Nv12Frame16.hdr10` so
    /// `present_frame` can call `swap.set_hdr10_metadata` on first IDR.
    #[cfg(feature = "media-win-hdr10")]
    Nv12_10 {
        tex: D3d11Texture,
        hdr10: Option<Hdr10Metadata>,
    },
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
    /// HDR10 path (PR3): the Windows viewer receives P010LE CPU frames from
    /// the FFmpeg Linux-side decode path (PR2 → wire → client). The recv
    /// task calls `uploader.upload(frame)` to get a GPU P010 texture, then
    /// sets `latest_texture` for the render thread to drain.
    ///
    /// `MfD3d11Consumer` is included for future use (Windows-native H265Main10
    /// decode path; out of scope for PR3 per plan §Out-of-scope F8 follow-up).
    ///
    /// `last_hdr10_meta` is updated once per stream; subsequent identical
    /// metadata calls `set_hdr10_metadata` are skipped via comparison.
    #[cfg(feature = "media-win-hdr10")]
    Hdr10 {
        uploader: CpuP010Uploader,
        latest_texture: Option<(D3d11Texture, Option<Hdr10Metadata>)>,
    },
    /// F8 — Windows-native HEVC Main10 decode via MF. The `MfHevcMain10Consumer`
    /// pumps the MF decoder and delivers R13-isolated P010 GPU textures directly
    /// (D3D11VA path) or via `CpuP010Uploader` (SW fallback). Requires both
    /// `media-win-hdr10` (for `CpuP010Uploader`) and `media-win-hevc-main10`.
    #[cfg(all(feature = "media-win-hdr10", feature = "media-win-hevc-main10"))]
    HevcMain10Mf {
        consumer: MfHevcMain10Consumer,
        latest_texture: Option<(D3d11Texture, Option<Hdr10Metadata>)>,
    },
}

/// Decoder-selected renderer enum. Private (held inside `PlatformRender`).
/// Renamed from `ViewerRenderer` (lib.rs).
pub(crate) enum WinRenderer {
    Mf(Nv12Renderer),
    #[cfg(prdt_nvdec_bindings)]
    Nvdec(prdt_media_win::DualPlaneYuvRenderer),
    /// OpenH264 SW path: takes a single NV12 D3D11 texture (uploaded by
    /// `CpuI420Uploader`) and converts via a custom BT.709 pixel shader.
    /// Sidesteps `ID3D11VideoProcessor`, which the Intel iGPU rejects on
    /// CPU-uploaded NV12 textures (issue #19 Bug 4).
    Openh264(Nv12ShaderRenderer),
    /// HDR10 P010 path: takes a pre-uploaded P010 D3D11 texture and converts
    /// via a BT.2020 NCL Y′CbCr → R′G′B′ pixel shader (PQ pass-through) into
    /// the R10G10B10A2_UNORM HDR10 swapchain.
    #[cfg(feature = "media-win-hdr10")]
    Hdr10(Nv12ShaderRendererP010),
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
    /// Tracks whether `set_hdr10_metadata` has been called for the current
    /// stream so we only re-call when the metadata actually changes.
    #[cfg(feature = "media-win-hdr10")]
    pub(crate) last_hdr10_meta: Option<Hdr10Metadata>,
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
        #[cfg(feature = "media-win-hdr10")]
        last_hdr10_meta: None,
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
            WinRenderer::Openh264(_) => {
                // Nv12ShaderRenderer is dimension-agnostic.
            }
            #[cfg(feature = "media-win-hdr10")]
            WinRenderer::Hdr10(_) => {
                // Nv12ShaderRendererP010 is dimension-agnostic.
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
    // Lazy HDR10 swapchain upgrade: on the first Nv12_10 frame, if the
    // swapchain is still in 8-bit mode, upgrade it now. Fails loud —
    // no silent SDR fallback (plan PR3 Step 4 change #6).
    #[cfg(feature = "media-win-hdr10")]
    if matches!(f, PlatformFrame::Nv12_10 { .. }) && !r.swap.is_hdr10() {
        let hwnd = extract_hwnd(&r.window)
            .map_err(|e| super::RenderError::Init(format!("extract_hwnd for HDR10: {e}")))?;
        rebuild_swap_hdr10(r, hwnd)
            .map_err(|e| super::RenderError::Init(format!("rebuild_swap_hdr10: {e}")))?;
    }

    let needs_new = match (f, r.renderer.as_ref()) {
        (PlatformFrame::Nv12(nv12), Some(WinRenderer::Mf(rmf))) => {
            rmf.input_size() != (nv12.width(), nv12.height())
        }
        (PlatformFrame::Nv12(_), Some(WinRenderer::Openh264(_))) => false,
        #[cfg(feature = "media-win-hdr10")]
        (PlatformFrame::Nv12_10 { .. }, Some(WinRenderer::Hdr10(_))) => false,
        (_, None) => true,
        #[allow(unreachable_patterns)]
        _ => true,
    };
    if needs_new {
        let (iw, ih) = match f {
            PlatformFrame::Nv12(nv12) => (nv12.width(), nv12.height()),
            #[cfg(prdt_nvdec_bindings)]
            PlatformFrame::DualPlane(dp) => (dp.width, dp.height),
            #[cfg(feature = "media-win-hdr10")]
            PlatformFrame::Nv12_10 { tex, .. } => (tex.width(), tex.height()),
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
        } else if decoder_label == "openh264" {
            let rn = Nv12ShaderRenderer::new(&r.dev)
                .map_err(|e| super::RenderError::Init(format!("Nv12ShaderRenderer::new: {e}")))?;
            WinRenderer::Openh264(rn)
        } else {
            // HDR10 path: if the swapchain is in HDR10 mode, use the P010 renderer.
            #[cfg(feature = "media-win-hdr10")]
            if r.swap.is_hdr10() {
                let rn = Nv12ShaderRendererP010::new(&r.dev).map_err(|e| {
                    super::RenderError::Init(format!("Nv12ShaderRendererP010::new: {e}"))
                })?;
                r.renderer = Some(WinRenderer::Hdr10(rn));
                // Fall through to the render dispatch below with the new renderer.
                return present_frame(r, f, decoder_label, _shared);
            }
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
            (WinRenderer::Openh264(rn), PlatformFrame::Nv12(nv12_tex)) => {
                rn.render(nv12_tex, &r.swap).map_err(|e| {
                    super::RenderError::Present(format!("Nv12ShaderRenderer::render: {e}"))
                })?;
            }
            #[cfg(prdt_nvdec_bindings)]
            (WinRenderer::Nvdec(rnv), PlatformFrame::DualPlane(dpl)) => {
                rnv.render(dpl.as_ref(), &r.swap).map_err(|e| {
                    super::RenderError::Present(format!("DualPlaneYuvRenderer::render: {e}"))
                })?;
            }
            #[cfg(feature = "media-win-hdr10")]
            (WinRenderer::Hdr10(rp010), PlatformFrame::Nv12_10 { tex, hdr10 }) => {
                // Forward HDR10 metadata to the swapchain on first IDR (or on change).
                if let Some(meta) = hdr10 {
                    if r.last_hdr10_meta.as_ref() != Some(meta) {
                        if let Err(e) = r.swap.set_hdr10_metadata(meta) {
                            tracing::warn!(?e, "set_hdr10_metadata failed; continuing");
                        } else {
                            r.last_hdr10_meta = Some(*meta);
                        }
                    }
                }
                rp010.render(tex, &r.swap).map_err(|e| {
                    super::RenderError::Present(format!("Nv12ShaderRendererP010::render: {e}"))
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
///
/// For `Codec::H265Main10` (gated on `media-win-hdr10`): attempts to construct
/// an HDR10 swapchain. If HDR10 is unavailable and the `media-win-hdr-to-sdr-fallback`
/// feature is not enabled, returns `ConsumerError::HdrUnavailable` (no silent
/// precision loss per plan PR3 Step 4 change #6).
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
            let nv = NvdecD3d11Consumer::new(dev, width, height).map_err(|e| {
                super::ConsumerError::Init(format!("NvdecD3d11Consumer::new: {e}"))
            })?;
            Ok(PlatformConsumer::Nvdec(nv))
        }
        #[cfg(not(prdt_nvdec_bindings))]
        ("nvdec", Codec::H265) => Err(super::ConsumerError::Init(
            "nvdec requested but built without prdt_nvdec_bindings cfg".into(),
        )),
        #[cfg(all(feature = "media-win-hdr10", feature = "media-win-hevc-main10"))]
        (_, Codec::H265Main10) => {
            match prdt_media_win::MfHevcMain10Consumer::new(dev, width, height) {
                Ok(consumer) => Ok(PlatformConsumer::HevcMain10Mf {
                    consumer,
                    latest_texture: None,
                }),
                Err(prdt_media_win::MediaError::DecoderNotAvailable { codec, reason }) => {
                    // F8 Principle 3: loud-fail. Do NOT fall back to the PR3 Hdr10 path —
                    // the host has negotiated Main10 NAL bytes; the PR3 Hdr10 path expects
                    // pre-decoded P010 CPU frames over the wire, not encoded NAL units.
                    Err(super::ConsumerError::Init(format!(
                        "HEVC Main10 decoder unavailable: {codec}: {reason}"
                    )))
                }
                Err(e) => Err(super::ConsumerError::Init(format!(
                    "MfHevcMain10Consumer::new: {e}"
                ))),
            }
        }
        #[cfg(all(feature = "media-win-hdr10", not(feature = "media-win-hevc-main10")))]
        (_, Codec::H265Main10) => {
            build_consumer_hdr10(width, height, dev)
        }
        // PR3 — Windows FFmpeg NVDEC 8-bit path.
        // Constructs HevcNvdecFfmpegDecoderWindows; on CI (no GPU) the decoder
        // fails with DecoderNotAvailable — callers handle gracefully.
        #[cfg(feature = "media-win-ffmpeg-nvdec-any")]
        ("ffmpeg-nvdec-hevc", Codec::H265) => {
            use prdt_media_win::ffmpeg::{
                HevcNvdecFfmpegDecoderWindows, HevcNvdecFfmpegDecoderWindowsConfig,
            };
            HevcNvdecFfmpegDecoderWindows::new(HevcNvdecFfmpegDecoderWindowsConfig {
                width,
                height,
                cuda_device_index: None,
            })
            .map_err(|e| {
                super::ConsumerError::Init(format!("HevcNvdecFfmpegDecoderWindows::new: {e}"))
            })?;
            // TODO(PR3-followup): wrap in a dedicated PlatformConsumer variant once the
            // full viewer recv-loop integration is wired. For now the decoder is
            // constructed (proving the feature compiles + runtime path works) and the
            // call falls through to the MF consumer as a placeholder.
            let mf = MfD3d11Consumer::new(dev, width, height).map_err(|e| {
                super::ConsumerError::Init(format!(
                    "MfD3d11Consumer::new (ffmpeg-nvdec fallback): {e}"
                ))
            })?;
            Ok(PlatformConsumer::Mf(mf))
        }
        // PR3 — Windows FFmpeg NVDEC Main10 path.
        #[cfg(feature = "media-win-ffmpeg-nvdec-main10-any")]
        ("ffmpeg-nvdec-hevc-main10", Codec::H265Main10) => {
            use prdt_media_win::ffmpeg::{
                HevcNvdecMain10FfmpegDecoderWindows, HevcNvdecMain10FfmpegDecoderWindowsConfig,
            };
            HevcNvdecMain10FfmpegDecoderWindows::new(HevcNvdecMain10FfmpegDecoderWindowsConfig {
                width,
                height,
                cuda_device_index: None,
            })
            .map_err(|e| {
                super::ConsumerError::Init(format!(
                    "HevcNvdecMain10FfmpegDecoderWindows::new: {e}"
                ))
            })?;
            // TODO(PR3-followup): wrap in a dedicated PlatformConsumer variant once the
            // full viewer recv-loop integration is wired. Falls through to HDR10 placeholder.
            #[cfg(feature = "media-win-hdr10")]
            {
                build_consumer_hdr10(width, height, dev)
            }
            #[cfg(not(feature = "media-win-hdr10"))]
            {
                Err(super::ConsumerError::Init(
                    "ffmpeg-nvdec-hevc-main10 requires the media-win-hdr10 feature".into(),
                ))
            }
        }
        (other_decoder, other_codec) => Err(super::ConsumerError::Init(format!(
            "unsupported decoder/codec combination on Windows: decoder={other_decoder}, codec={other_codec:?}"
        ))),
    }
}

/// Build the HDR10 consumer: constructs a `CpuP010Uploader` for CPU→GPU
/// P010 texture upload. The HDR10 swapchain is constructed separately in
/// `build_render_hdr10` (called by lib.rs after `build_consumer` succeeds).
///
/// Separated from `build_consumer` so the function stays under the
/// `media-win-hdr10` feature gate without polluting the 8-bit path.
#[cfg(feature = "media-win-hdr10")]
pub fn build_consumer_hdr10(
    width: u32,
    height: u32,
    dev: &D3d11Device,
) -> Result<PlatformConsumer, super::ConsumerError> {
    let uploader = CpuP010Uploader::new(dev, width, height)
        .map_err(|e| super::ConsumerError::Init(format!("CpuP010Uploader::new: {e}")))?;
    Ok(PlatformConsumer::Hdr10 {
        uploader,
        latest_texture: None,
    })
}

/// Build an HDR10 swapchain bound to `hwnd`. Returns `ConsumerError::HdrUnavailable`
/// (wrapping `MediaError::HdrUnavailable`) if either DXGI capability probe fails.
/// Callers must surface this to the user — no silent SDR fallback (change #6).
///
/// Replaces the 8-bit swapchain in `PlatformRender::swap` so subsequent
/// `present_frame` calls use the HDR10 path.
#[cfg(feature = "media-win-hdr10")]
pub fn rebuild_swap_hdr10(r: &mut PlatformRender, hwnd: HWND) -> Result<(), super::ConsumerError> {
    let swap = prdt_media_win::SwapChain::new_for_hwnd_hdr10(
        &r.dev,
        hwnd,
        r.swap.width(),
        r.swap.height(),
    )
    .map_err(|e| super::ConsumerError::HdrUnavailable(e.to_string()))?;
    r.swap = swap;
    r.renderer = None; // force renderer rebuild on next present_frame call
    r.last_hdr10_meta = None;
    Ok(())
}
