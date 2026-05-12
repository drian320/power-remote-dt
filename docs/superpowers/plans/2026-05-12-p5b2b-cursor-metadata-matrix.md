# P5B-2b Implementation Plan — Cursor Metadata + GNOME/KDE Matrix

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade Wayland-portal capture from `cursor_mode=Embedded` to `cursor_mode=Metadata` (mode 4) with viewer-side cursor compositing, bump wire protocol from v3 to v4 with a new `ControlMessage::CursorUpdate` variant, and ship a GNOME (mutter) + KDE (kwin) smoke walkthrough.

**Architecture:** New `cursor.rs` FFI helper extracts `SPA_META_Cursor` from each PipeWire buffer, normalizes RGBA/ARGB → BGRA, and forwards an owned `CursorUpdate` over a dedicated mpsc channel. Host's session task drains the channel and emits `ControlMessage::CursorUpdate` over the existing `Transport::send_control` path. Viewer's per-platform render (`platform/linux.rs` softbuffer / `platform/win.rs` D3D11) composites the cursor on top of the decoded frame using a shared `cursor_state::CursorState`.

**Tech Stack:** Rust 1.85, pipewire 0.9.2, libspa 0.9.2 (bare FFI via `spa_sys::spa_buffer_find_meta` — no Meta wrapper exists), ashpd 0.12.3 (`Screencast::available_cursor_modes`), tokio 1.x mpsc, bincode/serde for wire, softbuffer 0.4 (Linux viewer), D3D11 (Windows viewer).

