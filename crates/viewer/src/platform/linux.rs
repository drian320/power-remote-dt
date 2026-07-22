//! Linux viewer backend. Wraps `prdt-media-sw::Openh264Decoder` for
//! decode + softbuffer for present, with `prdt_media_linux::i420_to_bgra`
//! for color conversion. Mirrors the cross-platform API surface defined
//! in `platform/mod.rs`.

#![cfg(target_os = "linux")]

use std::num::NonZeroU32;
use std::sync::Arc;

use prdt_input_linux::{
    clipboard_sequence_number as _input_linux_clipboard_sequence_number,
    read_clipboard_text as _input_linux_read_clipboard_text,
    virtual_desktop_rect as _input_linux_virtual_desktop_rect,
    write_clipboard_text as _input_linux_write_clipboard_text,
    MAX_CLIPBOARD_BYTES as _INPUT_LINUX_MAX,
};
use prdt_media_linux::i420_to_bgra::i420_to_bgra;
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any"
))]
use prdt_media_linux::Nv12Frame;
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any"
))]
use prdt_media_linux::Nv12Frame16;
use prdt_media_sw::{I420Frame, Openh264Decoder};
use prdt_protocol::{frame::Codec, MonitorRect};
use winit::window::Window;

/// Re-exported max clipboard bytes; identical value across OSes.
pub const MAX_CLIPBOARD_BYTES: usize = _INPUT_LINUX_MAX;

/// Per-OS decoded frame. Pre-P2 Linux had only the I420 (OpenH264) path;
/// P2 adds an NV12 variant for the three FFmpeg HEVC decode backends so
/// the renderer can blit NV12 → BGRA without an intermediate I420
/// conversion. The I420 H.264 path stays untouched.
pub enum PlatformFrame {
    I420(Arc<I420Frame>),
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    Nv12(Arc<Nv12Frame>),
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    Nv12_10(Arc<Nv12Frame16>),
}

