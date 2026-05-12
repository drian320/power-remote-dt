//! SPA_META_Cursor receive path for cursor_mode=Metadata.
//!
//! Bare-FFI helper that reads `SPA_META_Cursor` out of a PipeWire buffer
//! and produces a fully owned [`CursorUpdate`] so the value can cross the
//! mpsc channel into the host's session task without lifetime entanglement
//! with the PipeWire buffer pool.
//!
//! # libspa Rust wrapper status (probed 2026-05-12)
//!
//! pipewire-rs 0.9.2 exposes NO typed `Meta` wrapper for cursor metadata.
//! The canonical path is hand-rolling a walk of `spa_buffer.metas[]` for
//! `type_ == SPA_META_Cursor` (value 5), then raw pointer reads into
//! `spa_meta_cursor` / `spa_meta_bitmap`. OBS Studio uses the same approach
//! at `plugins/linux-pipewire/pipewire.c#L889`.
//!
//! `spa_buffer_find_meta` is an inline C helper that libspa-sys does NOT
//! re-export (confirmed: libspa-sys-0.9.2/src/lib.rs only re-exports
//! bindgen-generated symbols; inline C statics are wrapped via type_info.c
//! but find_meta is absent). We hand-roll the equivalent.
//!
//! # Probed constants (libspa-sys-0.9.2, bindings.rs from bookworm build)
//!
//! ```text
//! SPA_META_Cursor: spa_meta_type = 5  (type alias = c_uint = u32)
//! SPA_VIDEO_FORMAT_RGBA:  spa_video_format = 11  (type alias = c_uint = u32)
//! SPA_VIDEO_FORMAT_BGRA:  spa_video_format = 12
//! SPA_VIDEO_FORMAT_ARGB:  spa_video_format = 13
//!
//! spa_meta_cursor { id: u32, flags: u32, position: spa_point, hotspot: spa_point, bitmap_offset: u32 }
//!   size_of = 28, align = 4
//! spa_meta_bitmap { format: u32, size: spa_rectangle, stride: i32, offset: u32 }
//!   size_of = 20, align = 4
//! spa_meta  { type_: u32, size: u32, data: *mut c_void }
//! spa_buffer { n_metas: u32, n_datas: u32, metas: *mut spa_meta, datas: *mut spa_data }
//! spa_point  { x: i32, y: i32 }
//! ```
//!
//! # pipewire::buffer::Buffer raw-pointer access (deviation from plan)
//!
//! `pipewire::buffer::Buffer` (pipewire-0.9.2) holds
//! `buf: NonNull<pw_sys::pw_buffer>` but exposes NO public `as_raw_ptr()`.
//! The plan's blanket impl on `&pipewire::spa::buffer::Buffer` refers to a
//! type that does not exist in this crate version (`pipewire::spa` re-exports
//! libspa, which has no `Buffer` type). The production impl of `SpaBufferLike`
//! for `pipewire::buffer::Buffer` must be added at the call site (stream.rs /
//! capturer.rs) using `unsafe { &*(buf.datas_mut() as *const _ as *const _) }`
//! or by accessing the underlying `pw_buffer.buffer` pointer directly via a
//! locally-written unsafe helper. The trait in this module is kept so the
//! test harness and future callers have the correct interface.
//!
//! # Format normalization
//!
//! GNOME mutter + KDE kwin commonly emit cursor bitmaps in `RGBA`, not
//! `BGRA`. This helper normalizes to tightly-packed BGRA8 at the host
//! boundary so the wire encoder + viewer compositor have a single code
//! path (per Codex advisor finding 2026-05-12).

#![cfg(target_os = "linux")]

use std::os::raw::c_uint;

/// `spa_meta_cursor.id == 0` means "no new cursor metadata".
pub(crate) const SPA_META_CURSOR_ID_INVALID: u32 = 0;

/// Host-side owned snapshot of a portal cursor metadata update. Field
/// layout mirrors `ControlMessage::CursorUpdate` but lives in `media-linux`
/// so the `protocol` crate has no Linux-specific knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    pub id: u32, // never 0 in emitted values (filtered at parse time)
    pub position_x: i32,
    pub position_y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub bitmap: Option<CursorBitmap>,
}

/// Tightly packed BGRA8 cursor bitmap. `width == 0 && height == 0` means
/// "cursor invisible" — the host signals the viewer to hide compositing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorBitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>, // len == width * height * 4
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorMetaError {
    #[error("no SPA_META_Cursor on buffer")]
    Absent,
    #[error("meta size {0} smaller than spa_meta_cursor")]
    MetaTooSmall(u32),
    #[error("bitmap_offset {0} out of bounds (meta size {1})")]
    BitmapOffsetOutOfBounds(u32, u32),
    #[error("bitmap size {0}x{1} exceeds cap 256x256 — silent-truncate at caller")]
    BitmapTooLarge(u32, u32),
    #[error("unsupported bitmap format {0} (only BGRA/RGBA/ARGB)")]
    UnsupportedFormat(u32),
}