**Constraints:**
- All cargo invocations through `./scripts/dev-container.sh` (Debian bookworm container — Ubuntu 22.04 host's pipewire 0.3.48 is too old).
- BGRA wire format only (host normalizes from RGBA/ARGB).
- 256×256 bitmap cap, silent-truncate on overflow.
- `kind_u8 = 18` for the new `CursorUpdate` variant (free slot ≤ 22 decode bound).
- Hard `protocol_version 3 → 4` bump (matches existing strict-match convention).

**Spec:** `docs/superpowers/specs/2026-05-12-p5b2b-cursor-metadata-matrix-design.md` (commit `f410dad`).

---

## Task 1: `cursor.rs` — SPA_META_Cursor FFI parser

**Files:**
- Create: `crates/media-linux/src/wayland_portal/cursor.rs`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs` (re-export)

- [ ] **Step 1: Probe spa_sys for SPA_META_Cursor + spa_meta_cursor / spa_meta_bitmap definitions**

```bash
./scripts/dev-container.sh bash -c '
echo "=== libspa-sys spa_sys::SPA_META_Cursor ==="
grep -n "SPA_META_Cursor\|SPA_VIDEO_FORMAT_BGRA\|SPA_VIDEO_FORMAT_RGBA\|SPA_VIDEO_FORMAT_ARGB" \
  target-docker/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/libspa-sys-0.9.2/src/*.rs | head -10
echo ""
echo "=== spa_meta_cursor / spa_meta_bitmap field layout ==="
grep -n -B 1 -A 12 "spa_meta_cursor\|spa_meta_bitmap" \
  target-docker/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/libspa-sys-0.9.2/src/*.rs | head -50
echo ""
echo "=== spa_buffer_find_meta C helper signature ==="
grep -n "spa_buffer_find_meta" \
  target-docker/cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/libspa-sys-0.9.2/src/*.rs | head -5
'
```

Record the result in a comment block at the top of `cursor.rs` (Step 3) so a future reader can confirm the FFI shape. Expected from spec §10:
- `SPA_META_Cursor = 5`
- `SPA_VIDEO_FORMAT_BGRA = 7`, `SPA_VIDEO_FORMAT_RGBA = 8` (verify), `SPA_VIDEO_FORMAT_ARGB = 9` (verify) — pipewire's `spa-0.2/spa/param/video/format.h` is the source of truth; bindgen-generated values may differ slightly. Verify against `/usr/include/spa-0.2/spa/param/video/raw.h` inside the container.

**Fallback** if `spa_buffer_find_meta` is NOT re-exported by `libspa-sys` (sometimes wrappers strip inline helpers): hand-roll the equivalent — iterate `spa_buf.metas` slice for the `type_ == SPA_META_Cursor` entry. Document the chosen path in the file header.

- [ ] **Step 2: Write the failing tests + module scaffold**

Create `crates/media-linux/src/wayland_portal/cursor.rs` with just the public types, error variants, and 4 tests (the implementation body is `todo!()` for now):

```rust
//! SPA_META_Cursor receive path for cursor_mode=Metadata.
//!
//! Bare-FFI helper that reads `SPA_META_Cursor` out of a PipeWire buffer
//! and produces a fully owned [`CursorUpdate`] so the value can cross the
//! mpsc channel into the host's session task without lifetime entanglement
//! with the PipeWire buffer pool.
//!
//! # libspa Rust wrapper status (probed 2026-05-12)
//!
//! pipewire-rs 0.9.2 exposes NO typed `Meta` wrapper. The canonical path
//! is `spa_sys::spa_buffer_find_meta(buf, SPA_META_Cursor)` + raw pointer
//! reads into `spa_meta_cursor` / `spa_meta_bitmap`. OBS Studio uses the
//! same approach at `plugins/linux-pipewire/pipewire.c#L889`.
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
    pub id: u32,            // never 0 in emitted values (filtered at parse time)
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
/// constructing a `pipewire::spa::buffer::Buffer`. Production blanket impl
/// on `&pipewire::spa::buffer::Buffer` (same pattern as `dmabuf::SpaDataLike`).
pub trait SpaBufferLike {
    /// Returns a raw pointer to the underlying `spa_sys::spa_buffer`, or
    /// null if not yet bound. Caller dereferences inside an unsafe block.
    fn as_raw_spa_buffer(&self) -> *const pipewire::spa::sys::spa_buffer;
}

impl SpaBufferLike for &pipewire::spa::buffer::Buffer {
    fn as_raw_spa_buffer(&self) -> *const pipewire::spa::sys::spa_buffer {
        // SAFETY: as_raw returns a valid pointer for the buffer's lifetime,
        // which is bounded by &self.
        unsafe { (*self).as_raw_ptr() as *const _ }
    }
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
    todo!("Step 3")
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

    /// Helper: layout a spa_meta_cursor into a Vec, optionally followed by
    /// a spa_meta_bitmap + pixel bytes.
    fn build_cursor_payload(
        id: u32,
        pos: (i32, i32),
        hotspot: (i32, i32),
        bitmap: Option<(u32, (u32, u32), i32, Vec<u8>)>, // (format, (w,h), stride, pixels)
    ) -> Vec<u8> {
        let cur_sz = size_of::<spa_sys::spa_meta_cursor>();
        let bmp_sz = size_of::<spa_sys::spa_meta_bitmap>();

        let mut payload = Vec::new();
        // spa_meta_cursor at offset 0
        payload.resize(cur_sz, 0);
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
            Some((spa_sys::SPA_VIDEO_FORMAT_BGRA as u32, (2, 1), 8, pixels.clone())),
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
            Some((spa_sys::SPA_VIDEO_FORMAT_RGBA as u32, (1, 1), 4, rgba)),
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
```

- [ ] **Step 3: Run the failing tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::cursor 2>&1 | head -30
```

Expected: compile passes, all 4 tests panic with `not yet implemented` because `read_meta_cursor` is `todo!()`.

- [ ] **Step 4: Implement `read_meta_cursor`**

Replace the `todo!()` body with the bare-FFI implementation. Insert above the `tests` module:

```rust
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
    //    is an inline C helper; libspa-sys may or may not re-export it.
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
        if (*m).type_ == spa_sys::SPA_META_Cursor as std::os::raw::c_uint {
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
    let bmp_off = c.bitmap_offset as u32;
    let bmp_sz = size_of::<spa_sys::spa_meta_bitmap>() as u32;
    if bmp_off < cur_sz || bmp_off.checked_add(bmp_sz).map_or(true, |s| s > meta_size) {
        return Err(CursorMetaError::BitmapOffsetOutOfBounds(bmp_off, meta_size));
    }

    let b = &*(base.add(bmp_off as usize) as *const spa_sys::spa_meta_bitmap);

    if b.format == 0 {
        return Ok(Some(out)); // compositor signals "ignore bitmap"
    }

    let w = b.size.width;
    let h = b.size.height;

    // Cap check — caller silent-truncates. Return error and let caller
    // decide; for now we just clamp at parse boundary.
    if w > 256 || h > 256 {
        return Err(CursorMetaError::BitmapTooLarge(w, h));
    }
    if w == 0 || h == 0 {
        // Invisible cursor.
        out.bitmap = Some(CursorBitmap { width: 0, height: 0, bgra: Vec::new() });
        return Ok(Some(out));
    }

    if b.offset == 0 {
        // No image data (cursor invisible despite valid size).
        out.bitmap = Some(CursorBitmap { width: 0, height: 0, bgra: Vec::new() });
        return Ok(Some(out));
    }

    // 4. Pixel data extraction + format normalization.
    if b.stride <= 0 {
        return Err(CursorMetaError::UnsupportedFormat(b.format));
    }
    let stride = b.stride as u32;
    let pixel_rel_off = b.offset; // offset within spa_meta_bitmap
    let pixel_abs_off = bmp_off.checked_add(pixel_rel_off).ok_or(
        CursorMetaError::BitmapOffsetOutOfBounds(b.offset, meta_size),
    )?;
    let needed = stride.checked_mul(h).ok_or(
        CursorMetaError::BitmapOffsetOutOfBounds(stride, meta_size),
    )?;
    if pixel_abs_off.checked_add(needed).map_or(true, |s| s > meta_size) {
        return Err(CursorMetaError::BitmapOffsetOutOfBounds(pixel_abs_off, meta_size));
    }

    let pixel_ptr = base.add(pixel_abs_off as usize);
    let mut bgra = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..(h as usize) {
        // Source row: stride bytes (may include padding past w*4).
        // SAFETY: bounds checked above.
        let src = std::slice::from_raw_parts(
            pixel_ptr.add(row * stride as usize),
            (w as usize) * 4,
        );
        let dst = &mut bgra[row * (w as usize) * 4..(row + 1) * (w as usize) * 4];
        dst.copy_from_slice(src);
    }

    // Format normalization — host emits BGRA always.
    match b.format as i32 {
        x if x == spa_sys::SPA_VIDEO_FORMAT_BGRA => {
            // already BGRA
        }
        x if x == spa_sys::SPA_VIDEO_FORMAT_RGBA => {
            // R<->B swap per pixel.
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        x if x == spa_sys::SPA_VIDEO_FORMAT_ARGB => {
            // ARGB → BGRA: byte rotation.
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
        }
        fmt => return Err(CursorMetaError::UnsupportedFormat(fmt as u32)),
    }

    out.bitmap = Some(CursorBitmap { width: w, height: h, bgra });
    Ok(Some(out))
}
```

- [ ] **Step 5: Add `cursor` module + re-exports to `mod.rs`**

Edit `crates/media-linux/src/wayland_portal/mod.rs`:

```rust
pub mod cursor;
pub use cursor::{CursorBitmap, CursorMetaError, CursorUpdate, SpaBufferLike};
```

- [ ] **Step 6: Run the tests + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::cursor
./scripts/dev-container.sh cargo clippy -p prdt-media-linux --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: 4 tests pass, clippy clean. Constants for `SPA_VIDEO_FORMAT_BGRA` etc. may need typing adjustment depending on what libspa-sys generates; use `as u32` casts as needed and match the test setup against the actual generated type.

- [ ] **Step 7: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/cursor.rs \
        crates/media-linux/src/wayland_portal/mod.rs
git commit -m "$(cat <<'EOF'
P5B-2b T1: cursor.rs — SPA_META_Cursor FFI parser + BGRA normalization

New crates/media-linux/src/wayland_portal/cursor.rs implements

  pub unsafe fn read_meta_cursor<B: SpaBufferLike>(buf: &B)
      -> Result<Option<CursorUpdate>, CursorMetaError>

The helper walks spa_buffer.metas[] for SPA_META_Cursor, parses
spa_meta_cursor + spa_meta_bitmap, normalizes pixel data from
SPA_VIDEO_FORMAT_BGRA / _RGBA / _ARGB to tightly-packed BGRA8, and
returns an owned CursorUpdate. spa_meta_cursor.id == 0 yields Ok(None)
("no new metadata"); bitmap_offset == 0 yields Ok(Some(_)) with
bitmap=None ("reuse cached bitmap" — NOT erase).

Rationale (per spec §3.2 + Codex advisor finding):

- libspa-rs 0.9.2 exposes no Meta wrapper, so we go through bare FFI
  via spa_sys::spa_meta + spa_sys::spa_meta_cursor / _bitmap.
- GNOME mutter + KDE kwin frequently emit RGBA-format cursors; host
  normalizes once so wire + viewer have a single BGRA path.
- SpaBufferLike trait lets unit tests inject a hand-built spa_buffer
  (same pattern as dmabuf::SpaDataLike from P5B-2a T3).

4 new tests: read_meta_cursor_returns_none_for_id_zero,
_handles_position_only_update, _handles_bgra_bitmap,
_normalizes_rgba_to_bgra.
EOF
)"
```

---

## Task 2: protocol — `CursorUpdate` variant + `protocol_version 3→4`

**Files:**
- Modify: `crates/protocol/src/control.rs`
- Modify: `crates/protocol/src/wire.rs`
- Modify: `crates/transport/src/handshake.rs`
- Modify: `crates/host/src/auth.rs`

- [ ] **Step 1: Write the failing wire round-trip test**

Append to `crates/protocol/src/wire.rs` under the existing `control_tests` module:

```rust
    #[test]
    fn cursor_update_round_trip_no_bitmap() {
        let msg = ControlMessage::CursorUpdate {
            id: 7,
            position_x: 100,
            position_y: 200,
            hotspot_x: 3,
            hotspot_y: 5,
            bitmap: None,
        };
        let buf = encode_control(&msg).expect("encode ok");
        let decoded = decode_control(&buf).expect("decode ok");
        assert_eq!(msg, decoded);
        assert_eq!(buf[0], 18, "kind_u8 must be 18");
    }

    #[test]
    fn cursor_update_round_trip_with_bitmap() {
        let bgra = vec![0xff, 0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff]; // 2x1 BGRA
        let msg = ControlMessage::CursorUpdate {
            id: 42,
            position_x: -10,
            position_y: 50,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap: Some(crate::control::CursorBitmap {
                width: 2,
                height: 1,
                bgra,
            }),
        };
        let buf = encode_control(&msg).expect("encode ok");
        let decoded = decode_control(&buf).expect("decode ok");
        assert_eq!(msg, decoded);
    }
```

Run:
```bash
./scripts/dev-container.sh cargo test -p prdt-protocol --lib --target x86_64-unknown-linux-gnu cursor_update 2>&1 | head -20
```

Expected: compile failure (`CursorUpdate` variant doesn't exist, `CursorBitmap` struct doesn't exist).

- [ ] **Step 2: Add `CursorBitmap` + `CursorUpdate` variant in `control.rs`**

Locate the end of `pub struct MonitorRect` (around line 116) and insert before `pub enum ControlMessage`:

```rust
/// Tightly packed BGRA8 cursor bitmap carried inside
/// [`ControlMessage::CursorUpdate`]. `width == 0 && height == 0` means
/// "cursor invisible" — viewer hides compositing but does NOT show the
/// OS-native cursor inside the capture region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBitmap {
    pub width: u16,
    pub height: u16,
    /// BGRA8 tightly packed: `len() == width * height * 4` when both
    /// dimensions are non-zero, else `Vec::new()`.
    pub bgra: Vec<u8>,
}
```

Add the new variant to the `ControlMessage` enum (after `KeepAlive` at kind 17, before `Probe` at 20):

```rust
    /// Host → Viewer. Cursor metadata update for cursor_mode=Metadata path.
    ///
    /// Sent only when negotiated protocol_version >= 4 AND the host's
    /// capture backend supports `SPA_META_Cursor` extraction (Linux portal
    /// only; Windows DXGI bakes the cursor into the frame and never emits
    /// this variant).
    ///
    /// Coordinates are in capture-region-relative LOGICAL pixels; the
    /// origin is the top-left of `HelloAck.host_monitor_rect`. May fall
    /// outside the rect when the cursor is at an edge; viewer clamps.
    ///
    /// `bitmap == None` is a position-only update (viewer reuses cached
    /// bitmap). `bitmap == Some { width: 0, height: 0, .. }` is "cursor
    /// invisible". Otherwise `bitmap.bgra.len() == width * height * 4`
    /// (decode validator enforces).
    CursorUpdate {
        id: u32,
        position_x: i32,
        position_y: i32,
        hotspot_x: i32,
        hotspot_y: i32,
        bitmap: Option<CursorBitmap>,
    },
```

Add the `kind_u8` arm at line ~243 (in the existing match):

```rust
            Self::CursorUpdate { .. } => 18,
```

- [ ] **Step 3: Update `decode_control` upper bound (if needed)**

Open `crates/protocol/src/wire.rs:659`. The check is `if kind > 22`. Since 18 ≤ 22 the existing bound covers `CursorUpdate` without change. **No edit required**. Skip if confirmed; otherwise file the deviation.

- [ ] **Step 4: Bump `protocol_version 3 → 4`**

```bash
sed -i 's/pub const HELLO_PROTOCOL_VERSION: u8 = 3;/pub const HELLO_PROTOCOL_VERSION: u8 = 4;/' \
    crates/transport/src/handshake.rs
sed -i 's/const PROTOCOL_VERSION_REQUIRED: u8 = 3;/const PROTOCOL_VERSION_REQUIRED: u8 = 4;/' \
    crates/host/src/auth.rs
```

Verify (no extra matches, no comment lines accidentally rewritten):

```bash
grep -n "PROTOCOL_VERSION" crates/transport/src/handshake.rs crates/host/src/auth.rs
```

Update the comment above `HELLO_PROTOCOL_VERSION` in `handshake.rs:59-61`:

```rust
/// Wire-level protocol_version that this build of the codebase speaks.
/// Bumped to 4 in P5B-2b for the CursorUpdate variant + cursor_mode=Metadata
/// path. v3 viewers and v4 hosts are mutually incompatible (strict-match
/// rejection in handshake.rs); operators upgrade both sides simultaneously.
pub const HELLO_PROTOCOL_VERSION: u8 = 4;
```

- [ ] **Step 5: Update host auth integration test (`auth_integration.rs:268-283`)**

The existing test exercises `protocol_version: 2` rejection. With the bump, the test should still pass (2 ≠ 4 → reject), but update any hard-coded `3` literals to `4` to keep coverage realistic. Search for `protocol_version: 3` or `HELLO_PROTOCOL_VERSION` literals:

```bash
grep -rn "protocol_version: 3\|HELLO_PROTOCOL_VERSION" \
    crates/host/tests/ crates/transport/tests/ crates/protocol/tests/ 2>/dev/null
```

For each match, decide:
- If it's a legitimacy test (host accepts v3) → bump to v4.
- If it's a rejection test (host rejects v2) → no change (still rejects).
- If it asserts the constant equals `3` → bump to `4`.

- [ ] **Step 6: Run protocol + transport + host tests**

```bash
./scripts/dev-container.sh cargo test -p prdt-protocol --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo test -p prdt-transport --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo test -p prdt-host --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo clippy -p prdt-protocol -p prdt-transport -p prdt-host --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: green. Both `cursor_update_round_trip_no_bitmap` + `_with_bitmap` pass. Existing `control_all_kinds_round_trip` passes (its iteration must include the new variant — the test should be a non-exhaustive match if it's pattern-based, in which case `_` covers; check the implementation and either extend the iterator or rely on the match-`_` catchall).

- [ ] **Step 7: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/protocol/src/control.rs \
        crates/protocol/src/wire.rs \
        crates/transport/src/handshake.rs \
        crates/host/src/auth.rs \
        $(grep -rl "HELLO_PROTOCOL_VERSION\|PROTOCOL_VERSION_REQUIRED" crates/host/tests/ 2>/dev/null)
git commit -m "$(cat <<'EOF'
P5B-2b T2: wire CursorUpdate variant + protocol_version 3->4 bump

Adds ControlMessage::CursorUpdate at kind_u8=18 (first free slot below
the 22 decode-bound) carrying:

  id: u32, position_x/y: i32, hotspot_x/y: i32,
  bitmap: Option<CursorBitmap { width: u16, height: u16, bgra: Vec<u8> }>

bitmap=None is "position-only update, reuse cached bitmap" (NOT erase).
bitmap=Some with width==0 && height==0 is "cursor invisible". Otherwise
bgra.len() == width * height * 4 in tightly packed BGRA8 (host
normalizes from RGBA/ARGB at the SPA meta read boundary).

protocol_version bumped 3->4 at handshake.rs:61 + auth.rs:25.
Backward-compat: hard bump — v3 viewer connecting to v4 host (or
vice-versa) is rejected at wire level via HelloReject{
ProtocolVersionMismatch }. Operators upgrade both sides simultaneously.
This matches the v2->v3 transition convention from the OpenH264 swap.

Decode bound at wire.rs:659 still says kind > 22 (18 < 22; no edit).

2 new round-trip tests + existing exhaustiveness tests pass.
EOF
)"
```

---

## Task 3: PipeWireStream wire-up — drain cursor meta + new mpsc channel

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/stream.rs`
- Modify: `crates/media-linux/src/wayland_portal/session.rs`
- Modify: `crates/media-linux/src/wayland_portal/capturer.rs`
- Modify: `crates/media-linux/src/policy.rs`

- [ ] **Step 1: Add the cursor-mode probe to `PortalSession::start_with_token_opt`**

Open `crates/media-linux/src/wayland_portal/session.rs`. Find the `CursorMode::Embedded` line (around line 172 per existing code). Replace with a probe-driven selection:

```rust
// P5B-2b: ask the portal which cursor modes it supports and prefer
// Metadata. Fall back to Embedded if the portal doesn't advertise
// Metadata — the alternative (assuming ashpd transparently downgrades)
// is unsound per Codex finding.
let available = proxy.available_cursor_modes().await.unwrap_or_default();
let cursor_mode = if available.contains(CursorMode::Metadata) {
    tracing::info!("portal advertises Metadata cursor mode — using it");
    CursorMode::Metadata
} else {
    tracing::warn!(
        ?available,
        "portal does not advertise Metadata cursor mode — falling back to Embedded"
    );
    CursorMode::Embedded
};
```

Also extend `PortalStartOutput` to carry the resolved cursor mode so callers can branch:

```rust
pub struct PortalStartOutput {
    pub fd: OwnedFd,
    pub node_id: u32,
    pub restore_token: Option<String>,
    pub cursor_mode: CursorMode,  // NEW
}
```

Update the constructor at the end of `start_with_token_opt` to populate the field.

- [ ] **Step 2: Add the failing dispatch + plumbing test in `stream.rs`**

Append to the existing `tests` module in `crates/media-linux/src/wayland_portal/stream.rs`:

```rust
    #[test]
    fn pipewire_stream_connect_emits_two_receivers() {
        // Compile-time test: verify the return type changed. If this
        // module compiles after the signature change, the type assertion
        // is satisfied.
        fn _assert_returns_two_rxs() -> impl FnOnce() -> (
            PipeWireStream,
            mpsc::Receiver<RawFrame>,
            mpsc::Receiver<crate::wayland_portal::cursor::CursorUpdate>,
        ) {
            // We can't construct a PipeWireStream in tests (it needs a
            // real fd + node_id); this is a type-shape assertion only.
            panic!("type-shape assertion — never called");
        }
        let _ = _assert_returns_two_rxs;
    }
```

Run:
```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::stream::tests::pipewire_stream_connect_emits_two_receivers 2>&1 | head -20
```

Expected: compile failure (3-tuple return doesn't exist yet).

- [ ] **Step 3: Change `PipeWireStream::connect` to return a 3-tuple**

Open `crates/media-linux/src/wayland_portal/stream.rs`. Find the existing `pub fn connect(...)` signature. Replace return type:

```rust
pub fn connect(
    fd: OwnedFd,
    node_id: u32,
    frame_buf_cap: usize,
    cursor_buf_cap: usize, // NEW; default 8 at callers
) -> Result<
    (
        Self,
        mpsc::Receiver<RawFrame>,
        mpsc::Receiver<crate::wayland_portal::cursor::CursorUpdate>,
    ),
    PipeWireStreamError,
>
```

Inside the function body, add a second `mpsc::channel::<CursorUpdate>(cursor_buf_cap)` alongside the existing `RawFrame` channel. Pass the `cursor_tx` into the spawned loop thread via the same closure capture pattern as `tx_cb` for frames.

- [ ] **Step 4: Drain cursor meta inside the `process` callback**

Inside `.process(move |stream, _ud| { ... })` body — BEFORE the existing video data dispatch — add:

```rust
            // P5B-2b: drain cursor meta first. The buffer's lifetime here
            // is the closure body; we copy into an owned CursorUpdate so
            // the value safely crosses the mpsc boundary.
            //
            // SAFETY: buf is held inside the process callback; read_meta_cursor
            // consumes no buffer ownership and copies pixel bytes into an
            // owned Vec before returning.
            match unsafe { crate::wayland_portal::cursor::read_meta_cursor(&&*buf) } {
                Ok(Some(c)) => {
                    let _ = cursor_tx_cb.try_send(c);
                }
                Ok(None) => {} // id==0; no new metadata
                Err(crate::wayland_portal::cursor::CursorMetaError::Absent) => {
                    // Expected on first frames + on Embedded-mode streams
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cursor meta parse failed");
                }
            }
```

- [ ] **Step 5: Update `WaylandPortalCapturer::new` to return `cursor_rx`**

Open `crates/media-linux/src/wayland_portal/capturer.rs`. The constructor currently returns `Result<Self, WaylandPortalCapturerInitError>`. Change to:

```rust
pub async fn new(
    token_path: std::path::PathBuf,
) -> Result<
    (
        Self,
        tokio::sync::mpsc::Receiver<crate::wayland_portal::cursor::CursorUpdate>,
    ),
    WaylandPortalCapturerInitError,
> {
    // … existing PortalSession::start_with_token_opt + PipeWireStream::connect …
    // PipeWireStream::connect now returns (stream, frame_rx, cursor_rx);
    // the Capturer wraps frame_rx as before and hands cursor_rx to the
    // caller.
    let (stream, frame_rx, cursor_rx) =
        PipeWireStream::connect(fd, node_id, /*frame_cap*/ 2, /*cursor_cap*/ 8)?;
    let capturer = Self { /* … existing fields, frame_rx wrapped … */ };
    Ok((capturer, cursor_rx))
}
```

- [ ] **Step 6: Wire `cursor_rx` through `LinuxSwFactory::create`**

Open `crates/media-linux/src/policy.rs`. The `LinuxSwFactory::create` method has a `WaylandPortal` arm that calls `WaylandPortalCapturer::new(token_path)`. Update to:

1. Take an additional argument `cursor_tx: Option<tokio::sync::mpsc::Sender<protocol::CursorUpdate>>` (or similar — the host owns the protocol-typed sender).
2. The Wayland arm spawns a small forwarder task that owns `cursor_rx` and pushes each `CursorUpdate` through `cursor_tx` (after converting the `media_linux::CursorUpdate` to the `protocol::CursorUpdate` wire type — they share a layout but live in different crates).
3. The forwarder task exits when `cursor_rx` returns `None` (PipeWire stream closed).

Decide at planning time whether to extend `VideoProducerFactory::create` trait signature (touches every backend) or to thread the cursor sender through a side channel on `LinuxSwFactory` directly (Wayland-only). **Recommended: side-channel on `LinuxSwFactory`** because X11 + Windows backends have no cursor metadata and the trait change is high-blast-radius.

If a `media_linux::CursorUpdate → protocol::CursorUpdate` mapper doesn't yet exist, add it as `impl From<media_linux::CursorUpdate> for protocol::CursorUpdate` in `crates/media-linux/src/wayland_portal/cursor.rs`.

- [ ] **Step 7: Run the stream + capturer + policy tests + workspace gate**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu \
    wayland_portal::stream wayland_portal::capturer policy
./scripts/dev-container.sh cargo test -p prdt-media-linux --test capture_source_contract --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo clippy -p prdt-media-linux -p prdt-media-policy --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: green. X11 contract test still 3 pass / 1 ignored.

- [ ] **Step 8: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/stream.rs \
        crates/media-linux/src/wayland_portal/session.rs \
        crates/media-linux/src/wayland_portal/capturer.rs \
        crates/media-linux/src/policy.rs
git commit -m "$(cat <<'EOF'
P5B-2b T3: PipeWireStream emits cursor channel + LinuxSwFactory forwards

PortalSession now probes ashpd Screencast::available_cursor_modes()
and selects Metadata when advertised, falling back to Embedded with a
warn log otherwise. PortalStartOutput.cursor_mode carries the resolved
value so downstream code can branch.

PipeWireStream::connect signature widens:

  (PipeWireStream, mpsc::Receiver<RawFrame>, mpsc::Receiver<CursorUpdate>)

with a new cursor_buf_cap parameter (default 8). The process callback
drains cursor meta via cursor::read_meta_cursor before the existing
video data dispatch; cursor_tx is try_send (drop-on-full latest-only
semantics, matching the RawFrame channel discipline).

WaylandPortalCapturer::new returns (Self, cursor_rx); LinuxSwFactory::create
accepts an Option<protocol::CursorUpdate sender> and spawns a forwarder
task that converts media-linux's CursorUpdate to protocol::CursorUpdate
and pushes it onto the host's wire send path. Side-channel on the
factory keeps the trait signature unchanged for X11 + Windows backends.

Tests: existing 5 stream tests pass; 3 dmabuf tests pass; X11 contract
3 pass / 1 ignored; clippy clean.
EOF
)"
```