/// Per-OS decoder/consumer. Pre-P2 Linux had only the Openh264 arm; P2
/// adds three FFmpeg HEVC backends, each behind its own feature gate so
/// the exhaustive `match` over `PlatformConsumer` in `recv_task` stays
/// well-defined for any subset of compiled backends. The Openh264 arm
/// preserves byte-for-byte semantics (the H.264 hot path is sacrosanct
/// per the P2 plan's regression-safety principle).
pub enum PlatformConsumer {
    Openh264 {
        decoder: Openh264Decoder,
        latest: Option<Arc<I420Frame>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
    FfmpegHevcSw {
        decoder: prdt_media_linux::HevcSwFfmpegDecoderAdapter,
        latest: Option<Arc<Nv12Frame>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-vaapi-any")]
    FfmpegHevcVaapi {
        decoder: prdt_media_linux::HevcVaapiFfmpegDecoderAdapter,
        latest: Option<Arc<Nv12Frame>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-nvdec-any")]
    FfmpegHevcNvdec {
        decoder: prdt_media_linux::HevcNvdecFfmpegDecoderAdapter,
        latest: Option<Arc<Nv12Frame>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-sw-main10-any")]
    FfmpegHevcSwMain10 {
        decoder: prdt_media_linux::HevcSwMain10FfmpegDecoder,
        latest: Option<Arc<Nv12Frame16>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-vaapi-main10-any")]
    FfmpegHevcVaapiMain10 {
        decoder: prdt_media_linux::HevcVaapiMain10FfmpegDecoder,
        latest: Option<Arc<Nv12Frame16>>,
        needs_idr: bool,
    },
    #[cfg(feature = "ffmpeg-decode-hevc-nvdec-main10-any")]
    FfmpegHevcNvdecMain10 {
        decoder: prdt_media_linux::HevcNvdecMain10FfmpegDecoder,
        latest: Option<Arc<Nv12Frame16>>,
        needs_idr: bool,
    },
}

/// Per-OS render state. P3 turns this into a two-backend enum: the
/// historical softbuffer CPU presenter (DEFAULT) and an opt-in wgpu GPU
/// presenter selected via `PRDT_LINUX_RENDERER=wgpu`. The public name,
/// the `window()` accessor and the free-function API (`build_render`,
/// `present_frame`, `resize_renderer`) are preserved so lib.rs call
/// sites are untouched.
pub enum PlatformRender {
    Softbuffer(SoftbufferRender),
    Wgpu(Box<crate::platform::linux_wgpu::WgpuRender>),
}

/// softbuffer CPU presenter. Wraps softbuffer's Surface + a scratch BGRA
/// buffer used to convert I420/NV12/P010 → BGRA before blitting into the
/// surface's `&mut [u32]` framebuffer. This is the pre-P3 `PlatformRender`
/// struct, moved here verbatim.
pub struct SoftbufferRender {
    window: Arc<Window>,
    // softbuffer 0.4 Surface is generic over (D, W) where D:
    // HasDisplayHandle and W: HasWindowHandle. Arc<Window> satisfies
    // both, so D = W = Arc<Window>.
    _ctx: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// I420/NV12/P010 → BGRA conversion scratch, sized to the decoded FRAME
    /// (`last_size`). Re-allocated on frame-size change. Blitted (cropped /
    /// letterboxed) into the window-sized `surface`.
    scratch_bgra: Vec<u8>,
    /// Cached decoded-FRAME dimensions (== `scratch_bgra` geometry). Gates
    /// redundant scratch reallocations.
    last_size: (u32, u32),
    /// Cached softbuffer SURFACE dimensions (== the window's inner size, which
    /// the OS may clamp below the frame size). Gates redundant surface resizes
    /// and drives the crop/letterbox math in `blit_scratch_to_surface`.
    surface_size: (u32, u32),
}

impl PlatformRender {
    /// Borrow the underlying window. Used by lib.rs to call
    /// `request_redraw`, `set_title`, `inner_size`, etc., without leaking
    /// the platform-specific render-state internals.
    pub fn window(&self) -> &Window {
        match self {
            PlatformRender::Softbuffer(r) => &r.window,
            PlatformRender::Wgpu(r) => r.window(),
        }
    }
}

/// Build the Linux render state. Called by lib.rs in `resumed()`.
///
/// Backend selection: when `PRDT_LINUX_RENDERER=wgpu` we try the GPU
/// presenter and, on any init error, log a warning and fall back to
/// softbuffer. With no env var (or any other value) softbuffer is the
/// default, so the build works on machines/CI without a working wgpu
/// surface.
pub fn build_render(
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> Result<PlatformRender, super::RenderError> {
    if std::env::var("PRDT_LINUX_RENDERER").as_deref() == Ok("wgpu") {
        match crate::platform::linux_wgpu::WgpuRender::new(Arc::clone(&window), width, height) {
            Ok(r) => {
                tracing::info!(target: "video.pipeline", renderer = "wgpu", "Linux GPU presenter active");
                return Ok(PlatformRender::Wgpu(Box::new(r)));
            }
            Err(e) => {
                tracing::warn!(
                    target: "video.pipeline",
                    error = %e,
                    "PRDT_LINUX_RENDERER=wgpu requested but wgpu init failed; falling back to softbuffer"
                );
            }
        }
    }
    build_softbuffer_render(window, width, height).map(PlatformRender::Softbuffer)
}

/// Build the softbuffer CPU render state (pre-P3 `build_render` body).
fn build_softbuffer_render(
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> Result<SoftbufferRender, super::RenderError> {
    let ctx = softbuffer::Context::new(Arc::clone(&window))
        .map_err(|e| super::RenderError::Init(format!("softbuffer::Context::new: {e}")))?;
    let mut surface = softbuffer::Surface::new(&ctx, Arc::clone(&window))
        .map_err(|e| super::RenderError::Init(format!("softbuffer::Surface::new: {e}")))?;
    let nz_w = NonZeroU32::new(width.max(1)).expect("non-zero width");
    let nz_h = NonZeroU32::new(height.max(1)).expect("non-zero height");
    surface
        .resize(nz_w, nz_h)
        .map_err(|e| super::RenderError::Init(format!("Surface::resize: {e}")))?;
    // On Wayland a wl_surface stays unmapped until the first buffer is
    // committed (Wayland spec). Our render path only commits when a
    // decoded frame arrives, so without this initial blank present the
    // window never appears on screen until the first successful decode.
    // Commit a black/transparent buffer once so the compositor maps the
    // window immediately.
    {
        let mut buf = surface
            .buffer_mut()
            .map_err(|e| super::RenderError::Init(format!("initial buffer_mut: {e}")))?;
        buf.fill(0);
        buf.present()
            .map_err(|e| super::RenderError::Init(format!("initial present: {e}")))?;
    }
    Ok(SoftbufferRender {
        window,
        _ctx: ctx,
        surface,
        scratch_bgra: vec![0u8; (width * height * 4) as usize],
        last_size: (width, height),
        surface_size: (width, height),
    })
}

/// Build the consumer for the negotiated codec. Pre-P2 Linux only
/// supported openh264 (CPU H.264). P2 adds three opt-in FFmpeg HEVC
/// backends (sw / vaapi / nvdec) for the H.265 path; each is reachable
/// either by an explicit `--decoder ffmpeg-{sw,vaapi,nvdec}-hevc` arg
/// or via `--decoder auto` when only one HEVC backend is compiled in.
/// The OpenH264 H.264 arm is byte-for-byte unchanged.
pub fn build_consumer(
    decoder_arg: &str,
    codec: Codec,
    #[cfg_attr(
        not(any(
            feature = "ffmpeg-decode-hevc-sw-any",
            feature = "ffmpeg-decode-hevc-vaapi-any",
            feature = "ffmpeg-decode-hevc-nvdec-any"
        )),
        allow(unused_variables)
    )]
    width: u32,
    #[cfg_attr(
        not(any(
            feature = "ffmpeg-decode-hevc-sw-any",
            feature = "ffmpeg-decode-hevc-vaapi-any",
            feature = "ffmpeg-decode-hevc-nvdec-any"
        )),
        allow(unused_variables)
    )]
    height: u32,
) -> Result<PlatformConsumer, super::ConsumerError> {
    match (decoder_arg, codec) {
        // ── H.264 hot path (SACROSANCT — must not change) ──────────────────
        ("openh264" | "auto", Codec::H264) => {
            let dec = Openh264Decoder::new()
                .map_err(|e| super::ConsumerError::Init(format!("Openh264Decoder::new: {e}")))?;
            Ok(PlatformConsumer::Openh264 {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        // ── P2 HEVC dispatch ───────────────────────────────────────────────
        #[cfg(feature = "ffmpeg-decode-hevc-sw-any")]
        ("ffmpeg-sw-hevc", Codec::H265) => {
            let dec = prdt_media_linux::build_ffmpeg_sw_hevc_decoder(width, height)
                .map_err(|e| super::ConsumerError::Init(format!("ffmpeg-sw-hevc: {e}")))?;
            Ok(PlatformConsumer::FfmpegHevcSw {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(feature = "ffmpeg-decode-hevc-vaapi-any")]
        ("ffmpeg-vaapi-hevc", Codec::H265) => {
            let dec = prdt_media_linux::build_ffmpeg_vaapi_hevc_decoder(width, height)
                .map_err(|e| super::ConsumerError::Init(format!("ffmpeg-vaapi-hevc: {e}")))?;
            Ok(PlatformConsumer::FfmpegHevcVaapi {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(feature = "ffmpeg-decode-hevc-nvdec-any")]
        ("ffmpeg-nvdec-hevc", Codec::H265) => {
            let dec = prdt_media_linux::build_ffmpeg_nvdec_hevc_decoder(width, height)
                .map_err(|e| super::ConsumerError::Init(format!("ffmpeg-nvdec-hevc: {e}")))?;
            Ok(PlatformConsumer::FfmpegHevcNvdec {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(any(
            feature = "ffmpeg-decode-hevc-sw-any",
            feature = "ffmpeg-decode-hevc-vaapi-any",
            feature = "ffmpeg-decode-hevc-nvdec-any"
        ))]
        ("auto", Codec::H265) => {
            let pick = resolve_auto_decode_hevc();
            build_consumer(pick, Codec::H265, width, height)
        }
        // ── P3.2 HEVC Main10 dispatch ──────────────────────────────────────
        #[cfg(feature = "ffmpeg-decode-hevc-sw-main10-any")]
        ("ffmpeg-sw-hevc-main10", Codec::H265Main10) => {
            let dec = prdt_media_linux::build_ffmpeg_sw_hevc_main10_decoder(width, height)
                .map_err(|e| super::ConsumerError::Init(format!("ffmpeg-sw-hevc-main10: {e}")))?;
            Ok(PlatformConsumer::FfmpegHevcSwMain10 {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(feature = "ffmpeg-decode-hevc-vaapi-main10-any")]
        ("ffmpeg-vaapi-hevc-main10", Codec::H265Main10) => {
            let dec = prdt_media_linux::build_ffmpeg_vaapi_hevc_main10_decoder(width, height)
                .map_err(|e| {
                    super::ConsumerError::Init(format!("ffmpeg-vaapi-hevc-main10: {e}"))
                })?;
            Ok(PlatformConsumer::FfmpegHevcVaapiMain10 {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(feature = "ffmpeg-decode-hevc-nvdec-main10-any")]
        ("ffmpeg-nvdec-hevc-main10", Codec::H265Main10) => {
            let dec = prdt_media_linux::build_ffmpeg_nvdec_hevc_main10_decoder(width, height)
                .map_err(|e| {
                    super::ConsumerError::Init(format!("ffmpeg-nvdec-hevc-main10: {e}"))
                })?;
            Ok(PlatformConsumer::FfmpegHevcNvdecMain10 {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        #[cfg(any(
            feature = "ffmpeg-decode-hevc-sw-main10-any",
            feature = "ffmpeg-decode-hevc-vaapi-main10-any",
            feature = "ffmpeg-decode-hevc-nvdec-main10-any"
        ))]
        ("auto", Codec::H265Main10) => {
            let pick = resolve_auto_decode_hevc_main10();
            build_consumer(pick, Codec::H265Main10, width, height)
        }
        // ── Reject everything else ─────────────────────────────────────────
        (other_decoder, other_codec) => Err(super::ConsumerError::Init(format!(
            "unsupported decoder/codec on Linux: decoder={other_decoder}, codec={other_codec:?} \
             (Linux supports openh264+H264 plus opt-in ffmpeg-*-hevc backends for H265)"
        ))),
    }
}

/// Pick a HEVC decode backend based on compiled features + the
/// `PRDT_PREFER_NVDEC` env var. Priority order (deliberately inverted
/// vs encode-side `PRDT_PREFER_NVENC`): VAAPI → NVDEC → SW. Reason:
/// decode is power-bound on hybrid laptops; iGPU at ~5 W beats dGPU at
/// ~25 W at the same workload, and waking the dGPU disables panel
/// self-refresh + delays its return to idle. `PRDT_PREFER_NVDEC=1`
/// (truthy: `{1,true,yes,on}` case-insensitive) flips to NVDEC for
/// users on desktops / always-plugged-in machines.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any"
))]
// `return` keeps the function single-expression across every cfg
// combination — without it the cascade of cfg-gated branches needs a
// trailing `unreachable!()` that's actually reachable depending on
// feature set.
#[allow(clippy::needless_return)]
fn resolve_auto_decode_hevc() -> &'static str {
    let prefer_nvdec = std::env::var("PRDT_PREFER_NVDEC")
        .ok()
        .map(|v| {
            let lc = v.to_ascii_lowercase();
            matches!(lc.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);

    #[cfg(all(
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    {
        if prefer_nvdec {
            tracing::info!(
                target: "video.pipeline",
                decoder = "ffmpeg-nvdec-hevc",
                selected_by = "auto",
                reason = "preferred-over-vaapi-by-env",
                "video decoder selected"
            );
            return "ffmpeg-nvdec-hevc";
        }
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-vaapi-hevc",
            selected_by = "auto",
            reason = "preferred-over-nvdec",
            "video decoder selected"
        );
        return "ffmpeg-vaapi-hevc";
    }
    // Single-backend builds: the cfg cascade below picks the only one
    // that's compiled in. The `prefer_nvdec` env var is silently ignored
    // when its target backend isn't available.
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-vaapi-any",
        not(feature = "ffmpeg-decode-hevc-nvdec-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-vaapi-hevc",
            selected_by = "auto",
            reason = "only-vaapi-compiled",
            "video decoder selected"
        );
        return "ffmpeg-vaapi-hevc";
    }
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-nvdec-any",
        not(feature = "ffmpeg-decode-hevc-vaapi-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-nvdec-hevc",
            selected_by = "auto",
            reason = "only-nvdec-compiled",
            "video decoder selected"
        );
        return "ffmpeg-nvdec-hevc";
    }
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-sw-any",
        not(feature = "ffmpeg-decode-hevc-vaapi-any"),
        not(feature = "ffmpeg-decode-hevc-nvdec-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-sw-hevc",
            selected_by = "auto",
            reason = "only-sw-compiled",
            "video decoder selected"
        );
        "ffmpeg-sw-hevc"
    }
}

/// Pick a HEVC Main10 decode backend based on compiled features + the
/// `PRDT_PREFER_NVDEC` env var. Priority order: NVDEC → VAAPI → SW
/// (per team-lead spec: nvdec_main10 > vaapi_main10 > sw_main10).
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any"
))]
#[allow(clippy::needless_return)]
fn resolve_auto_decode_hevc_main10() -> &'static str {
    let prefer_nvdec = std::env::var("PRDT_PREFER_NVDEC")
        .ok()
        .map(|v| {
            let lc = v.to_ascii_lowercase();
            matches!(lc.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);

    #[cfg(all(
        feature = "ffmpeg-decode-hevc-nvdec-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any"
    ))]
    {
        if prefer_nvdec {
            tracing::info!(
                target: "video.pipeline",
                decoder = "ffmpeg-nvdec-hevc-main10",
                selected_by = "auto",
                reason = "preferred-over-vaapi-by-env",
                "video decoder selected"
            );
            return "ffmpeg-nvdec-hevc-main10";
        }
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-nvdec-hevc-main10",
            selected_by = "auto",
            reason = "preferred-over-vaapi",
            "video decoder selected"
        );
        return "ffmpeg-nvdec-hevc-main10";
    }
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-nvdec-main10-any",
        not(feature = "ffmpeg-decode-hevc-vaapi-main10-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-nvdec-hevc-main10",
            selected_by = "auto",
            reason = "only-nvdec-compiled",
            "video decoder selected"
        );
        return "ffmpeg-nvdec-hevc-main10";
    }
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        not(feature = "ffmpeg-decode-hevc-nvdec-main10-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-vaapi-hevc-main10",
            selected_by = "auto",
            reason = "only-vaapi-compiled",
            "video decoder selected"
        );
        return "ffmpeg-vaapi-hevc-main10";
    }
    #[cfg(all(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        not(feature = "ffmpeg-decode-hevc-vaapi-main10-any"),
        not(feature = "ffmpeg-decode-hevc-nvdec-main10-any")
    ))]
    {
        let _ = prefer_nvdec;
        tracing::info!(
            target: "video.pipeline",
            decoder = "ffmpeg-sw-hevc-main10",
            selected_by = "auto",
            reason = "only-sw-compiled",
            "video decoder selected"
        );
        "ffmpeg-sw-hevc-main10"
    }
}

