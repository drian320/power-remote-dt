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

/// Per-OS render state. Wraps softbuffer's Surface + a scratch BGRA
/// buffer used by `present_frame` to convert I420 → BGRA before
/// blitting into the surface's `&mut [u32]` framebuffer.
pub struct PlatformRender {
    window: Arc<Window>,
    // softbuffer 0.4 Surface is generic over (D, W) where D:
    // HasDisplayHandle and W: HasWindowHandle. Arc<Window> satisfies
    // both, so D = W = Arc<Window>.
    _ctx: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    /// I420 → BGRA conversion scratch. Re-allocated on stream-size change.
    scratch_bgra: Vec<u8>,
    /// Cached stream/surface dimensions to gate redundant resize calls.
    last_size: (u32, u32),
}

impl PlatformRender {
    /// Borrow the underlying window. Used by lib.rs to call
    /// `request_redraw`, `set_title`, `inner_size`, etc., without leaking
    /// the platform-specific render-state internals.
    pub fn window(&self) -> &Window {
        &self.window
    }
}

/// Build the Linux render state. Called by lib.rs in `resumed()`.
pub fn build_render(
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> Result<PlatformRender, super::RenderError> {
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
    Ok(PlatformRender {
        window,
        _ctx: ctx,
        surface,
        scratch_bgra: vec![0u8; (width * height * 4) as usize],
        last_size: (width, height),
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
    _decoder_label: &str,
    shared: &crate::ViewerShared,
) -> Result<(), super::RenderError> {
    match f {
        PlatformFrame::I420(i420) => {
            let stream_w = i420.width;
            let stream_h = i420.height;

            resize_surface_if_needed(r, stream_w, stream_h)?;

            // I420 → BGRA via the existing helper (BT.709 limited-range,
            // alpha 0xFF). Output layout matches softbuffer's LE u32 expectation
            // (B in lowest byte, A=0xFF in highest).
            i420_to_bgra(i420, &mut r.scratch_bgra);

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

            resize_surface_if_needed(r, stream_w, stream_h)?;

            nv12_to_bgra(nv12, &mut r.scratch_bgra);

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

            resize_surface_if_needed(r, stream_w, stream_h)?;

            p010_to_bgra_sdr_tonemap(nv12_10, &mut r.scratch_bgra);

            composite_cursor(r, shared, stream_w, stream_h);
            blit_scratch_to_surface(r)?;
        }
    }

    let _ = &r.window; // suppress unused-field warning; kept to extend Surface lifetime
    Ok(())
}

/// Resize the softbuffer surface and BGRA scratch buffer when the stream
/// dimensions change. Extracted from the pre-P2 `present_frame` body so
/// both the I420 and NV12 arms can share the path.
fn resize_surface_if_needed(
    r: &mut PlatformRender,
    stream_w: u32,
    stream_h: u32,
) -> Result<(), super::RenderError> {
    if r.last_size != (stream_w, stream_h) {
        let nz_w = NonZeroU32::new(stream_w.max(1)).expect("non-zero stream width");
        let nz_h = NonZeroU32::new(stream_h.max(1)).expect("non-zero stream height");
        r.surface
            .resize(nz_w, nz_h)
            .map_err(|e| super::RenderError::Present(format!("Surface::resize: {e}")))?;
        r.scratch_bgra.resize((stream_w * stream_h * 4) as usize, 0);
        r.last_size = (stream_w, stream_h);
    }
    Ok(())
}

/// Composite the cursor bitmap (if any) onto `r.scratch_bgra`. P5B-2b:
/// briefly take the cursor lock, copy out the values we need, then drop
/// the lock before the blend so we don't hold it across the CPU op.
fn composite_cursor(
    r: &mut PlatformRender,
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

/// Blit `r.scratch_bgra` into the softbuffer surface and present.
fn blit_scratch_to_surface(r: &mut PlatformRender) -> Result<(), super::RenderError> {
    let mut buf = r
        .surface
        .buffer_mut()
        .map_err(|e| super::RenderError::Present(format!("Surface::buffer_mut: {e}")))?;
    debug_assert_eq!(buf.len() * 4, r.scratch_bgra.len());
    let buf_bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut buf);
    buf_bytes.copy_from_slice(&r.scratch_bgra);
    buf.present()
        .map_err(|e| super::RenderError::Present(format!("Surface::present: {e}")))?;
    Ok(())
}

/// NV12 (Y plane + interleaved UV plane) → BGRA. BT.709 limited-range,
/// alpha 0xFF. Sibling of `i420_to_bgra`; same coefficients, just an
/// interleaved chroma layout (UV byte at offset 0 = U, +1 = V).
#[cfg(any(
    feature = "ffmpeg-decode-hevc-sw-any",
    feature = "ffmpeg-decode-hevc-vaapi-any",
    feature = "ffmpeg-decode-hevc-nvdec-any"
))]
fn nv12_to_bgra(nv12: &Nv12Frame, out_bgra: &mut [u8]) {
    let w = nv12.width as usize;
    let h = nv12.height as usize;
    debug_assert_eq!(out_bgra.len(), w * h * 4);
    let y_stride = nv12.stride_y as usize;
    let uv_stride = nv12.stride_uv as usize;
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
fn p010_to_bgra_sdr_tonemap(nv12_10: &Nv12Frame16, out_bgra: &mut [u8]) {
    let w = nv12_10.width as usize;
    let h = nv12_10.height as usize;
    debug_assert_eq!(out_bgra.len(), w * h * 4);
    let y_stride = nv12_10.stride_y as usize;
    let uv_stride = nv12_10.stride_uv as usize;

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

/// Resize the renderer. softbuffer auto-resizes on the next
/// `present_frame` based on stream size, so window-resize events are
/// no-ops here (kept for API symmetry with Windows).
pub fn resize_renderer(
    _r: &mut PlatformRender,
    _width: u32,
    _height: u32,
) -> Result<(), super::RenderError> {
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
        nv12_to_bgra(&frame, &mut out);
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0xe113_1b22_fd54_6e98;
        assert_eq!(
            digest, GOLDEN,
            "nv12_to_bgra gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
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
        p010_to_bgra_sdr_tonemap(&frame, &mut out);
        let digest = fnv1a64(&out);
        const GOLDEN: u64 = 0x2706_6b09_316e_181e;
        assert_eq!(
            digest, GOLDEN,
            "p010_to_bgra_sdr_tonemap gradient digest changed: got {digest:#018x} (update GOLDEN if intentional)"
        );
    }
}