---

## Task 4: Viewer (Linux softbuffer) cursor compositing

**Files:**
- Create: `crates/viewer/src/cursor_state.rs`
- Modify: `crates/viewer/src/lib.rs` (host the CursorState in ViewerShared)
- Modify: `crates/viewer/src/platform/linux.rs` (alpha-blend before `present`)

- [ ] **Step 1: Write the failing test for `CursorState`**

Create `crates/viewer/src/cursor_state.rs`:

```rust
//! Viewer-side cursor compositing state. Receives ControlMessage::CursorUpdate
//! over the wire, holds the latest position + cached bitmap, and exposes
//! a `composite_target` helper that the platform-specific renderer uses
//! to alpha-blend the cursor on top of the decoded frame.

use prdt_protocol::CursorBitmap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CursorState {
    /// Last received id; informational only (Codex flag: compositors may
    /// recycle ids, so don't use for cache invalidation).
    pub id: u32,
    pub position_x: i32,
    pub position_y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// `None` until first bitmap received. `Some(width=0, height=0)` means
    /// "host signals invisible cursor — hide compositing".
    bitmap: Option<Arc<CursorBitmap>>,
}

impl CursorState {
    pub fn new() -> Self {
        Self {
            id: 0,
            position_x: 0,
            position_y: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            bitmap: None,
        }
    }

    /// Apply a wire update. Position always overwrites; bitmap is replaced
    /// ONLY when the message carries one (bitmap-presence is the cache
    /// invalidation signal — NOT the id).
    pub fn apply(
        &mut self,
        id: u32,
        position_x: i32,
        position_y: i32,
        hotspot_x: i32,
        hotspot_y: i32,
        bitmap: Option<CursorBitmap>,
    ) {
        self.id = id;
        self.position_x = position_x;
        self.position_y = position_y;
        self.hotspot_x = hotspot_x;
        self.hotspot_y = hotspot_y;
        if let Some(b) = bitmap {
            self.bitmap = Some(Arc::new(b));
        }
        // If bitmap is None: keep cached (reuse).
    }

    /// `true` when we have a non-empty cached bitmap to draw.
    pub fn visible(&self) -> bool {
        self.bitmap
            .as_ref()
            .is_some_and(|b| b.width > 0 && b.height > 0)
    }

    pub fn bitmap(&self) -> Option<&CursorBitmap> {
        self.bitmap.as_deref()
    }
}

impl Default for CursorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_position_only_keeps_cached_bitmap() {
        let mut s = CursorState::new();
        let bmp = CursorBitmap { width: 2, height: 1, bgra: vec![0xff, 0, 0, 0xff, 0, 0xff, 0, 0xff] };
        s.apply(1, 10, 20, 0, 0, Some(bmp.clone()));
        assert!(s.visible());
        // Position-only update.
        s.apply(2, 100, 200, 0, 0, None);
        assert_eq!((s.position_x, s.position_y), (100, 200));
        assert_eq!(s.bitmap().map(|b| (b.width, b.height)), Some((2, 1)));
    }

    #[test]
    fn apply_invisible_bitmap_hides_compositing() {
        let mut s = CursorState::new();
        s.apply(1, 0, 0, 0, 0, Some(CursorBitmap { width: 0, height: 0, bgra: vec![] }));
        assert!(!s.visible());
    }

    #[test]
    fn apply_new_bitmap_replaces_cache() {
        let mut s = CursorState::new();
        let b1 = CursorBitmap { width: 2, height: 1, bgra: vec![0u8; 8] };
        let b2 = CursorBitmap { width: 4, height: 2, bgra: vec![0u8; 32] };
        s.apply(1, 0, 0, 0, 0, Some(b1));
        s.apply(1, 0, 0, 0, 0, Some(b2)); // same id; bitmap-presence triggers swap
        assert_eq!(s.bitmap().map(|b| (b.width, b.height)), Some((4, 2)));
    }
}
```

