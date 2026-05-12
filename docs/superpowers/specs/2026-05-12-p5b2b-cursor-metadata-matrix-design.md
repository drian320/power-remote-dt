# P5B-2b: Cursor Metadata + Compositor Matrix — Design

**Status:** Draft (2026-05-12) — pending plan + implementation
**Predecessor:** `phase-p5b2a-libspa-pod-dmabuf-complete` (commit `caad13a`)
**Branch:** `phase-p5b2b-cursor-metadata-matrix`

## 1. Goal

Upgrade the Wayland-portal capture path from `cursor_mode=Embedded` (cursor baked into frame) to `cursor_mode=Metadata` (mode 4) so the cursor flows over the wire as a separate, high-frequency channel. Viewer composites the cursor on top of the decoded video frame; the *position* updates run independent of the video frame rate, giving Parsec/Moonlight-style "snappy cursor" UX even when video drops below 30 fps.

Ship a 2-compositor smoke walkthrough (GNOME mutter + KDE kwin); Sway / Hyprland / wlroots variants are descoped to P5C.

## 2. Constraints (locked in)

| Constraint | Choice | Why |
|---|---|---|
| Cursor scope | Metadata mode 4 + Embedded fallback | UX (cursor latency dominates user perception of remote-desktop responsiveness); fallback for compositors that don't advertise mode 4 |
| Protocol bump | `protocol_version: 3 → 4`, hard-bump (strict match) | Matches existing convention (handshake.rs:222 + auth.rs:146); no graceful-degrade plumbing exists |
| New variant | `ControlMessage::CursorUpdate { … }` at `kind_u8 = 18` | First free slot ≤ 22 (the decode bound); single-file change in `control.rs` |
| Bitmap wire format | BGRA + cap **256×256** (262 144 B) + **no chunking** | 4K HiDPI cursors top out at 128×128; silent-truncate on overflow keeps cursor visible |
| Compositor matrix | GNOME (mutter) + KDE (kwin) | 2026-time-of-writing both have stable mode-4 support; Sway/Hyprland descoped |
| DoD | Auto-evidence ship (container clippy + affected-crate lib tests + 2 new unit tests) | Same as P5A / P6 / P5B-1 / P5B-2a; real-machine smoke deferred to walkthrough doc |

## 3. Architecture

### 3.1 Capture-side (`crates/media-linux`)

```
WaylandPortalCapturer::new(token_path)
  ├─ PortalSession::start_with_token_opt(token_opt)
  │    ├─ Screencast::available_cursor_modes()  ← ashpd 0.12.3 probe (new in P5B-2b)
  │    │    BitFlags<CursorMode> → contains(Metadata)?
  │    │     ├─ yes → request CursorMode::Metadata
  │    │     └─ no  → request CursorMode::Embedded + log warn (fallback)
  │    └─ Start ScreenCast session
  │
  ├─ PipeWireStream::connect(fd, node_id)
  │    └─ stream listener:
  │         ├─ param_changed → existing format::parse  (unchanged P5B-2a path)
  │         └─ process callback:
  │              ├─ Video data path  → existing DMABUF/MemFd dispatch  (unchanged P5B-2a)
  │              └─ Cursor meta path → cursor::read_meta_cursor(buf)
  │                                    └─ if present → cursor_tx.try_send(CursorUpdate)
  │
  └─ stream returns (RawFrame channel, CursorUpdate channel)  ← two parallel channels
```

**Two-channel surface** at the `PipeWireStream` boundary:
- `frame_rx: mpsc::Receiver<RawFrame>` — existing P5B-1 path, unchanged
- `cursor_rx: mpsc::Receiver<CursorUpdate>` — **new**, decoupled so cursor updates aren't dropped when the video channel back-pressures

### 3.2 New module: `crates/media-linux/src/wayland_portal/cursor.rs`

Pure FFI helper that reads `SPA_META_Cursor` out of a `pipewire::spa::buffer::Buffer` and converts to an owned `CursorUpdate` struct.