/// Present one decoded frame on the existing render state. Lazily
/// resizes the softbuffer surface to match the stream size on first
/// frame or stream-size change.
///
/// P2 rewrite: the body used to live inside an irrefutable
/// `let PlatformFrame::I420(..) = f;` binding. With the new
/// `PlatformFrame::Nv12` variant the destructure has to become a
/// `match`. The I420 arm is byte-for-byte identical to the pre-P2 body
/// (stream-size resize, i420_to_bgra, cursor composite, present); the
/// new Nv12 arm reuses the same scratch/cursor/present blocks and only
/// swaps the color-conversion helper.
pub fn present_frame(
    r: &mut PlatformRender,
    f: &PlatformFrame,
    decoder_label: &str,
    shared: &crate::ViewerShared,
) -> Result<(), super::RenderError> {
    match r {
        PlatformRender::Softbuffer(sb) => present_frame_softbuffer(sb, f, decoder_label, shared),
        PlatformRender::Wgpu(w) => w.present_frame(f, shared),
    }
}

/// softbuffer present path (pre-P3 `present_frame` body, byte-for-byte).
fn present_frame_softbuffer(
    r: &mut SoftbufferRender,
    f: &PlatformFrame,
    _decoder_label: &str,
    shared: &crate::ViewerShared,
) -> Result<(), super::RenderError> {
    match f {
        PlatformFrame::I420(i420) => {
            let stream_w = i420.width;
            let stream_h = i420.height;

            resize_scratch_if_needed(r, stream_w, stream_h);

            // I420 → BGRA via the existing helper (BT.709 limited-range,
            // alpha 0xFF). Output layout matches softbuffer's LE u32 expectation
            // (B in lowest byte, A=0xFF in highest).
            if let Err(e) = i420_to_bgra(i420, &mut r.scratch_bgra) {
                tracing::warn!(
                    target: "video.pipeline",
                    error = %e,
                    "skipping I420 frame: geometry inconsistent with render buffer"
                );
                return Ok(());
            }

            composite_cursor(r, shared, stream_w, stream_h);
            blit_scratch_to_surface(r)?;
        }
        #[cfg(any(
            feature = "ffmpeg-decode-hevc-sw-any",
            feature = "ffmpeg-decode-hevc-vaapi-any",
            feature = "ffmpeg-decode-hevc-nvdec-any"
        ))]
        PlatformFrame::Nv12(nv12) => {
            let stream_w = nv12.width;
            let stream_h = nv12.height;

            resize_scratch_if_needed(r, stream_w, stream_h);

            if let Err(e) = nv12_to_bgra(nv12, &mut r.scratch_bgra) {
                tracing::warn!(
                    target: "video.pipeline",
                    error = %e,
                    "skipping NV12 frame: geometry inconsistent with render buffer"
                );
                return Ok(());
            }

            composite_cursor(r, shared, stream_w, stream_h);
            blit_scratch_to_surface(r)?;
        }
        #[cfg(any(
            feature = "ffmpeg-decode-hevc-sw-main10-any",
            feature = "ffmpeg-decode-hevc-vaapi-main10-any",
            feature = "ffmpeg-decode-hevc-nvdec-main10-any"
        ))]
        PlatformFrame::Nv12_10(nv12_10) => {
            let stream_w = nv12_10.width;
            let stream_h = nv12_10.height;

            resize_scratch_if_needed(r, stream_w, stream_h);

            if let Err(e) = p010_to_bgra_sdr_tonemap(nv12_10, &mut r.scratch_bgra) {
                tracing::warn!(
                    target: "video.pipeline",
                    error = %e,
                    "skipping P010 frame: geometry inconsistent with render buffer"
                );
                return Ok(());
            }

            composite_cursor(r, shared, stream_w, stream_h);
            blit_scratch_to_surface(r)?;
        }
    }

    Ok(())
}