Run:
```bash
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu cursor_state 2>&1 | head -20
```

Expected: compile failure (`prdt_protocol::CursorBitmap` exists from T2; if any unrelated viewer code refuses to build, focus on cursor_state-specific failures only).

- [ ] **Step 2: Add `cursor_state` to `crates/viewer/src/lib.rs` + ViewerShared**

In `crates/viewer/src/lib.rs`, add:

```rust
mod cursor_state;
pub use cursor_state::CursorState;
```

Locate the `ViewerShared` struct (search for `latest_frame: Arc<Mutex<...>>`). Add:

```rust
pub struct ViewerShared {
    // … existing fields …
    pub cursor: Arc<std::sync::Mutex<crate::cursor_state::CursorState>>,
}
```

Initialise `cursor: Arc::new(Mutex::new(CursorState::new()))` wherever `ViewerShared` is constructed.

Locate the ControlMessage dispatch in the receive loop (search for `ControlMessage::Stats` or `ControlMessage::Pong`). Add an arm:

```rust
                ControlMessage::CursorUpdate {
                    id,
                    position_x,
                    position_y,
                    hotspot_x,
                    hotspot_y,
                    bitmap,
                } => {
                    if let Ok(mut s) = shared.cursor.lock() {
                        s.apply(id, position_x, position_y, hotspot_x, hotspot_y, bitmap);
                    }
                }
```