```rust
//! SPA_META_Cursor receive path. The meta block is owned by the PipeWire
//! buffer; we copy out a fully-owned `CursorUpdate` so the value can cross
//! the mpsc boundary without lifetime entanglement.

#![cfg(target_os = "linux")]

/// `pipewire::spa::buffer::Buffer::find_meta(MetaType)`-like accessor, but
/// hand-rolled because libspa-rs 0.9.2 does NOT expose a Rust Meta wrapper
/// (verified via container probe: `target-docker/.../libspa-0.9.2/src/buffer/mod.rs`
/// contains zero `Meta`/`spa_meta` references).
pub(crate) const SPA_META_CURSOR: u32 = 5; // libspa-0.2 meta.h:46

/// Owned cursor snapshot. Mirrors `ControlMessage::CursorUpdate` payload
/// but lives in the `media-linux` crate so the `protocol` crate has no
/// Linux-specific knowledge.
pub struct CursorUpdate {
    pub id: u32,                   // 0 if invalid (no new metadata)
    pub position_x: i32,
    pub position_y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// `None` when bitmap_offset == 0 (reuse previous bitmap).
    /// `Some(empty Vec)` when bitmap is present but invisible (offset == 0 in spa_meta_bitmap).
    /// `Some(bgra)` otherwise; pixel data is BGRA8 (raw kept; viewer handles format).
    pub bitmap: Option<CursorBitmap>,
}

pub struct CursorBitmap {
    pub format: u32,  // spa_video_format; 0 == invalid, 7 == BGRA (most common)
    pub width: u32,
    pub height: u32,
    pub stride: i32,
    pub bgra: Vec<u8>,  // size = stride * height; tightly-packed when stride==width*4
}

#[derive(Debug, thiserror::Error)]
pub enum CursorMetaError {
    #[error("no SPA_META_Cursor on buffer")]
    Absent,
    #[error("meta size {0} smaller than spa_meta_cursor")]
    MetaTooSmall(u32),
    #[error("bitmap_offset {0} out of bounds (meta size {1})")]
    BitmapOffsetOutOfBounds(u32, u32),
    #[error("bitmap size {0}x{1} exceeds cap 256x256")]
    BitmapTooLarge(u32, u32),
    #[error("unsupported bitmap format {0} (expected BGRA=7)")]
    UnsupportedFormat(u32),
}

/// Read SPA_META_Cursor from a `pipewire::spa::buffer::Buffer`. Returns
/// `Ok(Some(_))` only when `cursor.id != 0` (libspa convention: id 0 == no
/// new data). Returns `Ok(None)` for "metadata present but stale" so the
/// caller can skip emitting a wire message without treating absence as an
/// error.
///
/// # Safety
///
/// The function performs unsafe pointer reads into the SPA meta block.
/// `buf` MUST be a valid `&mut pipewire::spa::buffer::Buffer` whose lifetime
/// extends across the call (i.e. obtained from `stream.dequeue_buffer()`
/// and not yet released back to the pool).
pub(crate) unsafe fn read_meta_cursor(
    buf: &pipewire::spa::buffer::Buffer,
) -> Result<Option<CursorUpdate>, CursorMetaError> {
    // 1. Iterate buf.as_raw().metas[0..n_metas]; locate the entry with
    //    meta.type_ == SPA_META_CURSOR. If none, return Absent.
    // 2. Bounds-check: meta.size >= sizeof(spa_meta_cursor). If not,
    //    return MetaTooSmall.
    // 3. Read spa_meta_cursor fields (id, position, hotspot, bitmap_offset).
    //    If id == 0, return Ok(None).
    // 4. If bitmap_offset == 0, return Ok(Some(CursorUpdate { bitmap: None, … })).
    // 5. Else bounds-check bitmap_offset >= sizeof(spa_meta_cursor)
    //    && bitmap_offset + sizeof(spa_meta_bitmap) <= meta.size.
    // 6. Read spa_meta_bitmap at meta.data + bitmap_offset. If
    //    bitmap.format == 0, return Ok(Some(CursorUpdate { bitmap: None, … })).
    // 7. Cap check: bitmap.size.width <= 256 && height <= 256;
    //    else BitmapTooLarge (caller silent-truncates to 256×256).
    // 8. Bitmap.offset == 0 → cursor invisible → CursorBitmap with bgra=Vec::new().
    // 9. Else pixel_ptr = (meta.data + bitmap_offset + bitmap.offset);
    //    pixel_len = stride * height (clamped). memcpy into owned Vec.
    todo!("implement in T1")
}
```