/// Trait abstraction so unit tests can inject a stub buffer without
/// constructing a real `pipewire::buffer::Buffer`. Production impl is
/// provided at the call site (stream.rs) since `pipewire::buffer::Buffer`
/// exposes no public `as_raw_ptr()` in this crate version.
/// Same pattern as `dmabuf::SpaDataLike` from P5B-2a T3.
pub trait SpaBufferLike {
    /// Returns a raw pointer to the underlying `spa_sys::spa_buffer`, or
    /// null if not yet bound. Caller dereferences inside an unsafe block.
    fn as_raw_spa_buffer(&self) -> *const pipewire::spa::sys::spa_buffer;
}

/// Parse `SPA_META_Cursor` from a PipeWire buffer, returning an owned
/// [`CursorUpdate`]. Returns `Ok(None)` when `spa_meta_cursor.id == 0`
/// (compositor signals "no new metadata" — keep cached state).
///
/// # Safety
///
/// Caller asserts `buf` references a valid `spa_buffer` whose memory is
/// live for the duration of this call (i.e. obtained inside the PipeWire
/// `process` callback and not yet released back to the pool).
pub unsafe fn read_meta_cursor<B: SpaBufferLike>(
    buf: &B,
) -> Result<Option<CursorUpdate>, CursorMetaError> {
    use pipewire::spa::sys as spa_sys;
    use std::mem::size_of;

    let spa_buf = buf.as_raw_spa_buffer();
    if spa_buf.is_null() {
        return Err(CursorMetaError::Absent);
    }

    // 1. Locate SPA_META_Cursor — walk metas[] slice. spa_buffer_find_meta
    //    is an inline C helper; libspa-sys does not re-export it.
    //    Hand-rolling the equivalent avoids the dependency.
    //
    // SAFETY: spa_buf is a valid pointer per the caller's contract; we
    // read n_metas + metas as a primitive slice header.
    let n_metas = (*spa_buf).n_metas as usize;
    let metas = (*spa_buf).metas;
    let mut meta_ptr: *const spa_sys::spa_meta = std::ptr::null();
    for i in 0..n_metas {
        // SAFETY: metas[i] is valid for i < n_metas (PipeWire contract).
        let m = metas.add(i);
        if (*m).type_ == spa_sys::SPA_META_Cursor as c_uint {
            meta_ptr = m;
            break;
        }
    }
    if meta_ptr.is_null() {
        return Err(CursorMetaError::Absent);
    }

    // 2. Bounds-check spa_meta_cursor.
    let meta_size = (*meta_ptr).size;
    let cur_sz = size_of::<spa_sys::spa_meta_cursor>() as u32;
    if meta_size < cur_sz {
        return Err(CursorMetaError::MetaTooSmall(meta_size));
    }

    let base = (*meta_ptr).data as *const u8;
    if base.is_null() {
        return Err(CursorMetaError::MetaTooSmall(meta_size));
    }
    let c = &*(base as *const spa_sys::spa_meta_cursor);

    if c.id == SPA_META_CURSOR_ID_INVALID {
        return Ok(None);
    }

    let mut out = CursorUpdate {
        id: c.id,
        position_x: c.position.x,
        position_y: c.position.y,
        hotspot_x: c.hotspot.x,
        hotspot_y: c.hotspot.y,
        bitmap: None,
    };

    if c.bitmap_offset == 0 {
        return Ok(Some(out)); // reuse cached bitmap, NOT clear
    }

    // 3. Bounds-check spa_meta_bitmap.
    let bmp_off = c.bitmap_offset;
    let bmp_sz = size_of::<spa_sys::spa_meta_bitmap>() as u32;
    if bmp_off < cur_sz || bmp_off.checked_add(bmp_sz).is_none_or(|s| s > meta_size) {
        return Err(CursorMetaError::BitmapOffsetOutOfBounds(bmp_off, meta_size));
    }

    let b = &*(base.add(bmp_off as usize) as *const spa_sys::spa_meta_bitmap);

    if b.format == 0 {
        return Ok(Some(out)); // compositor signals "ignore bitmap"
    }

    let w = b.size.width;
    let h = b.size.height;

    // Cap check — caller silent-truncates. Return error and let caller decide.
    if w > 256 || h > 256 {
        return Err(CursorMetaError::BitmapTooLarge(w, h));
    }
    if w == 0 || h == 0 {
        // Invisible cursor.
        out.bitmap = Some(CursorBitmap {
            width: 0,
            height: 0,
            bgra: Vec::new(),
        });
        return Ok(Some(out));
    }

    if b.offset == 0 {
        // No image data (cursor invisible despite valid size).
        out.bitmap = Some(CursorBitmap {
            width: 0,
            height: 0,
            bgra: Vec::new(),
        });
        return Ok(Some(out));
    }

    // 4. Pixel data extraction + format normalization.
    if b.stride <= 0 {
        return Err(CursorMetaError::UnsupportedFormat(b.format));
    }
    let stride = b.stride as u32;
    let pixel_rel_off = b.offset; // offset within spa_meta_bitmap struct
    let pixel_abs_off =
        bmp_off
            .checked_add(pixel_rel_off)
            .ok_or(CursorMetaError::BitmapOffsetOutOfBounds(
                b.offset, meta_size,
            ))?;
    let needed = stride
        .checked_mul(h)
        .ok_or(CursorMetaError::BitmapOffsetOutOfBounds(stride, meta_size))?;
    if pixel_abs_off
        .checked_add(needed)
        .is_none_or(|s| s > meta_size)
    {
        return Err(CursorMetaError::BitmapOffsetOutOfBounds(
            pixel_abs_off,
            meta_size,
        ));
    }

    let pixel_ptr = base.add(pixel_abs_off as usize);
    let mut bgra = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..(h as usize) {
        // Source row: stride bytes (may include padding past w*4).
        // SAFETY: bounds checked above.
        let src =
            std::slice::from_raw_parts(pixel_ptr.add(row * stride as usize), (w as usize) * 4);
        let dst = &mut bgra[row * (w as usize) * 4..(row + 1) * (w as usize) * 4];
        dst.copy_from_slice(src);
    }

    // Format normalization — host emits BGRA always.
    // spa_video_format is c_uint (u32); compare as u32.
    let fmt = b.format;
    if fmt == spa_sys::SPA_VIDEO_FORMAT_BGRA {
        // already BGRA — pass through
    } else if fmt == spa_sys::SPA_VIDEO_FORMAT_RGBA {
        // R<->B swap per pixel.
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    } else if fmt == spa_sys::SPA_VIDEO_FORMAT_ARGB {
        // ARGB → BGRA: byte rotation [A,R,G,B] → [B,G,R,A].
        for px in bgra.chunks_exact_mut(4) {
            let a = px[0];
            let r = px[1];
            let g = px[2];
            let bl = px[3];
            px[0] = bl;
            px[1] = g;
            px[2] = r;
            px[3] = a;
        }
    } else {
        return Err(CursorMetaError::UnsupportedFormat(fmt));
    }

    out.bitmap = Some(CursorBitmap {
        width: w,
        height: h,
        bgra,
    });
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::sys as spa_sys;
    use std::mem::size_of;

    /// In-test stand-in for `spa_buffer` exposing a hand-built meta block.
    struct TestBuf {
        meta_buf: Vec<u8>,           // backing storage; lives across the call
        spa_meta: spa_sys::spa_meta, // points into meta_buf
        spa_buffer: spa_sys::spa_buffer,
    }

    impl TestBuf {
        /// Build a buffer with a single SPA_META_Cursor entry containing the
        /// raw bytes the caller provides. The caller is responsible for
        /// laying out spa_meta_cursor + (optionally) spa_meta_bitmap +
        /// pixel data inside `cursor_meta_payload`.
        fn with_cursor_meta(cursor_meta_payload: Vec<u8>) -> Self {
            // Layout: meta_buf is the raw payload bytes. spa_meta.data points
            // to its start; spa_meta.type_ = SPA_META_Cursor; spa_meta.size =
            // payload length.
            let mut me = TestBuf {
                meta_buf: cursor_meta_payload,
                spa_meta: unsafe { std::mem::zeroed() },
                spa_buffer: unsafe { std::mem::zeroed() },
            };
            me.spa_meta.type_ = spa_sys::SPA_META_Cursor as c_uint;
            me.spa_meta.size = me.meta_buf.len() as u32;
            me.spa_meta.data = me.meta_buf.as_mut_ptr() as *mut _;
            // spa_buffer.metas points to &me.spa_meta; n_metas = 1.
            me.spa_buffer.n_metas = 1;
            me.spa_buffer.metas = &me.spa_meta as *const _ as *mut _;
            me
        }
    }

    impl SpaBufferLike for &TestBuf {
        fn as_raw_spa_buffer(&self) -> *const spa_sys::spa_buffer {
            &self.spa_buffer as *const _
        }
    }

    /// Bitmap argument to `build_cursor_payload`: (format, (width, height), stride, pixels).
    type BitmapArg = Option<(u32, (u32, u32), i32, Vec<u8>)>;

    /// Helper: layout a spa_meta_cursor into a Vec, optionally followed by
    /// a spa_meta_bitmap + pixel bytes.
    fn build_cursor_payload(
        id: u32,
        pos: (i32, i32),
        hotspot: (i32, i32),
        bitmap: BitmapArg,
    ) -> Vec<u8> {
        let cur_sz = size_of::<spa_sys::spa_meta_cursor>();
        let bmp_sz = size_of::<spa_sys::spa_meta_bitmap>();

        // spa_meta_cursor at offset 0
        let mut payload = vec![0u8; cur_sz];
        unsafe {
            let c = payload.as_mut_ptr() as *mut spa_sys::spa_meta_cursor;
            (*c).id = id;
            (*c).flags = 0;
            (*c).position.x = pos.0;
            (*c).position.y = pos.1;
            (*c).hotspot.x = hotspot.0;
            (*c).hotspot.y = hotspot.1;
            (*c).bitmap_offset = if bitmap.is_some() { cur_sz as u32 } else { 0 };
        }
        if let Some((fmt, (w, h), stride, pixels)) = bitmap {
            // spa_meta_bitmap immediately after spa_meta_cursor
            let bmp_start = payload.len();
            payload.resize(bmp_start + bmp_sz, 0);
            // Pixel data at offset (bmp_start + bmp_sz) - bmp_start = bmp_sz
            unsafe {
                let b = payload.as_mut_ptr().add(bmp_start) as *mut spa_sys::spa_meta_bitmap;
                (*b).format = fmt;
                (*b).size.width = w;
                (*b).size.height = h;
                (*b).stride = stride;
                (*b).offset = if pixels.is_empty() { 0 } else { bmp_sz as u32 };
            }
            payload.extend_from_slice(&pixels);
        }
        payload
    }

    #[test]
    fn read_meta_cursor_returns_none_for_id_zero() {
        let payload = build_cursor_payload(0, (10, 20), (0, 0), None);
        let buf = TestBuf::with_cursor_meta(payload);
        let r = unsafe { read_meta_cursor(&&buf) }.expect("parse ok");
        assert_eq!(r, None, "id=0 must yield Ok(None)");
    }

    #[test]
    fn read_meta_cursor_handles_position_only_update() {
        // id != 0, bitmap_offset == 0 → position-only update.
        let payload = build_cursor_payload(42, (100, 200), (3, 5), None);
        let buf = TestBuf::with_cursor_meta(payload);
        let r = unsafe { read_meta_cursor(&&buf) }
            .expect("parse ok")
            .expect("Some update");
        assert_eq!(r.id, 42);
        assert_eq!((r.position_x, r.position_y), (100, 200));
        assert_eq!((r.hotspot_x, r.hotspot_y), (3, 5));
        assert!(r.bitmap.is_none(), "position-only must have None bitmap");
    }

    #[test]
    fn read_meta_cursor_handles_bgra_bitmap() {
        // 2x1 BGRA cursor: pixel0=red(0,0,255,255), pixel1=green(0,255,0,255).
        let pixels = vec![
            0x00, 0x00, 0xff, 0xff, // BGRA = blue=0, green=0, red=255, alpha=255 → red
            0x00, 0xff, 0x00, 0xff, // green
        ];
        let payload = build_cursor_payload(
            7,
            (50, 50),
            (1, 1),
            Some((spa_sys::SPA_VIDEO_FORMAT_BGRA, (2, 1), 8, pixels.clone())),
        );
        let buf = TestBuf::with_cursor_meta(payload);
        let r = unsafe { read_meta_cursor(&&buf) }
            .expect("parse ok")
            .expect("Some update");
        let bmp = r.bitmap.expect("Some bitmap");
        assert_eq!((bmp.width, bmp.height), (2, 1));
        assert_eq!(bmp.bgra, pixels, "BGRA must pass through unchanged");
    }

    #[test]
    fn read_meta_cursor_normalizes_rgba_to_bgra() {
        // RGBA pixel = (R, G, B, A); want BGRA = (B, G, R, A).
        let rgba = vec![0xff, 0x00, 0x00, 0xff]; // R=255, G=0, B=0, A=255 = red
        let payload = build_cursor_payload(
            8,
            (0, 0),
            (0, 0),
            Some((spa_sys::SPA_VIDEO_FORMAT_RGBA, (1, 1), 4, rgba)),
        );
        let buf = TestBuf::with_cursor_meta(payload);
        let r = unsafe { read_meta_cursor(&&buf) }
            .expect("parse ok")
            .expect("Some update");
        let bmp = r.bitmap.expect("Some bitmap");
        // Red as RGBA(255,0,0,255) → BGRA(0,0,255,255).
        assert_eq!(bmp.bgra, vec![0x00, 0x00, 0xff, 0xff]);
    }
}