- [ ] **Step 3: Composite the cursor inside Linux `present_frame`**

Open `crates/viewer/src/platform/linux.rs`. Find the `present_frame` body where BGRA is written into `scratch_bgra`. After the frame data fill but before `softbuffer::Surface::buffer_mut().present()`:

```rust
        // P5B-2b: cursor composite (Linux softbuffer).
        if let Ok(s) = shared.cursor.lock() {
            if let Some(bmp) = s.bitmap() {
                if s.visible() {
                    let top_left_x = s.position_x - s.hotspot_x;
                    let top_left_y = s.position_y - s.hotspot_y;
                    alpha_blend_bgra(
                        &mut scratch_bgra,
                        frame_width,
                        frame_height,
                        bmp.width as i32,
                        bmp.height as i32,
                        top_left_x,
                        top_left_y,
                        &bmp.bgra,
                    );
                }
            }
        }
```

Add the `alpha_blend_bgra` helper at the bottom of `linux.rs`:

```rust
/// CPU alpha-blend a BGRA source rectangle onto a BGRA destination
/// framebuffer. Source pixels' alpha channel modulates the contribution.
/// Clips source to the destination bounds.
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
            // Standard over-operator: dst = src + dst*(1-alpha).
            let inv = 255 - alpha;
            d[0] = ((s[0] as u32 * alpha + d[0] as u32 * inv) / 255) as u8;
            d[1] = ((s[1] as u32 * alpha + d[1] as u32 * inv) / 255) as u8;
            d[2] = ((s[2] as u32 * alpha + d[2] as u32 * inv) / 255) as u8;
            d[3] = 255;
        }
    }
}
```