### 3.3 Stream listener wire-up (`crates/media-linux/src/wayland_portal/stream.rs`)

Existing `process()` callback's body extends to drain *both* video data AND cursor metadata from each buffer (they ride on the same SPA buffer):

```rust
.process(move |stream, _ud| {
    let Some(mut buf) = stream.dequeue_buffer() else { return };

    // 1. Cursor metadata path — drain FIRST so a video processing error
    //    doesn't lose cursor updates. Safe even on stale buffers.
    //
    // SAFETY: buf is owned by the dequeue_buffer scope; read_meta_cursor
    // performs no allocations beyond the bitmap clone (which happens
    // before buf is released back to the pool).
    match unsafe { crate::wayland_portal::cursor::read_meta_cursor(&buf) } {
        Ok(Some(c)) => {
            // try_send: drop on Full (rare; cursor channel cap=8 absorbs
            // bursts at 250Hz).
            let _ = cursor_tx_cb.try_send(c);
        }
        Ok(None) => {} // id==0; skip
        Err(crate::wayland_portal::cursor::CursorMetaError::Absent) => {} // expected on first frames
        Err(e) => tracing::warn!(error=%e, "cursor meta parse failed"),
    }

    // 2. Existing video data dispatch (P5B-2a path, unchanged).
    let datas = buf.datas_mut();
    let Some(d) = datas.first_mut() else { return };
    // … DataPath::DmaBuf / MemFd / MemPtr arms unchanged …
})
```

`PipeWireStream::connect()` signature gains a second mpsc:

```rust
pub fn connect(
    fd: OwnedFd,
    node_id: u32,
    frame_buf_cap: usize,
    cursor_buf_cap: usize,         // NEW; default 8
) -> Result<(Self, mpsc::Receiver<RawFrame>, mpsc::Receiver<CursorUpdate>), PipeWireStreamError>
```

### 3.4 Wire protocol (`crates/protocol`)

**`Hello.protocol_version: u8`** bumped `3 → 4` everywhere it's referenced:
- `crates/transport/src/handshake.rs:61` — `HELLO_PROTOCOL_VERSION: u8 = 4;`
- `crates/host/src/auth.rs:25` — `PROTOCOL_VERSION_REQUIRED: u8 = 4;`
- `crates/host/tests/auth_integration.rs:268-283` — version-mismatch test stays valid (just exercises a different reject path)

**Hard-bump behaviour** is documented: a v3 viewer connecting to a v4 host receives `HelloReject { code: ProtocolVersionMismatch }` (existing handshake.rs:222 path); a v4 viewer connecting to a v3 host likewise. Operators must upgrade both sides. This matches the v2→v3 transition convention from the OpenH264 swap.

**New `ControlMessage::CursorUpdate`** at `kind_u8 = 18`:

```rust
ControlMessage::CursorUpdate {
    /// Frame-stable cursor id from `spa_meta_cursor.id`. 0 is never sent
    /// (host filters out id==0 == "no new metadata"). Viewer uses id to
    /// detect cursor identity changes for bitmap-cache invalidation.
    id: u32,
    /// Cursor hotspot position in capture-region-relative LOGICAL pixels.
    /// Origin is the top-left of the capture region (the `host_monitor_rect`
    /// from `HelloAck`). Coordinates may be outside the region (cursor at
    /// the edge); viewer clamps for compositing.
    position_x: i32,
    position_y: i32,
    /// Hotspot offset within the bitmap (where the click happens). Only
    /// meaningful when `bitmap` is `Some(..)` or the cached bitmap is valid.
    hotspot_x: i32,
    hotspot_y: i32,
    /// `None` when this is a position-only update (reuse cached bitmap by id).
    /// `Some { width: 0, height: 0, .. }` when the cursor is invisible (host should
    /// hide cursor compositing; do NOT show OS-native cursor in capture region).
    /// `Some { width, height, bgra }` otherwise; `bgra.len() == width * height * 4`.
    bitmap: Option<CursorBitmap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBitmap {
    pub width: u16,    // ≤ 256 (silent-truncated at host)
    pub height: u16,   // ≤ 256
    pub bgra: Vec<u8>, // tightly packed (host re-strides if needed)
}
```

**`kind_u8`** edit:
- Add `Self::CursorUpdate { .. } => 18` to the `kind_u8()` match in `control.rs:243-…`
- `wire.rs:659` upper-bound stays at 22 (18 is well below); no decode-bound bump needed.
- New tests: `cursor_update_round_trip` + (existing) `control_all_kinds_round_trip` auto-covers via match exhaustiveness.

### 3.5 Host → viewer plumbing

The Linux host's `prdt-host` binary already has a `ControlMessage` send channel feeding the viewer's read loop. P5B-2b adds:

1. `WaylandPortalCapturer::new(...)` returns `(producer, cursor_rx)` instead of just `producer`.
2. Host main loop spawns a `tokio::task` per session that reads `cursor_rx` and forwards each `CursorUpdate` onto the existing `ControlMessage` send channel.
3. On `cursor_rx` close (PipeWire stream ended), the forwarder task exits cleanly.

The Windows host has no cursor-metadata source (DXGI capture path always embeds the cursor); `prdt-host` on Windows sends NO `CursorUpdate` messages. Viewers receiving zero cursor updates fall back to the embedded-cursor display path (just render the frame as-is).

### 3.6 Viewer-side compositing

**Two distinct paths** because the Windows + Linux viewer renderers differ:

| Platform | Renderer | Cursor composite approach |
|---|---|---|
| Windows | D3D11 + `Nv12Renderer::VideoProcessorBlt` | New `D3D11Texture2D` for cursor BGRA; render with a tiny pixel shader **after** `VideoProcessorBlt` writes the YUV-converted frame to the swapchain backbuffer, **before** `IDXGISwapChain1::Present` |
| Linux | softbuffer (CPU framebuffer) | CPU alpha-blend the cursor bitmap into `scratch_bgra` after `i420_to_bgra` writes the decoded frame; before `softbuffer::Surface::buffer_mut().present()` |

**Shared logic** in `crates/viewer/src/cursor_state.rs` (new):
- `struct CursorState { id: u32, position_x: i32, position_y: i32, hotspot_x: i32, hotspot_y: i32, bitmap: Option<Arc<CursorBitmap>> }`
- `pub fn apply(&mut self, msg: ControlMessage::CursorUpdate)` updates position always, replaces `bitmap` only when the message's `bitmap` is `Some`, identifies bitmap-cache hits by `msg.id == self.id`.
- `pub fn visible(&self) -> bool` — `false` if `bitmap` is `Some` with `width == 0 || height == 0` (host signalled "hide").
- `pub fn composite_target(&self, frame_width: u32, frame_height: u32) -> Option<CompositeTarget>` — returns the `(top_left_x, top_left_y, width, height)` rectangle to blit into the frame buffer; `None` when invisible or out-of-bounds.

The Windows-specific GPU path lives in `crates/media-win/src/d3d11/cursor_overlay.rs` (new); Linux CPU path lives in `crates/viewer/src/platform/linux.rs` (extend existing `present_frame`).