/// Resize the BGRA conversion scratch to the decoded FRAME dimensions. The
/// converters write exactly `frame_w * frame_h * 4` bytes and the cursor is
/// composited in frame space, so the scratch tracks the frame — independent of
/// the window-sized `surface` (see `resize_surface_to_window`). Cannot fail;
/// the surface resize (which can) is a separate step.
fn resize_scratch_if_needed(r: &mut SoftbufferRender, frame_w: u32, frame_h: u32) {
    if r.last_size != (frame_w, frame_h) {
        tracing::info!(
            target: "video.pipeline",
            from_w = r.last_size.0,
            from_h = r.last_size.1,
            to_w = frame_w,
            to_h = frame_h,
            "softbuffer scratch resized to decoded frame dimensions"
        );
        r.scratch_bgra
            .resize(frame_w as usize * frame_h as usize * 4, 0);
        r.last_size = (frame_w, frame_h);
    }
}

/// Ensure the softbuffer surface matches the window's current inner size.
/// softbuffer presents into the window's drawable, so the surface must track
/// the WINDOW — never the decoded frame, which is often larger (the OS clamps
/// the window to the screen while the host captures at full resolution).
/// Presenting a frame-sized buffer into a smaller window is what rendered the
/// NVDEC session as garbage.
fn resize_surface_to_window(r: &mut SoftbufferRender) -> Result<(), super::RenderError> {
    let size = r.window.inner_size();
    let (w, h) = (size.width.max(1), size.height.max(1));
    if r.surface_size != (w, h) {
        tracing::info!(
            target: "video.pipeline",
            from_w = r.surface_size.0,
            from_h = r.surface_size.1,
            to_w = w,
            to_h = h,
            "softbuffer surface resized to window inner size"
        );
        let nz_w = NonZeroU32::new(w).expect("non-zero window width");
        let nz_h = NonZeroU32::new(h).expect("non-zero window height");
        r.surface
            .resize(nz_w, nz_h)
            .map_err(|e| super::RenderError::Present(format!("Surface::resize: {e}")))?;
        r.surface_size = (w, h);
    }
    Ok(())
}

/// Composite the cursor bitmap (if any) onto `r.scratch_bgra`. P5B-2b:
/// briefly take the cursor lock, copy out the values we need, then drop
/// the lock before the blend so we don't hold it across the CPU op.
fn composite_cursor(
    r: &mut SoftbufferRender,
    shared: &crate::ViewerShared,
    stream_w: u32,
    stream_h: u32,
) {
    if let Ok(s) = shared.cursor.lock() {
        if s.visible() {
            if let Some(bmp) = s.bitmap() {
                let top_left_x = s.position_x - s.hotspot_x;
                let top_left_y = s.position_y - s.hotspot_y;
                let bmp_w = bmp.width as i32;
                let bmp_h = bmp.height as i32;
                let bgra_copy = bmp.bgra.clone();
                drop(s);
                alpha_blend_bgra(
                    &mut r.scratch_bgra,
                    stream_w as i32,
                    stream_h as i32,
                    bmp_w,
                    bmp_h,
                    top_left_x,
                    top_left_y,
                    &bgra_copy,
                );
            }
        }
    }
}

/// Blit `r.scratch_bgra` (sized to the decoded FRAME) into the softbuffer
/// surface (sized to the WINDOW) and present. Frame and window routinely
/// differ — the OS clamps the window to the screen while the host streams at
/// capture size — so this crops the frame to the window and letterboxes any
/// surplus window area with black, top-left aligned at 1:1.
///
/// The old code `copy_from_slice`d the whole frame into the surface, which
/// only worked when the two were byte-identical; once the window was clamped
/// smaller than the frame it presented an oversized buffer that the backend
/// rendered as garbage. All row math is bounded by the surface buffer's real
/// length so a stale tracked size can never overrun it.
fn blit_scratch_to_surface(r: &mut SoftbufferRender) -> Result<(), super::RenderError> {
    resize_surface_to_window(r)?;

    let (frame_w, frame_h) = (r.last_size.0 as usize, r.last_size.1 as usize);
    let win_w = r.surface_size.0 as usize;
    let src = &r.scratch_bgra;

    let mut buf = r
        .surface
        .buffer_mut()
        .map_err(|e| super::RenderError::Present(format!("Surface::buffer_mut: {e}")))?;
    let dst: &mut [u8] = bytemuck::cast_slice_mut(&mut buf);
    blit_crop_letterbox(src, frame_w, frame_h, dst, win_w);

    buf.present()
        .map_err(|e| super::RenderError::Present(format!("Surface::present: {e}")))?;
    Ok(())
}

/// Copy a frame-sized BGRA source (`frame_w` × `frame_h`) into a window-sized
/// BGRA destination (`win_w` × derived rows), cropping the frame to the window
/// and letterboxing surplus window area with black, top-left aligned at 1:1.
///
/// `dst` is the softbuffer surface's raw byte buffer. Its length is treated as
/// authoritative for the row count, so a stale `win_w`/window-height can never
/// index past it. `src` must be exactly `frame_w * frame_h * 4` bytes (the
/// converters and `resize_scratch_if_needed` guarantee this).
fn blit_crop_letterbox(src: &[u8], frame_w: usize, frame_h: usize, dst: &mut [u8], win_w: usize) {
    let stride = win_w * 4;
    let rows = if stride == 0 { 0 } else { dst.len() / stride };
    let copy_w = frame_w.min(win_w);
    let copy_h = frame_h.min(rows);
    for y in 0..rows {
        let drow = &mut dst[y * stride..y * stride + stride];
        if y < copy_h && frame_w != 0 {
            let s = y * frame_w * 4;
            drow[..copy_w * 4].copy_from_slice(&src[s..s + copy_w * 4]);
            drow[copy_w * 4..].fill(0); // letterbox to the right of the frame
        } else {
            drow.fill(0); // letterbox below the frame
        }
    }
}

/// NV12 (Y plane + interleaved UV plane) → BGRA. BT.709 limited-range,
/// alpha 0xFF. Sibling of `i420_to_bgra`; same coefficients, just an
/// interleaved chroma layout (UV byte at offset 0 = U, +1 = V).
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any"
))]
fn nv12_to_bgra(nv12: &Nv12Frame, out_bgra: &mut [u8]) -> Result<(), String> {
    let w = nv12.width as usize;
    let h = nv12.height as usize;
    let y_stride = nv12.stride_y as usize;
    let uv_stride = nv12.stride_uv as usize;
    // Validate up front: a decoded frame whose header geometry does not match
    // its plane buffers (or a scratch buffer sized for a different frame) must
    // be rejected, not indexed blindly — `debug_assert` is compiled out of the
    // release viewer, so the raw indexing below would panic in production.
    // These bounds mirror the exact worst-case indices the loop touches.
    if let Err(e) = check_nv12_like_geometry(
        w,
        h,
        y_stride,
        uv_stride,
        nv12.y.len(),
        nv12.uv.len(),
        out_bgra.len(),
    ) {
        return Err(format!("nv12_to_bgra: {e}"));
    }
    for j in 0..h {
        for i in 0..w {
            let y = nv12.y[j * y_stride + i] as i32;
            let uv_row = (j / 2) * uv_stride;
            let uv_col = (i / 2) * 2;
            let u = nv12.uv[uv_row + uv_col] as i32 - 128;
            let v = nv12.uv[uv_row + uv_col + 1] as i32 - 128;
            // Matches i420_to_bgra: BT.709 coefficients, full-range arithmetic.
            let r = y + ((1793 * v) >> 10);
            let g = y - ((534 * u + 213 * v) >> 10);
            let b = y + ((2115 * u) >> 10);
            let off = (j * w + i) * 4;
            out_bgra[off] = r_clamp(b);
            out_bgra[off + 1] = r_clamp(g);
            out_bgra[off + 2] = r_clamp(r);
            out_bgra[off + 3] = 0xFF;
        }
    }
    Ok(())
}