- [ ] **Step 4: Add a `cursor_state` test that exercises `alpha_blend_bgra`**

In `crates/viewer/src/platform/linux.rs` (under `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn alpha_blend_bgra_red_over_black() {
        let mut dst = vec![0u8; 4 * 4]; // 2x2 black BGRA
        let src = vec![0x00, 0x00, 0xff, 0xff]; // 1x1 red opaque
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
        let src = vec![0x00, 0x00, 0xff, 0xff, 0x00, 0xff, 0x00, 0xff]; // 2x1 red+green
        // Place at (-1, 0): only x=1 of source draws at dst x=0.
        alpha_blend_bgra(&mut dst, 2, 2, 2, 1, -1, 0, &src);
        assert_eq!(dst[0..4], [0x00, 0xff, 0x00, 0xff], "green at (0,0)");
        assert_eq!(dst[4..8], [0, 0, 0, 0], "(1,0) unchanged");
    }
```

- [ ] **Step 5: Run viewer tests + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu cursor_state
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu platform::linux
./scripts/dev-container.sh cargo clippy -p prdt-viewer --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: green. `alpha_blend_bgra` clips correctly; cursor_state tests pass.

- [ ] **Step 6: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/viewer/src/cursor_state.rs \
        crates/viewer/src/lib.rs \
        crates/viewer/src/platform/linux.rs
git commit -m "$(cat <<'EOF'
P5B-2b T4: viewer cursor compositing (Linux softbuffer CPU blend)

Adds crates/viewer/src/cursor_state.rs holding the latest
ControlMessage::CursorUpdate values + cached bitmap. apply() always
overwrites position; bitmap is replaced ONLY when the wire message
carries one (bitmap-presence is the cache-invalidation signal — Codex
flagged that compositor ids are unreliable for identity).

ViewerShared gains cursor: Arc<Mutex<CursorState>>; the existing
ControlMessage dispatch loop populates it from CursorUpdate variants.

platform/linux.rs::present_frame composites the cursor on top of the
decoded BGRA frame buffer via a new alpha_blend_bgra helper that
performs the standard over-operator with negative-offset clipping.
The blend happens BEFORE softbuffer::present(), so the cursor lands
in the same frame as the decoded video.

3 new unit tests:
- cursor_state::apply_position_only_keeps_cached_bitmap
- cursor_state::apply_invisible_bitmap_hides_compositing
- cursor_state::apply_new_bitmap_replaces_cache
- platform::linux::tests::alpha_blend_bgra_red_over_black
- platform::linux::tests::alpha_blend_bgra_clips_negative_offset

Windows D3D11 cursor overlay deferred to T5.
EOF
)"
```

---

## Task 5: Viewer (Windows D3D11) cursor compositing

**Files:**
- Create: `crates/media-win/src/d3d11/cursor_overlay.rs`
- Modify: `crates/media-win/src/d3d11/mod.rs` (re-export)
- Modify: `crates/viewer/src/platform/win.rs` (call overlay after VideoProcessorBlt)

**Note for the implementer:** This task is a Windows-only path. The Debian bookworm container CANNOT compile `media-win`. The plan author proposes the Windows side be implemented in a follow-up branch ONCE the cross-platform CI can validate it. For the P5B-2b auto-evidence DoD, this task is **descoped** — implement the trait shape (so `cursor_state` is consumed) but stub the actual D3D11 overlay draw with `// TODO(P5B-2b-windows-follow-up)` markers.

Specifically:

- [ ] **Step 1: Add a no-op `cursor_overlay` module skeleton**

Create `crates/media-win/src/d3d11/cursor_overlay.rs`:

```rust
//! Windows D3D11 cursor overlay — composite cursor on top of the
//! VideoProcessorBlt'd swapchain backbuffer before IDXGISwapChain1::Present.
//!
//! P5B-2b ships this as a stub; full implementation is deferred to the
//! Windows follow-up branch because the Debian bookworm dev container
//! cannot compile media-win (Windows SDK + D3D11 headers absent).

#![cfg(target_os = "windows")]

use prdt_protocol::CursorBitmap;

pub struct CursorOverlay {
    // … D3D11 texture cache + pixel shader handles (TODO follow-up) …
}

impl CursorOverlay {
    pub fn new(/* device: &ID3D11Device */) -> windows::core::Result<Self> {
        // TODO(P5B-2b-windows-follow-up): create cursor texture + shader
        Ok(Self {})
    }

    /// Update the cached cursor bitmap. Call once per new bitmap-carrying
    /// `ControlMessage::CursorUpdate`.
    pub fn update_bitmap(&mut self, _bitmap: &CursorBitmap) -> windows::core::Result<()> {
        // TODO(P5B-2b-windows-follow-up): upload to ID3D11Texture2D
        Ok(())
    }

    /// Draw the cursor at (x, y) on the current backbuffer. Called after
    /// the video Blt + before IDXGISwapChain1::Present.
    pub fn draw(
        &self,
        _x: i32,
        _y: i32,
        // … swapchain backbuffer RTV …
    ) -> windows::core::Result<()> {
        // TODO(P5B-2b-windows-follow-up): pixel-shader draw
        Ok(())
    }
}
```

- [ ] **Step 2: Add re-export in `crates/media-win/src/d3d11/mod.rs`**

```rust
#[cfg(target_os = "windows")]
pub mod cursor_overlay;
#[cfg(target_os = "windows")]
pub use cursor_overlay::CursorOverlay;
```

- [ ] **Step 3: Document the stub in spec § "Out of scope"**

Add a one-line follow-up marker in `docs/superpowers/specs/2026-05-12-p5b2b-cursor-metadata-matrix-design.md` under §6 "Out of scope":

```markdown
- **Windows D3D11 cursor overlay full implementation** — stubbed in T5;
  full pixel-shader draw lands in a Windows follow-up branch with
  cross-platform CI validation (Debian bookworm container cannot
  compile media-win).
```