### 3.7 OS-native cursor hide

When `negotiated_version == 4` AND the viewer window has focus AND the mouse is within the rendered frame region:

- **Windows**: `SetCursor(NULL)` on `WM_SETCURSOR`. Restore on `WM_KILLFOCUS` / mouse-leave.
- **Linux** (winit-backed viewer): `window.set_cursor_visible(false)`. Restore on `WindowEvent::Focused(false)` / `CursorLeft`.

Gemini-noted edge case: drag-out beyond the frame region must restore the OS cursor immediately, else the user "loses" the cursor outside the viewer. Guard by re-checking visibility on every `CursorMoved` event.

## 4. State machine — host-side cursor frame production

```
                ┌─ id == 0 (no new cursor) ─────────→ skip (no wire msg)
                │
                ├─ id != 0, bitmap_offset == 0 ────→ CursorUpdate { bitmap: None }
                │                                     (viewer reuses cached bitmap)
                │
                ├─ id != 0, bitmap.format == 0 ────→ CursorUpdate { bitmap: None }
                │                                     (compositor said "ignore bitmap")
                │
                ├─ id != 0, bitmap.offset == 0 ────→ CursorUpdate { bitmap: Some(empty) }
                │                                     (cursor is invisible)
                │
                └─ id != 0, full bitmap present ──→ CursorUpdate { bitmap: Some(bgra) }
```

## 5. Compositor matrix

| Compositor | OS reference | Mode-4 stable since | RestoreToken durable? | Verified by |
|---|---|---|---|---|
| GNOME mutter | Ubuntu 22.04+, Fedora 36+ | mutter 42 (2022) | ✅ yes | walkthrough §G |
| KDE kwin | Ubuntu 24.04 (Kubuntu), Fedora 39+ | kwin 5.27 + xdg-desktop-portal-kde 5.27+ | ⚠️ session-scoped on some versions | walkthrough §H |
| Sway / Hyprland / wlroots-* | — | — | — | **descoped to P5C** |

The walkthrough document (`docs/superpowers/p5b1-smoke-walkthrough.md`) gains new sections §G (GNOME cursor metadata) and §H (KDE cursor metadata) with step-by-step verification + expected log lines.

## 6. Out of scope

- **Sway / Hyprland / wlroots smoke matrix** — descoped to P5C.
- **Cursor bitmap chunking** — 256×256 cap suffices for 4K HiDPI cursors; >256 silent-truncates.
- **Cursor cache invalidation across sessions** — viewer flushes cache on `HelloAck`.
- **Animated cursors** (mouse-throbber) — host sends each frame as a new `id`; viewer renders the latest. No frame-blending logic.
- **Cursor on Windows host** — DXGI path keeps Embedded-style cursor (always baked into frame). Windows host sends zero `CursorUpdate` messages.
- **Graceful v3↔v4 fallback** — strict version match preserved (existing convention); operators upgrade both sides.
- **Explicit sync** for cursor pixel data — implicit sync only (cursors are RGBA8 in shared mem, not DMABUF tiles).

## 7. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | KDE `RestoreToken` not durable; cursor probe re-runs each session | Already covered by existing P5B-1 token regeneration path; cursor probe is idempotent |
| 2 | mutter sends `spa_meta_bitmap.format != BGRA` (e.g. ARGB on some HW) | T1 rejects with `UnsupportedFormat`; viewer skips cursor compositing for that frame and logs |
| 3 | Cursor coordinate space differs (logical vs physical px on HiDPI) | Host sends capture-region-relative logical px; viewer scales to its render rect |
| 4 | `read_meta_cursor` UB if buffer is freed mid-process callback | Buffer scope ends at end of process closure (P5B-1 already drains synchronously); pointer never escapes the callback |
| 5 | `cursor_tx` channel back-pressure drops position updates | mpsc cap=8 with `try_send` drop-newest semantics; position interpolation NOT implemented (drop tolerable at 250Hz) |
| 6 | v3 viewer fails to connect to v4 host with cryptic `ProtocolVersionMismatch` | STATUS doc + viewer error message both reference the bump explicitly |
| 7 | Viewer crash on `CursorUpdate` from a malicious host with `bitmap.bgra.len() != width*height*4` | Decode-time validator rejects; protocol test covers |

