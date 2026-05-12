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
use prdt_media_sw::{I420Frame, Openh264Decoder};
use prdt_protocol::{frame::Codec, MonitorRect};
use winit::window::Window;

/// Re-exported max clipboard bytes; identical value across OSes.
pub const MAX_CLIPBOARD_BYTES: usize = _INPUT_LINUX_MAX;

/// Per-OS decoded frame. Linux only has the I420 (CPU) path for L1.5b.
pub enum PlatformFrame {
    I420(Arc<I420Frame>),
}

/// Per-OS decoder/consumer.
pub enum PlatformConsumer {
    Openh264 {
        decoder: Openh264Decoder,
        latest: Option<Arc<I420Frame>>,
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

/// Build the consumer for the negotiated codec. Linux only supports
/// openh264 (CPU H.264). HW backends and H.265 are rejected (caller
/// upstream — `choose_decoder` in lib.rs — should already prevent these,
/// but defense-in-depth is cheap here).
pub fn build_consumer(
    decoder_arg: &str,
    codec: Codec,
    _width: u32,
    _height: u32,
) -> Result<PlatformConsumer, super::ConsumerError> {
    match (decoder_arg, codec) {
        ("openh264" | "auto", Codec::H264) => {
            let dec = Openh264Decoder::new()
                .map_err(|e| super::ConsumerError::Init(format!("Openh264Decoder::new: {e}")))?;
            Ok(PlatformConsumer::Openh264 {
                decoder: dec,
                latest: None,
                needs_idr: true,
            })
        }
        (other_decoder, other_codec) => Err(super::ConsumerError::Init(format!(
            "unsupported decoder/codec on Linux: decoder={other_decoder}, codec={other_codec:?} (Linux supports openh264+H264 only)"
        ))),
    }
}

/// Present one decoded frame on the existing render state. Lazily
/// resizes the softbuffer surface to match the stream size on first
/// frame or stream-size change.
pub fn present_frame(
    r: &mut PlatformRender,
    f: &PlatformFrame,
    _decoder_label: &str,
    shared: &crate::ViewerShared,
) -> Result<(), super::RenderError> {
    let PlatformFrame::I420(i420) = f;
    let stream_w = i420.width;
    let stream_h = i420.height;

    if r.last_size != (stream_w, stream_h) {
        let nz_w = NonZeroU32::new(stream_w.max(1)).expect("non-zero stream width");
        let nz_h = NonZeroU32::new(stream_h.max(1)).expect("non-zero stream height");
        r.surface
            .resize(nz_w, nz_h)
            .map_err(|e| super::RenderError::Present(format!("Surface::resize: {e}")))?;
        r.scratch_bgra.resize((stream_w * stream_h * 4) as usize, 0);
        r.last_size = (stream_w, stream_h);
    }

    // I420 → BGRA via the existing helper (BT.709 limited-range,
    // alpha 0xFF). Output layout matches softbuffer's LE u32 expectation
    // (B in lowest byte, A=0xFF in highest).
    i420_to_bgra(i420, &mut r.scratch_bgra);

    // P5B-2b: cursor composite (Linux softbuffer).
    // Lock briefly, copy out the values we need, then drop the lock before
    // the blend to avoid holding it across the CPU operation.
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

    let mut buf = r
        .surface
        .buffer_mut()
        .map_err(|e| super::RenderError::Present(format!("Surface::buffer_mut: {e}")))?;
    debug_assert_eq!(buf.len() * 4, r.scratch_bgra.len());
    let buf_bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut buf);
    buf_bytes.copy_from_slice(&r.scratch_bgra);
    buf.present()
        .map_err(|e| super::RenderError::Present(format!("Surface::present: {e}")))?;

    let _ = &r.window; // suppress unused-field warning; kept to extend Surface lifetime
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

    #[test]
    fn linux_build_consumer_rejects_h265() {
        let err = expect_err(build_consumer("auto", Codec::H265, 1920, 1080));
        assert!(
            err.to_string()
                .contains("Linux supports openh264+H264 only"),
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

    #[test]
    fn linux_build_consumer_accepts_openh264_h264() {
        let c = build_consumer("openh264", Codec::H264, 1920, 1080).expect("should accept");
        match c {
            PlatformConsumer::Openh264 { needs_idr, .. } => {
                assert!(needs_idr, "fresh consumer should request IDR");
            }
        }
    }

    #[test]
    fn linux_build_consumer_auto_picks_openh264() {
        let c = build_consumer("auto", Codec::H264, 1920, 1080).expect("should accept");
        match c {
            PlatformConsumer::Openh264 { .. } => {}
        }
    }
}