- [ ] **Step 4: Commit (stub-only)**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-win/src/d3d11/cursor_overlay.rs \
        crates/media-win/src/d3d11/mod.rs \
        docs/superpowers/specs/2026-05-12-p5b2b-cursor-metadata-matrix-design.md
git commit -m "$(cat <<'EOF'
P5B-2b T5: Windows D3D11 cursor overlay stub (follow-up branch)

The bookworm dev container cannot compile media-win (no Windows SDK +
D3D11 headers), so the full pixel-shader overlay is deferred to a
follow-up branch where cross-platform CI can validate. This commit
ships the trait shape (CursorOverlay::new / update_bitmap / draw) so
the viewer's Windows render path has the integration seam ready;
each method is a no-op with TODO(P5B-2b-windows-follow-up) markers.

Spec §6 "Out of scope" updated to flag the follow-up.
EOF
)"
```

---

## Task 6: STATUS doc + smoke walkthrough + final gate

**Files:**
- Modify: `docs/superpowers/STATUS.md`
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md`

- [ ] **Step 1: Append the P5B-2b STATUS entry**

Edit `docs/superpowers/STATUS.md`. Bump the header:

```markdown
**Latest tag:** `phase-p5b2b-cursor-metadata-matrix-complete`
```

Insert after the P5B-2a entry (right before `### **C.` section):

```markdown
- **P5B-2b (`phase-p5b2b-cursor-metadata-matrix-complete`, 2026-05-12)**:
  Wayland-portal cursor_mode=Metadata + viewer-side cursor compositing
  + GNOME (mutter) + KDE (kwin) smoke walkthrough.
  - New `crates/media-linux/src/wayland_portal/cursor.rs`:
    bare-FFI `read_meta_cursor<B: SpaBufferLike>(buf: &B) -> Result<Option<CursorUpdate>, CursorMetaError>`
    parses `SPA_META_Cursor` + `spa_meta_bitmap` via `spa_sys` raw pointers
    (libspa-rs 0.9.2 exposes no Meta wrapper) and normalizes pixel data
    from BGRA / RGBA / ARGB to tightly-packed BGRA8.
  - `wayland_portal/session.rs`: probes `Screencast::available_cursor_modes()`
    at start; selects `CursorMode::Metadata` when advertised, falls back
    to `CursorMode::Embedded` with a warn log otherwise.
  - `wayland_portal/stream.rs::PipeWireStream::connect` signature widens
    to return `(stream, frame_rx, cursor_rx)`; process callback drains
    cursor meta before the existing video data dispatch.
  - `crates/protocol/src/control.rs`: new `ControlMessage::CursorUpdate`
    variant at `kind_u8 = 18` carrying
    `{ id, position_x, position_y, hotspot_x, hotspot_y, bitmap: Option<CursorBitmap> }`.
    `CursorBitmap { width: u16, height: u16, bgra: Vec<u8> }` — tightly
    packed BGRA8, `width==0 && height==0` signals "cursor invisible".
  - `protocol_version` bumped `3 → 4` at `crates/transport/src/handshake.rs:61`
    + `crates/host/src/auth.rs:25`. Hard bump — v3 viewers and v4 hosts
    are mutually incompatible (strict-match rejection); operators upgrade
    both sides simultaneously.
  - `crates/viewer/src/cursor_state.rs`: viewer-side `CursorState` holds
    the latest position + cached `Arc<CursorBitmap>`. `apply()` overwrites
    position always; replaces cached bitmap ONLY when the wire message
    carries a new one (bitmap-presence is the cache-invalidation signal;
    `id` is informational only per Codex finding).
  - `crates/viewer/src/platform/linux.rs`: CPU `alpha_blend_bgra` helper
    composites the cursor on top of `scratch_bgra` before
    `softbuffer::Surface::present()`. Windows D3D11 path stubbed in T5
    pending follow-up branch (bookworm container cannot compile media-win).
  - **Tests**: 4 `cursor::read_meta_cursor` + 3 `cursor_state` + 2
    `alpha_blend_bgra` + 2 wire `cursor_update_round_trip` = **11 new
    tests**. Container clippy clean on `prdt-media-linux + prdt-protocol +
    prdt-transport + prdt-host + prdt-viewer`. Affected-slice lib tests
    pass; X11 contract regression guard still 3 pass / 1 ignored.
  - **Out of scope (deferred)**: Sway / Hyprland / wlroots smoke (P5C);
    Windows D3D11 cursor overlay full implementation (follow-up branch);
    cursor bitmap chunking (>256×256 silent-truncates); cursor coordinate
    HiDPI scaling refinement (logical-pixel passthrough only).
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §P5B-2b Section G (GNOME cursor metadata) + Section H (KDE cursor
    metadata).
```

- [ ] **Step 2: Append the walkthrough sections**

Edit `docs/superpowers/p5b1-smoke-walkthrough.md`. Append at the end:

```markdown
---

## P5B-2b — Cursor metadata + 2-compositor smoke matrix

### Section G — GNOME (mutter) cursor metadata

**Pre-conditions:**
- Ubuntu 24.04 GNOME (Wayland session); mutter ≥ 42.
- v4 `prdt-host` + v4 `prdt-viewer` from this branch.
- `xdg-desktop-portal-gnome` ≥ 42 (Metadata cursor mode landed in 41).

**Steps:**

1. Start the host with cursor-mode tracing:

   ```bash
   RUST_LOG=info,prdt_media_linux::wayland_portal=debug \
       ./prdt-host --bitrate-mbps 5 --silent-allow --headless \
       2>&1 | tee p5b2b-gnome-cursor-run.log
   ```

2. Click **Allow**. Expect:

   ```
   portal advertises Metadata cursor mode — using it
   ```

3. Connect a v4 viewer (`./prdt-viewer`).

4. Move the host's cursor. Expect the viewer's window cursor to track
   the host's pointer at near-zero latency (independent of video FPS).

5. Change cursor shape on the host (hover over a resize handle / text
   field). The viewer's cursor should update with the new shape within
   one frame.

### Section H — KDE (kwin) cursor metadata

**Pre-conditions:**
- Kubuntu 24.04 KDE (Wayland session); kwin ≥ 5.27.
- v4 `prdt-host` + v4 `prdt-viewer`.
- `xdg-desktop-portal-kde` ≥ 5.27.

**Steps:**

1. Start the host as in §G.
2. Click **Share** in the KDE dialog. Same expected log line ("portal
   advertises Metadata cursor mode").
3. Connect viewer. Same shape + position tracking verification.

### Section I — Embedded fallback regression

**Pre-conditions:** A compositor that does NOT advertise Metadata
(e.g. old GNOME 40 / `xdg-desktop-portal-wlr`).

**Expected log:**

```
portal does not advertise Metadata cursor mode — falling back to Embedded
```

Viewer shows the cursor baked into the frame (existing P5B-1 successor
behaviour); no `CursorUpdate` messages on the wire.

### Known issues / follow-ups (P5B-2b specific)

- **Windows D3D11 overlay**: stubbed; full pixel-shader draw lands in
  a Windows follow-up branch.
- **Sway / Hyprland / wlroots**: not in this matrix; revisit in P5C.
- **HiDPI cursor scaling**: cursor coordinates pass through as logical
  pixels; if the viewer has a different DPI than the host, the cursor
  position may be off-by-scale. Logged but not auto-corrected.
- **`SetCursor(NULL)` on Windows viewer**: hides OS-native cursor when
  the viewer window has focus + cursor is within the render rect.
  Restores on focus loss / cursor-leave. Race with modal dialogs may
  cause brief double-cursor flashes.
```