## 8. Auto-evidence DoD

- `prdt-media-linux` clippy clean (`-D warnings`)
- `prdt-protocol` clippy clean
- `prdt-viewer` (and `crates/viewer-overlay`) clippy clean for non-GUI builds
- Affected-crate slice lib tests pass: `protocol + media-core + media-sw + media-policy + media-linux + transport + host + viewer-overlay`
- New unit tests:
  - `cursor::tests::read_meta_cursor_returns_none_for_id_zero` (host-side parse)
  - `cursor::tests::read_meta_cursor_handles_position_only_update`
  - `cursor::tests::read_meta_cursor_truncates_oversize_bitmap`
  - `cursor::tests::read_meta_cursor_handles_invisible_cursor`
  - `wire::cursor_update_round_trip` (protocol wire round-trip)
  - `cursor_state::apply_position_only_uses_cached_bitmap`
  - `cursor_state::apply_bitmap_replaces_cache_on_new_id`
- X11 contract regression guard: `capture_source_contract` 3 pass / 1 ignored
- **Total**: ≥ 7 new tests on top of P5B-2a's 10.
- Real-machine smoke (GNOME + KDE) deferred to walkthrough §G/§H.

## 9. Ambiguities flagged for plan resolution

1. **CursorUpdate `id == 0` semantics** — spec says host never sends id 0. Plan must add an explicit assertion in the wire encoder so a future bug can't accidentally emit one.
2. **Bitmap stride handling** — spec sends tightly-packed BGRA; if libspa emits a stride > width*4 the host re-strides during the meta-read step. Plan must include a stride-stripping unit test.
3. **Cursor visibility hide vs OS-cursor restore race** — Gemini flagged the Windows `SetCursor(NULL)` getting overridden by modal dialogs. Plan adds a `WM_SETCURSOR` hook that re-asserts on every cursor-related event.
4. **`PipeWireStream::connect` signature change** — affects `WaylandPortalCapturer::new`. The existing callers in `policy.rs::LinuxSwFactory::create` need the new return tuple. Plan must touch policy.rs.
5. **Viewer cursor channel back-pressure** — spec picks cap=8 with try_send. If real-machine smoke shows drops, plan adds a follow-up to bump to a ring-buffer of size 1 (latest-only).

## 10. References

- **libspa C header**: `/usr/include/spa-0.2/spa/buffer/meta.h` (Debian bookworm, libspa-0.2-dev 0.3.65) — `spa_meta_cursor` at line 141, `spa_meta_bitmap` at line 122, `SPA_META_Cursor = 5` at line 46
- **ashpd 0.12.3**: `CursorMode::Metadata` at `src/desktop/screencast.rs:75`; `available_cursor_modes()` at line 501
- **libspa-rs 0.9.2**: NO Meta wrapper — bare-FFI required (verified container probe)
- **OBS Studio**: cursor parse reference at `obs-studio/plugins/linux-pipewire/pipewire.c` (search for `on_process` + `SPA_META_Cursor`)
- **Moonlight-Qt**: D3D11 cursor overlay at `app/streaming/video/base.cpp`
- **xdg-desktop-portal**: ScreenCast spec [`AvailableCursorModes`](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
- **Protocol surface report** (this session): `ControlMessage` defined at `crates/protocol/src/control.rs:119`; `HELLO_PROTOCOL_VERSION = 3` at `crates/transport/src/handshake.rs:61`; `PROTOCOL_VERSION_REQUIRED = 3` at `crates/host/src/auth.rs:25`; encode/decode at `crates/protocol/src/wire.rs:638-664`; kind=18 is the next free slot ≤ 22