/// Validate that a NV12/P010-shaped frame's plane lengths and the caller's
/// output buffer are all consistent with the `width`/`height`/`stride`
/// geometry, so the interleaved-chroma converters (`nv12_to_bgra`,
/// `p010_to_bgra_sdr_tonemap`) can index without bounds checks. `y_len` and
/// `uv_len` are element counts (bytes for NV12, u16 elements for P010 — the
/// access pattern is identical). Returns the offending geometry as an error
/// string so the caller can log actual-vs-expected before skipping the frame.
///
/// The bounds are the exact worst-case indices the converters reach on the
/// final pixel `(w-1, h-1)`: chroma row `(h-1)/2`, chroma column pair
/// `((w-1)/2)*2 + 1`. A `saturating_mul` for the output size and an explicit
/// zero-dimension short-circuit keep the `(h-1)` math from underflowing.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any"
))]
fn check_nv12_like_geometry(
    w: usize,
    h: usize,
    y_stride: usize,
    uv_stride: usize,
    y_len: usize,
    uv_len: usize,
    out_len: usize,
) -> Result<(), String> {
    let expect_out = w.saturating_mul(h).saturating_mul(4);
    if out_len != expect_out {
        return Err(format!(
            "out buffer {out_len} bytes but geometry {w}x{h} needs {expect_out}"
        ));
    }
    if w == 0 || h == 0 {
        return Ok(());
    }
    let y_need = (h - 1) * y_stride + w;
    let uv_need = ((h - 1) / 2) * uv_stride + ((w - 1) / 2) * 2 + 2;
    if y_len < y_need {
        return Err(format!(
            "Y plane {y_len} elems < {y_need} needed for {w}x{h} stride_y={y_stride}"
        ));
    }
    if uv_len < uv_need {
        return Err(format!(
            "UV plane {uv_len} elems < {uv_need} needed for {w}x{h} stride_uv={uv_stride}"
        ));
    }
    Ok(())
}

#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any",
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any"
))]
#[inline]
fn r_clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// P010LE (Y/UV u16 planes, valid 10 bits in the high bits per FFmpeg
/// P010LE convention) → BGRA8. Applies a simple Reinhard-style SDR tone
/// map: BT.2020 NCL matrix → linearise via inverse PQ EOTF → Reinhard →
/// BT.709 gamma → clamp to 8-bit. HDR display on Linux is F6 follow-up.
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-main10-any",
    feature = "ffmpeg-decode-hevc-vaapi-main10-any",
    feature = "ffmpeg-decode-hevc-nvdec-main10-any"
))]
fn p010_to_bgra_sdr_tonemap(nv12_10: &Nv12Frame16, out_bgra: &mut [u8]) -> Result<(), String> {
    let w = nv12_10.width as usize;
    let h = nv12_10.height as usize;
    let y_stride = nv12_10.stride_y as usize;
    let uv_stride = nv12_10.stride_uv as usize;
    // Same total-function contract as `nv12_to_bgra`: reject a frame whose
    // u16 plane lengths or the output buffer disagree with the header geometry
    // instead of indexing past the plane (the `debug_assert` is gone in
    // release). Plane lengths here are u16-element counts.
    if let Err(e) = check_nv12_like_geometry(
        w,
        h,
        y_stride,
        uv_stride,
        nv12_10.y.len(),
        nv12_10.uv.len(),
        out_bgra.len(),
    ) {
        return Err(format!("p010_to_bgra_sdr_tonemap: {e}"));
    }

    for j in 0..h {
        for i in 0..w {
            // P010LE: valid 10 bits in the high bits of each u16. Shift right
            // by 6 to extract [0..1023], then normalise to [0.0, 1.0].
            let y_raw = (nv12_10.y[j * y_stride + i] >> 6) as f32 / 1023.0;
            let uv_row = (j / 2) * uv_stride;
            let uv_col = (i / 2) * 2;
            let u_raw = (nv12_10.uv[uv_row + uv_col] >> 6) as f32 / 1023.0 - 0.5;
            let v_raw = (nv12_10.uv[uv_row + uv_col + 1] >> 6) as f32 / 1023.0 - 0.5;

            // BT.2020 NCL Y'CbCr limited-range → full-range Y'CbCr.
            // (Skip limited-range expand since encoder uses full-range for Main10.)
            // BT.2020 NCL inverse matrix (Y', Cb, Cr) → (R', G', B') in [0,1].
            let r_lin = (y_raw + 1.4746 * v_raw).clamp(0.0, 1.0);
            let g_lin = (y_raw - 0.1646 * u_raw - 0.5714 * v_raw).clamp(0.0, 1.0);
            let b_lin = (y_raw + 1.8814 * u_raw).clamp(0.0, 1.0);

            // Inverse PQ EOTF (SMPTE ST 2084) → scene-linear light [0, 10000 cd/m²].
            let pq_eotf = |e: f32| -> f32 {
                const M1: f32 = 0.1593017578125;
                const M2: f32 = 78.84375;
                const C1: f32 = 0.8359375;
                const C2: f32 = 18.8515625;
                const C3: f32 = 18.6875;
                let ep = e.powf(1.0 / M2);
                let num = (ep - C1).max(0.0);
                let den = C2 - C3 * ep;
                (num / den).powf(1.0 / M1) * 10000.0
            };
            let r_scene = pq_eotf(r_lin);
            let g_scene = pq_eotf(g_lin);
            let b_scene = pq_eotf(b_lin);

            // Reinhard tone-map: L_out = L_in / (1 + L_in) with peak at 1000 cd/m².
            // Normalise to [0, 1] assuming 1000 cd/m² peak.
            let scale = 1.0 / 1000.0;
            let tone = |v: f32| -> f32 {
                let v = v * scale;
                v / (1.0 + v)
            };
            let r_tm = tone(r_scene);
            let g_tm = tone(g_scene);
            let b_tm = tone(b_scene);

            // BT.709 gamma (approximate sRGB): linear → gamma-encoded.
            let gamma = |v: f32| -> u8 {
                let v = v.clamp(0.0, 1.0);
                let enc = if v <= 0.0031308 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                (enc * 255.0 + 0.5) as u8
            };

            let off = (j * w + i) * 4;
            out_bgra[off] = gamma(b_tm);
            out_bgra[off + 1] = gamma(g_tm);
            out_bgra[off + 2] = gamma(r_tm);
            out_bgra[off + 3] = 0xFF;
        }
    }
    Ok(())
}