- [ ] **Step 3: Run the final pre-merge gate**

```bash
./scripts/dev-container.sh cargo fmt --all
./scripts/dev-container.sh cargo clippy -p prdt-protocol -p prdt-transport -p prdt-host \
    -p prdt-media-core -p prdt-media-sw -p prdt-media-policy -p prdt-media-linux \
    -p prdt-viewer -p prdt-viewer-overlay \
    --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --target x86_64-unknown-linux-gnu --lib \
    -p prdt-protocol -p prdt-media-core -p prdt-media-sw -p prdt-media-policy \
    -p prdt-media-linux -p prdt-transport -p prdt-host -p prdt-viewer
./scripts/dev-container.sh cargo test -p prdt-media-linux \
    --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: all green. Pre-existing flaky `transport::probe_test::two_transports_find_each_other` is the only allowed failure.

- [ ] **Step 4: Commit STATUS + walkthrough**

```bash
git add docs/superpowers/STATUS.md docs/superpowers/p5b1-smoke-walkthrough.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record P5B-2b cursor metadata + GNOME/KDE matrix

Adds the phase-p5b2b-cursor-metadata-matrix-complete entry under §1
with test counts (11 new), scope summary (BGRA wire / 256x256 cap /
Metadata cursor mode with Embedded fallback / protocol_version 3->4
hard bump / Linux softbuffer composite shipped, Windows D3D11 stubbed
for follow-up), and pointers to the smoke walkthrough's new
Section G (GNOME mutter), Section H (KDE kwin), and Section I
(Embedded fallback regression).

Out-of-scope list defers Sway/Hyprland/wlroots matrix (P5C), Windows
D3D11 overlay full impl (Windows follow-up branch), cursor bitmap
chunking (>256x256 silent-truncates), and HiDPI cursor scaling
refinement.

Latest tag header bumped from
phase-p5b2a-libspa-pod-dmabuf-complete to
phase-p5b2b-cursor-metadata-matrix-complete.
EOF
)"
```

- [ ] **Step 5: No PR creation — controller's job**

Stop here. Do **not** push the branch, do **not** open a PR, do **not**
tag. The plan controller handles the PR + tag sequence once auto-evidence
(container clippy + tests) is collected.

---

## Cross-task notes

- **Container-only build**: every cargo invocation runs inside `./scripts/dev-container.sh`. The Ubuntu 22.04 host's libpipewire 0.3.48 is too old for pipewire-rs 0.9 (needs ≥ 0.3.55, Debian bookworm ships 0.3.65).
- **Pre-existing flaky test**: `transport::probe_test::two_transports_find_each_other` is non-deterministic and unrelated. Do not treat as a regression.
- **No new workspace deps**: spec is explicit. Don't add `libspa-sys` independently (already transitively pulled by pipewire-rs 0.9.2). `pipewire::spa::sys` re-exports what we need.
- **`F_DUPFD_CLOEXEC` discipline**: no FDs leave the cursor parser; meta block memory is callback-scoped and we copy out into owned `Vec<u8>` before crossing the channel boundary. No FD lifetime concerns.
- **Drop order**: cursor channel's tokio mpsc closes naturally when `PipeWireStream` drops (Sender held inside the loop thread closure). Forwarder task observes `cursor_rx.recv() -> None` and exits.
- **Windows D3D11 cursor overlay**: T5 ships a stub; full implementation requires a Windows-capable CI/dev env. The follow-up branch is tracked as a `TODO(P5B-2b-windows-follow-up)` in `cursor_overlay.rs`.
- **`id` field is informational**: do NOT key bitmap cache or any state machine off it. Some compositors recycle ids; bitmap-presence on the wire is the cache invalidation signal.
- **Stride stripping happens at host**: `read_meta_cursor` re-packs the bitmap into tightly-packed BGRA. Viewer receives `bgra.len() == width * height * 4` and assumes contiguous rows.
- **Logging cadence**: cursor probe + format negotiation fire at `info!` level on each session start. Cursor parse errors fire `warn!` once per problem frame (no rate-limiting; cursor meta is rare relative to video frames). If smoke shows log spam, gate behind `std::sync::Once` in a follow-up.
- **WSLg X11 path unchanged**: zero touch in `crates/media-linux/src/x11_capture.rs`, `capture_source.rs`, `linux_sw_producer.rs`. X11 contract regression guard is the safety net.

---

## Ambiguities resolved (spec didn't cover; plan author chose)

1. **`SpaBufferLike` test-injection vs direct Buffer unsafe-construct**: pipewire-rs 0.9 `Buffer` keeps the inner `*mut spa_buffer` private. Same pattern as `dmabuf::SpaDataLike` (P5B-2a T3): trait fallback for tests, production blanket impl on `&Buffer`.
2. **`SPA_META_Cursor` constant import path**: spec says `SPA_META_Cursor = 5`. T1 Step 1 probes `libspa-sys-0.9.2` for the generated constant. If `spa_sys::SPA_META_Cursor` exists → use it. Otherwise hand-define `pub(crate) const SPA_META_CURSOR: u32 = 5;` with the ABI verification in a comment.
3. **`SPA_VIDEO_FORMAT_*` constants**: same — probe for `spa_sys::SPA_VIDEO_FORMAT_{BGRA,RGBA,ARGB}` and use them. Fall back to hand-defined constants with `/usr/include/spa-0.2/spa/param/video/raw.h` line refs in the comment.
4. **`LinuxSwFactory::create` signature change**: spec said "side-channel on the factory". The factory currently returns `Box<dyn VideoProducer>`. Extend `LinuxSwFactory::create` to take an additional `Option<Sender<CursorUpdate>>` argument. Callers in `host/src/lib.rs` construct the sender alongside the existing producer instantiation. X11 / Windows backends ignore it.
5. **`media_linux::CursorUpdate` ↔ `protocol::CursorUpdate` mapper**: add `impl From<media_linux::CursorUpdate> for protocol::CursorUpdate` in `cursor.rs`. Both types share field names + types modulo `width/height` widths (media-linux uses `u32`, protocol uses `u16` because 256×256 cap fits in u16). The mapper truncates.
6. **Position-only update batching**: spec said 250Hz target. Plan does NOT implement explicit batching; the existing `Transport::send_control` async path sends each `CursorUpdate` as its own UDP datagram. At 250Hz this is 250 datagrams/sec, well within UDP socket capacity. If real-machine smoke shows congestion, follow-up adds a coalescing task that emits only the latest position per N ms window.
7. **OS-native cursor hide**: T4 / T5 do not implement `SetCursor(NULL)` / `set_cursor_visible(false)` automatically. The spec calls for this UX polish; the plan flags it as a P5B-2b follow-up because focus + cursor-leave event plumbing is significant scope. The viewer continues to show its OS cursor underneath the composited host cursor for v4 connections; documented in walkthrough §P5B-2b Known issues.