/// CPU alpha-blend a BGRA source rectangle onto a BGRA destination
/// framebuffer. Source pixels' alpha channel modulates the contribution.
/// Clips source to the destination bounds.
#[allow(clippy::too_many_arguments)]
fn alpha_blend_bgra(
    dst: &mut [u8],
    dst_w: i32,
    dst_h: i32,
    src_w: i32,
    src_h: i32,
    dst_x: i32,
    dst_y: i32,
    src: &[u8],
) {
    debug_assert_eq!(
        src.len(),
        (src_w as usize)
            .saturating_mul(src_h as usize)
            .saturating_mul(4),
        "alpha_blend_bgra: src buffer size mismatch"
    );
    debug_assert_eq!(
        dst.len(),
        (dst_w as usize)
            .saturating_mul(dst_h as usize)
            .saturating_mul(4),
        "alpha_blend_bgra: dst buffer size mismatch"
    );
    let x0 = dst_x.max(0);
    let y0 = dst_y.max(0);
    let x1 = (dst_x + src_w).min(dst_w);
    let y1 = (dst_y + src_h).min(dst_h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let src_offset_x = (x0 - dst_x) as usize;
    let src_offset_y = (y0 - dst_y) as usize;

    for y in y0..y1 {
        let row_dst = ((y * dst_w + x0) * 4) as usize;
        let row_src = ((src_offset_y + (y - y0) as usize) * src_w as usize + src_offset_x) * 4;
        for x in 0..((x1 - x0) as usize) {
            let s = &src[row_src + x * 4..row_src + x * 4 + 4];
            let d = &mut dst[row_dst + x * 4..row_dst + x * 4 + 4];
            let alpha = s[3] as u32;
            if alpha == 0 {
                continue;
            }
            // Standard over-operator: dst = src*alpha + dst*(1-alpha).
            let inv = 255 - alpha;
            d[0] = ((s[0] as u32 * alpha + d[0] as u32 * inv) / 255) as u8;
            d[1] = ((s[1] as u32 * alpha + d[1] as u32 * inv) / 255) as u8;
            d[2] = ((s[2] as u32 * alpha + d[2] as u32 * inv) / 255) as u8;
            d[3] = 255;
        }
    }
}

/// Resize the renderer. The softbuffer backend reconciles its surface to the
/// window's inner size inside `present_frame` (see `resize_surface_to_window`),
/// so explicit window-resize events are no-ops there (kept for API symmetry
/// with Windows). The wgpu backend reconfigures its surface to the new size.
pub fn resize_renderer(
    r: &mut PlatformRender,
    width: u32,
    height: u32,
) -> Result<(), super::RenderError> {
    match r {
        PlatformRender::Softbuffer(_) => {}
        PlatformRender::Wgpu(w) => w.resize(width, height),
    }
    Ok(())
}

/// Read the user's primary X11 _CLIPBOARD selection.
pub fn read_clipboard_text() -> Result<String, super::ClipboardError> {
    _input_linux_read_clipboard_text().map_err(|e| {
        use prdt_input_linux::error::LinuxInputError;
        match e {
            LinuxInputError::ClipboardTimeout | LinuxInputError::ClipboardNonUtf8 => {
                super::ClipboardError::NoText
            }
            LinuxInputError::ClipboardTooLarge(n) => super::ClipboardError::TooLarge(n),
            other => super::ClipboardError::Backend(other.to_string()),
        }
    })
}

/// Set the user's primary X11 _CLIPBOARD selection.
pub fn write_clipboard_text(text: &str) -> Result<(), super::ClipboardError> {
    _input_linux_write_clipboard_text(text).map_err(|e| {
        use prdt_input_linux::error::LinuxInputError;
        match e {
            LinuxInputError::ClipboardTooLarge(n) => super::ClipboardError::TooLarge(n),
            other => super::ClipboardError::Backend(other.to_string()),
        }
    })
}

/// Bumps each time an external X11 client takes the _CLIPBOARD selection.
pub fn clipboard_sequence_number() -> u32 {
    _input_linux_clipboard_sequence_number()
}

/// Return the host's virtual desktop rect via XRandR.
#[allow(dead_code)] // exposed via `platform::virtual_desktop_rect`; lib.rs uses it on Windows only
pub fn virtual_desktop_rect() -> MonitorRect {
    _input_linux_virtual_desktop_rect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_err(
        r: Result<PlatformConsumer, super::super::ConsumerError>,
    ) -> super::super::ConsumerError {
        // Manual destructure: PlatformConsumer doesn't derive Debug because
        // Openh264Decoder doesn't, and we'd rather not bolt it onto a foreign
        // type just to satisfy `unwrap_err()`'s `T: Debug` bound.
        match r {
            Ok(_) => panic!("expected build_consumer to fail"),
            Err(e) => e,
        }
    }

    #[test]
    fn alpha_blend_bgra_red_over_black() {
        let mut dst = vec![0u8; 4 * 4]; // 2x2 black BGRA
        let src = vec![0x00, 0x00, 0xff, 0xff]; // 1x1 red opaque (BGRA: B=0,G=0,R=255,A=255)
        alpha_blend_bgra(&mut dst, 2, 2, 1, 1, 0, 0, &src);
        // Top-left pixel should be red, rest black.
        assert_eq!(dst[0..4], [0x00, 0x00, 0xff, 0xff]);
        assert_eq!(dst[4..8], [0, 0, 0, 0]);
        assert_eq!(dst[8..12], [0, 0, 0, 0]);
        assert_eq!(dst[12..16], [0, 0, 0, 0]);
    }

    #[test]
    fn alpha_blend_bgra_clips_negative_offset() {
        let mut dst = vec![0u8; 4 * 4]; // 2x2 black
        let src = vec![0x00, 0x00, 0xff, 0xff, 0x00, 0xff, 0x00, 0xff]; // 2x1: red+green
                                                                        // Place at (-1, 0): only x=1 of source draws at dst x=0.
        alpha_blend_bgra(&mut dst, 2, 2, 2, 1, -1, 0, &src);
        assert_eq!(dst[0..4], [0x00, 0xff, 0x00, 0xff], "green at (0,0)");
        assert_eq!(dst[4..8], [0, 0, 0, 0], "(1,0) unchanged");
    }

    /// Pre-P2 the viewer rejected every H.265 stream because no HEVC
    /// decoder was wired in. When any of the P2 ffmpeg-decode-hevc-*
    /// features are compiled in, that hard reject is lifted, so the
    /// "rejects H.265" assertion only holds in builds with zero HEVC
    /// backends. A12.a regression-guard: the OpenH264 H.264 arm is
    /// untouched either way.
    #[cfg(not(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    )))]
    #[test]
    fn linux_build_consumer_rejects_h265() {
        let err = expect_err(build_consumer("auto", Codec::H265, 1920, 1080));
        assert!(
            err.to_string().contains("unsupported decoder/codec"),
            "unexpected error string: {err}"
        );
    }

    #[test]
    fn linux_build_consumer_rejects_hw_decoder_args() {
        let err = expect_err(build_consumer("nvdec", Codec::H264, 1920, 1080));
        assert!(err
            .to_string()
            .contains("unsupported decoder/codec on Linux"));
        let err = expect_err(build_consumer("mf", Codec::H264, 1920, 1080));
        assert!(err
            .to_string()
            .contains("unsupported decoder/codec on Linux"));
    }

    /// A12.a regression-guard: the OpenH264 H.264 arm of `build_consumer`
    /// must still return `PlatformConsumer::Openh264` with `needs_idr =
    /// true` when an explicit `--decoder openh264` is requested,
    /// regardless of whether any P2 HEVC features are compiled in.
    #[test]
    fn linux_build_consumer_accepts_openh264_h264() {
        let c = build_consumer("openh264", Codec::H264, 1920, 1080).expect("should accept");
        match c {
            PlatformConsumer::Openh264 { needs_idr, .. } => {
                assert!(needs_idr, "fresh consumer should request IDR");
            }
            #[allow(unreachable_patterns)]
            _ => panic!("expected Openh264 variant; P2 HEVC dispatch must not steal H264"),
        }
    }

    /// A12.a regression-guard: the `("auto", Codec::H264)` row must keep
    /// dispatching to OpenH264 even with all three P2 HEVC features
    /// compiled in (the H265 `auto` branch must not steal the H264 row).
    #[test]
    fn linux_build_consumer_auto_picks_openh264() {
        let c = build_consumer("auto", Codec::H264, 1920, 1080).expect("should accept");
        match c {
            PlatformConsumer::Openh264 { .. } => {}
            #[allow(unreachable_patterns)]
            _ => panic!("expected Openh264 variant; auto/H264 must not steal into HEVC dispatch"),
        }
    }

    /// A12.b — H.264 round-trip regression guard.
    ///
    /// Mirrors `openh264_decoder_accepts_self_encoded_stream` at
    /// `crates/media-sw/src/decoder.rs:95`. Exercises the rewritten
    /// `PlatformConsumer::Openh264` arm (the match arm that the P2
    /// destructure surgery moved into a `match &mut *c` in
    /// `crates/viewer/src/lib.rs:2137`): encode a small I420 frame →
    /// feed NAL units through the same `decoder.decode(&nal_units)` path
    /// → assert `latest` becomes `Some(Arc<I420Frame>)` with correct
    /// plane dimensions. No winit/softbuffer surface is needed; this is
    /// purely a decoder-arm unit test.
    #[test]
    fn a12b_openh264_round_trip_through_platform_consumer() {
        use prdt_media_sw::traits::SwH264Decoder as _;
        use prdt_media_sw::traits::SwH264Encoder as _;
        use prdt_media_sw::{I420Frame, Openh264Encoder, Openh264EncoderConfig};

        let w = 320u32;
        let h = 240u32;

        // Build the consumer the same way build_consumer() does for openh264/H264.
        let mut c = build_consumer("openh264", Codec::H264, w, h).expect("build_consumer failed");

        // Sanity: fresh consumer has needs_idr=true and latest=None.
        match c {
            PlatformConsumer::Openh264 {
                needs_idr,
                ref latest,
                ..
            } => {
                assert!(needs_idr, "fresh consumer must start with needs_idr=true");
                assert!(
                    latest.is_none(),
                    "fresh consumer must start with latest=None"
                );
            }
            #[allow(unreachable_patterns)]
            _ => panic!("expected Openh264 variant"),
        }

        // Encode a minimal I420 frame to obtain NAL units.
        let cfg = Openh264EncoderConfig {
            width: w,
            height: h,
            target_bitrate_bps: 500_000,
            max_fps: 30.0,
        };
        let mut enc = Openh264Encoder::new(cfg).expect("encoder init");
        let frame = {
            let mut f = I420Frame::new_packed(w, h).expect("I420Frame alloc");
            let stride_y = f.stride_y as usize;
            for row in 0..(h as usize) {
                for col in 0..(w as usize) {
                    f.y[row * stride_y + col] = ((col + row) & 0xFF) as u8;
                }
            }
            for b in f.u.iter_mut() {
                *b = 128;
            }
            for b in f.v.iter_mut() {
                *b = 128;
            }
            f
        };

        // Feed up to 3 IDR frames through the Openh264 arm of the match,
        // exactly mirroring what recv_task's match arm does.
        let (decoder, latest, needs_idr) = match c {
            PlatformConsumer::Openh264 {
                ref mut decoder,
                ref mut latest,
                ref mut needs_idr,
            } => (decoder, latest, needs_idr),
            #[allow(unreachable_patterns)]
            _ => panic!("expected Openh264 variant"),
        };

        let mut got_frame = false;
        for i in 0..3u64 {
            let ef = enc.encode(&frame, i == 0, i * 33_000).expect("encode");
            // This is exactly the match arm body from recv_task (lib.rs:2143–2162).
            match decoder.decode(&ef.nal_units) {
                Ok(Some(i420)) => {
                    let arc = std::sync::Arc::new(i420);
                    *latest = Some(std::sync::Arc::clone(&arc));
                    *needs_idr = false;
                    got_frame = true;
                    break;
                }
                Ok(None) => {}
                Err(e) => panic!("openh264 decode failed: {e}"),
            }
        }

        assert!(got_frame, "decoder produced no frame after 3 inputs");
        let decoded = latest.as_ref().expect("latest must be Some after decode");
        assert_eq!(decoded.width, w);
        assert_eq!(decoded.height, h);
        assert_eq!(
            decoded.y.len(),
            (decoded.stride_y as usize) * (h as usize),
            "Y plane size mismatch"
        );
        assert_eq!(
            decoded.u.len(),
            (decoded.stride_uv as usize) * (h as usize / 2),
            "U plane size mismatch"
        );
        assert_eq!(
            decoded.v.len(),
            (decoded.stride_uv as usize) * (h as usize / 2),
            "V plane size mismatch"
        );
        assert!(
            !*needs_idr,
            "needs_idr must be cleared after successful decode"
        );
    }

    // ---- P0 GUI-modernization baseline freeze ----------------------------
    // Golden digests of the CPU NV12/P010 → BGRA converters for deterministic
    // gradient inputs. P3 replaces these CPU loops with a wgpu fragment shader;
    // the shader output must reproduce these references within tolerance. If
    // you intentionally change the conversion math, recompute the constant
    // from the failure message. See .omc/plans/gui-modernization-design.md §8.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any",
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    #[test]
    fn nv12_to_bgra_gradient_golden_digest() {
        let (w, h) = (64usize, 64usize);
        let mut y = vec![0u8; w * h];
        for j in 0..h {
            for i in 0..w {
                y[j * w + i] = ((i.wrapping_mul(5)).wrapping_add(j.wrapping_mul(3))) as u8;
            }
        }
        // Interleaved UV at half resolution: stride_uv counts bytes (= w).
        let mut uv = vec![0u8; w * (h / 2)];
        for j in 0..(h / 2) {
            for i in 0..(w / 2) {
                uv[j * w + i * 2] = (i.wrapping_mul(7)) as u8; // U
                uv[j * w + i * 2 + 1] = (j.wrapping_mul(11)) as u8; // V
            }
        }
        let frame = Nv12Frame {
            width: w as u32,
            height: h as u32,
            y,
            uv,
            stride_y: w as u32,
            stride_uv: w as u32,
            pts_us: 0,
        };
        let mut out = vec![0u8; w * h * 4];
        nv12_to_bgra(&frame, &mut out).expect("consistent frame converts");
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0xe113_1b22_fd54_6e98;
        assert_eq!(
            digest, GOLDEN,
            "nv12_to_bgra gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
        );
    }

    /// Reproduces the production crash shape: a frame header claiming
    /// 3840x2160 whose UV plane holds only 1920x1080-worth of chroma. The
    /// converter must return Err (and let the caller skip the frame), not
    /// index past `uv` and abort the viewer.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    #[test]
    fn nv12_to_bgra_rejects_short_uv_plane() {
        let (w, h) = (3840u32, 2160u32);
        let y = vec![16u8; (w * h) as usize];
        // UV sized for 1920x1080 (interleaved: 1920 bytes/row * 540 rows).
        let uv = vec![128u8; (1920 * 540) as usize];
        let frame = Nv12Frame {
            width: w,
            height: h,
            y,
            uv,
            stride_y: w,
            stride_uv: w,
            pts_us: 0,
        };
        let mut out = vec![0u8; (w * h * 4) as usize];
        let err = nv12_to_bgra(&frame, &mut out).expect_err("short UV plane must be rejected");
        assert!(
            err.contains("UV plane"),
            "error should name the short UV plane, got: {err}"
        );
    }

    /// A small, fully consistent frame (with stride padding beyond width, and
    /// an ODD height that exercises the ceil-chroma-row bound) converts Ok and
    /// writes every pixel of the output buffer.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    #[test]
    fn nv12_to_bgra_ok_roundtrip() {
        let (w, h) = (64u32, 36u32);
        let stride_y = 80u32; // padded beyond width
        let stride_uv = 80u32; // interleaved UV byte stride, padded
        let y = vec![120u8; (stride_y * h) as usize];
        // ceil(h/2) chroma rows so an odd height would also be covered.
        let uv = vec![128u8; (stride_uv * h.div_ceil(2)) as usize];
        let frame = Nv12Frame {
            width: w,
            height: h,
            y,
            uv,
            stride_y,
            stride_uv,
            pts_us: 0,
        };
        let mut out = vec![7u8; (w * h * 4) as usize];
        nv12_to_bgra(&frame, &mut out).expect("consistent padded frame converts");
        assert!(
            out.chunks_exact(4).all(|px| px[3] == 0xFF),
            "converter must write every output pixel"
        );
    }

    /// A padded-stride NV12 frame (linesize > width, as HW downloads deliver)
    /// must convert to the exact same BGRA as the equivalent tight-stride
    /// frame — proving the converter indexes by stride, not width, so padding
    /// never shears or bleeds into the image.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-any",
        feature = "ffmpeg-decode-hevc-vaapi-any",
        feature = "ffmpeg-decode-hevc-nvdec-any"
    ))]
    #[test]
    fn nv12_to_bgra_padded_stride_matches_tight() {
        let (w, h) = (16usize, 8usize);
        // Tight planes.
        let mut y_t = vec![0u8; w * h];
        let mut uv_t = vec![0u8; w * (h / 2)];
        for j in 0..h {
            for i in 0..w {
                y_t[j * w + i] = ((i * 7 + j * 3) & 0xff) as u8;
            }
        }
        for j in 0..(h / 2) {
            for i in 0..(w / 2) {
                uv_t[j * w + i * 2] = ((i * 11 + j * 5) & 0xff) as u8; // U
                uv_t[j * w + i * 2 + 1] = ((i * 13 + j * 17) & 0xff) as u8; // V
            }
        }
        // Padded planes carrying the same logical rows (stride = w + 8).
        let (sy, su) = (w + 8, w + 8);
        let mut y_p = vec![0xAAu8; sy * h];
        let mut uv_p = vec![0x55u8; su * (h / 2)];
        for j in 0..h {
            y_p[j * sy..j * sy + w].copy_from_slice(&y_t[j * w..j * w + w]);
        }
        for j in 0..(h / 2) {
            uv_p[j * su..j * su + w].copy_from_slice(&uv_t[j * w..j * w + w]);
        }
        let tight = Nv12Frame {
            width: w as u32,
            height: h as u32,
            y: y_t,
            uv: uv_t,
            stride_y: w as u32,
            stride_uv: w as u32,
            pts_us: 0,
        };
        let padded = Nv12Frame {
            width: w as u32,
            height: h as u32,
            y: y_p,
            uv: uv_p,
            stride_y: sy as u32,
            stride_uv: su as u32,
            pts_us: 0,
        };
        let mut out_t = vec![0u8; w * h * 4];
        let mut out_p = vec![0u8; w * h * 4];
        nv12_to_bgra(&tight, &mut out_t).expect("tight converts");
        nv12_to_bgra(&padded, &mut out_p).expect("padded converts");
        assert_eq!(
            out_t, out_p,
            "padded-stride NV12 must convert identically to tight-stride"
        );
    }

    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    #[test]
    fn p010_to_bgra_sdr_tonemap_gradient_golden_digest() {
        let (w, h) = (64usize, 64usize);
        // P010LE: valid 10 bits in the HIGH part of each u16 (<< 6).
        let mut y = vec![0u16; w * h];
        for j in 0..h {
            for i in 0..w {
                let v10 = ((i.wrapping_mul(13)).wrapping_add(j.wrapping_mul(7)) & 0x3ff) as u16;
                y[j * w + i] = v10 << 6;
            }
        }
        let mut uv = vec![0u16; w * (h / 2)];
        for j in 0..(h / 2) {
            for i in 0..(w / 2) {
                let u10 = ((i.wrapping_mul(17)) & 0x3ff) as u16;
                let v10 = ((j.wrapping_mul(19)) & 0x3ff) as u16;
                uv[j * w + i * 2] = u10 << 6;
                uv[j * w + i * 2 + 1] = v10 << 6;
            }
        }
        let frame = Nv12Frame16 {
            width: w as u32,
            height: h as u32,
            y,
            uv,
            stride_y: w as u32,
            stride_uv: w as u32,
            pts_us: 0,
            hdr10: None,
        };
        let mut out = vec![0u8; w * h * 4];
        p010_to_bgra_sdr_tonemap(&frame, &mut out).expect("consistent frame converts");
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0x2706_6b09_316e_181e;
        assert_eq!(
            digest, GOLDEN,
            "p010_to_bgra_sdr_tonemap gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
        );
    }

    /// P010 sibling of `nv12_to_bgra_rejects_short_uv_plane`: a header claiming
    /// 3840x2160 with a UV plane holding only 1920x1080-worth of u16 chroma
    /// must return Err rather than index past the plane.
    #[cfg(any(
        feature = "ffmpeg-decode-hevc-sw-main10-any",
        feature = "ffmpeg-decode-hevc-vaapi-main10-any",
        feature = "ffmpeg-decode-hevc-nvdec-main10-any"
    ))]
    #[test]
    fn p010_to_bgra_sdr_tonemap_rejects_short_uv_plane() {
        let (w, h) = (3840u32, 2160u32);
        let y = vec![0u16; (w * h) as usize];
        // UV sized for 1920x1080 (interleaved u16: 1920 elems/row * 540 rows).
        let uv = vec![0u16; (1920 * 540) as usize];
        let frame = Nv12Frame16 {
            width: w,
            height: h,
            y,
            uv,
            stride_y: w,
            stride_uv: w,
            pts_us: 0,
            hdr10: None,
        };
        let mut out = vec![0u8; (w * h * 4) as usize];
        let err = p010_to_bgra_sdr_tonemap(&frame, &mut out)
            .expect_err("short UV plane must be rejected");
        assert!(
            err.contains("UV plane"),
            "error should name the short UV plane, got: {err}"
        );
    }

    /// A frame taller than the window (the 3840x2160-into-3840x2088 case that
    /// garbled the NVDEC session) is cropped to the window's rows, each kept
    /// row copied verbatim — no shear, no overrun.
    #[test]
    fn blit_crop_letterbox_crops_taller_frame() {
        let (fw, fh) = (4usize, 4usize);
        let mut src = vec![0u8; fw * fh * 4];
        for (i, b) in src.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let (ww, wh) = (4usize, 2usize); // window shorter than the frame
        let mut dst = vec![0xEEu8; ww * wh * 4];
        blit_crop_letterbox(&src, fw, fh, &mut dst, ww);
        assert_eq!(
            dst.as_slice(),
            &src[..ww * wh * 4],
            "cropped rows must copy verbatim"
        );
    }

    /// A frame smaller than the window is placed top-left and the surplus
    /// window area is black-filled (letterbox), never left stale or wrapped.
    #[test]
    fn blit_crop_letterbox_letterboxes_smaller_frame() {
        let (fw, fh) = (2usize, 2usize);
        let src = vec![0x7Fu8; fw * fh * 4];
        let (ww, wh) = (4usize, 3usize);
        let mut dst = vec![0xEEu8; ww * wh * 4];
        blit_crop_letterbox(&src, fw, fh, &mut dst, ww);
        for y in 0..wh {
            for x in 0..ww {
                let off = (y * ww + x) * 4;
                let expect = if x < fw && y < fh { 0x7F } else { 0x00 };
                assert!(
                    dst[off..off + 4].iter().all(|&b| b == expect),
                    "pixel {x},{y} expected {expect:#x}"
                );
            }
        }
    }

    /// A destination shorter than `win_w * win_h` (a stale/oversized tracked
    /// size) must not panic: the row count is derived from `dst.len()`.
    #[test]
    fn blit_crop_letterbox_bounded_by_dst_len() {
        let (fw, fh) = (8usize, 8usize);
        let src = vec![1u8; fw * fh * 4];
        // Claim width 8 but hand over a buffer only 3 rows tall.
        let mut dst = vec![9u8; 8 * 3 * 4];
        blit_crop_letterbox(&src, fw, fh, &mut dst, 8);
        assert_eq!(
            dst.as_slice(),
            &src[..8 * 3 * 4],
            "only dst-bounded rows written, no panic"
        );
    }
}
