# P5B-2a: libspa pod + DMABUF zero-copy (Performance Core)

**Status:** Design
**Created:** 2026-05-12
**Predecessors:** P5B-1 successor (`phase-p5b1-t5-t6-pipewire-runtime-complete`, commit `545b818`)
**Successors:** P5B-2b (cursor metadata + multi-compositor smoke matrix), P5C (Linux HW codec)

---

## 1. Goal & DoD

### 1.1 Goal

Replace the two staged stubs in `crates/media-linux/src/wayland_portal/stream.rs` (`parse_video_format` returns `Err`, `build_format_params` returns empty `Vec`) with real implementations, and add a DMABUF receive path so PipeWire frames carrying `SPA_DATA_DmaBuf` are consumed via `mmap` of the dup'd FD with no CPU memcpy. Existing `SPA_DATA_MemFd` and `SPA_DATA_MemPtr` paths remain as fallback. Cursor mode stays Embedded; multi-compositor smoke matrix is P5B-2b.

### 1.2 Definition of Done

1. `parse_video_format` reads `SPA_PARAM_Format` POD objects and extracts `(width: u32, height: u32, format: PixelFormat, framerate: Option<Fraction>, modifier: Option<i64>)`. Unsupported MediaType / MediaSubtype / PixelFormat values surface as typed errors (no panic).
2. `build_format_params` returns one or more pods advertising:
   - Format: `BGRA` or `BGRx` (Choice::Enum<Id>); `BGRA` is the default.
   - Size: 320×240 to 7680×4320 range (Choice::Range<Rectangle>); default 1920×1080.
   - Framerate: 15/1 to 60/1 range (Choice::Range<Fraction>); default 60/1.
   - Modifier: `DRM_FORMAT_MOD_LINEAR (0)` and `DRM_FORMAT_MOD_INVALID (-1)` (Choice::Enum<i64>). Linear is the default. (Plain "modifier not specified" is also acceptable — see §4.3.)
3. The PipeWire stream's `process` callback gains a `match` on `SpaData::type_`:
   - `SPA_DATA_DmaBuf`: dup the FD with `F_DUPFD_CLOEXEC`, `mmap(PROT_READ, MAP_PRIVATE)` the full mapping size (`maxsize + mapoffset`), walk the BGRA region (offset + size from chunk), feed the rows into the existing channel-bound `RawFrame { data, width, height, stride, ts_us }`. No CPU memcpy in the callback (read direct from the mapped pointer; producer-side BGRA→I420 still copies).
   - `SPA_DATA_MemFd`: existing path (CPU memcpy into pool-owned `Vec<u8>`) — unchanged.
   - `SPA_DATA_MemPtr`: same shape as MemFd but from a `void*` already mapped by libpipewire — pull the slice from `Data::data()` and copy out. Adds robustness on compositors that prefer this over MemFd.
4. Implicit sync only. No `SPA_META_SyncTimeline` / `SPA_DATA_SyncObj` handling in P5B-2a (those are sparsely deployed; revisit if a compositor needs them).
5. DMABUF FD ownership is explicit: every dup is paired with a `Drop` impl that calls `munmap` + `close`. No leaks under abnormal exit (panic in callback).
6. Per-frame BGRA-region access uses the chunk's reported `offset` and `size` (clamped to `<= maxsize`). Stride > width*4 stays supported (Intel iGPU 64-byte alignment).
7. `cargo build` + `cargo test` + `cargo clippy --workspace --all-targets -- -D warnings` green inside the Debian-bookworm dev container. ≥ 6 new tests:
   - Pod round-trip (build → parse → assert fields match).
   - Pod parse rejects unsupported MediaType / format / missing size.
   - `MappedPlane::Drop` calls munmap (verified via test stub).
   - `map_dmabuf_plane` with a `memfd_create`-based FD round-trips a known byte pattern.
   - Listener picks the right arm for each `SpaData::type_` (table-driven test against a mock SpaData).
   - End-to-end mock: build params → simulate `param_changed` → producer's `current_size` reflects the parsed Rectangle.
8. The X11 / WSLg path remains unchanged: `cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal` and the X11 contract tests both green.
9. Ship strategy: P5A/P6/P5B-1 同等 — auto evidence (container tests + 2-stage review) + real-machine smoke deferred to a follow-up session.

### 1.3 Out of Scope (deferred)

- **Explicit sync** (`SPA_META_SyncTimeline + SPA_DATA_SyncObj` / sync_file / DRM timeline wait). Codex notes this is "newer environment dependent" — sparse compositor support as of 2026-05. Revisit when a target compositor needs it.
- **NV12 multi-plane**. P5C territory (HW encoder integration changes the producer-side path; multi-plane handling lands together).
- **EGL import / GPU readback / Vulkan**. P5C.
- **`/dev/dri/card0` direct ioctl**. We never touch DRM device nodes directly in P5B-2a; the dmabuf FD comes pre-allocated from the compositor via the portal handshake. No `card0` open required.
- **Cursor metadata (mode=4)**. P5B-2b.
- **Multi-compositor smoke matrix (GNOME / KDE / Sway / Hyprland)**. P5B-2b.

---

## 2. Background

### 2.1 What P5B-1 left at the boundary

`crates/media-linux/src/wayland_portal/stream.rs` (commit `03cf221`, refined in `bc84770`/`734e597`) ships with two staged stubs:

```rust
// parse_video_format
fn parse_video_format(_p: &Pod) -> Result<(u32, u32, PixelFormat), &'static str> {
    Err("T5 stub — libspa pod parse deferred")
}

// build_format_params
fn build_format_params() -> Vec<Vec<u8>> { Vec::new() }
```

`param_changed` calls `parse_video_format` and skips its format-validation branch when the stub returns Err — current_size is then read from per-frame chunk metadata if available, else falls back to 1920×1080 with a warn. `Stream::connect` calls `build_format_params` and passes an empty `&mut params: &mut [&Pod]` slice; the compositor picks its own default. GNOME and KDE typically land on BGRA in practice, which is fortunate but unprincipled — a future compositor that defaults to NV12 or refuses to negotiate would break us.

DMABUF advertising is also tied to negotiation: if the producer doesn't include a `VideoModifier` property in its EnumFormat object, the compositor falls back to MemFd / MemPtr. That's why P5B-1 sees CPU memcpy on every frame even on GNOME with full DMABUF support.

### 2.2 Why the order matters

Codex's review explicitly flagged: **fix the negotiation first, then DMABUF**. Without a real `build_format_params` that advertises modifier support, the compositor won't hand us a DMABUF — so the DMABUF path can't be tested. Without a real `parse_video_format` that surfaces the negotiated format/size, we can't validate what the compositor actually picked.

So P5B-2a ships them together: pod first (§3.1, §3.2), then DMABUF (§3.3), then the listener's per-`SpaData::type_` dispatch (§3.4).

### 2.3 The dmabuf model in 30 seconds

PipeWire delivers a `SpaBuffer` to the `process` callback. `SpaBuffer.datas` is a slice of `SpaData` entries — one per plane for multi-plane formats; one for BGRA. Each `SpaData` has:

- `type_`: `SPA_DATA_DmaBuf | SPA_DATA_MemFd | SPA_DATA_MemPtr`.
- `fd: i64`: the FD (DmaBuf, MemFd) or a `-1` for MemPtr.
- `data: *mut u8`: a pointer libpipewire has mapped for us when `STREAM_FLAG_MAP_BUFFERS` is set. **For DmaBuf this pointer is NOT automatically mmapped** by libpipewire — we must mmap the FD ourselves.
- `maxsize: u32`: the total mapped region size.
- `chunk.offset / chunk.size / chunk.stride`: per-frame valid region inside the mapping.
- `mapoffset: u32`: byte offset within the mapping where frame data starts (usually 0 for our case; non-zero for some shared mappings).

For BGRA single-plane: `datas.len() == 1`, `chunk.size == stride * height`, `data` points at the first byte of the frame (or `data + chunk.offset` if `chunk.offset > 0`).

---

## 3. Architecture

### 3.1 Module layout

| Path | Change | Responsibility |
|---|---|---|
| `crates/media-linux/src/wayland_portal/format.rs` | **NEW** | POD parse + build helpers. Owns `BuiltParams { bytes: Vec<Vec<u8>> }` with `as_pods(&self) -> Vec<&Pod>` (rebuilds the slice on each call — `Pod::from_bytes` is a borrow, so the byte storage must live as long as the `Pod` reference does). |
| `crates/media-linux/src/wayland_portal/dmabuf.rs` | **NEW** | `MappedPlane { fd: OwnedFd, ptr, len, data_off }` with `Drop` impl (munmap; `OwnedFd::Drop` closes the FD). `map_dmabuf_plane(d: &Data) -> io::Result<MappedPlane>` unsafe helper. |
| `crates/media-linux/src/wayland_portal/stream.rs` | **MODIFIED** | `parse_video_format` → real impl that delegates to `format::parse`. `build_format_params` → real impl that delegates to `format::build`. `process` callback gains the `SpaData::type_` dispatch. |
| `crates/media-linux/src/wayland_portal/mod.rs` | **MODIFIED** | Add `pub mod format;` + `pub mod dmabuf;`. Re-export `BuiltParams`, `PixelFormat`, `MappedPlane` if external test code needs them. |

No new crates. No new workspace deps. Existing `pipewire = "0.9"`, `libc`, `tracing`, `thiserror` already cover what's needed.

### 3.2 POD parse (`format::parse`)

`parse_video_format(p: &Pod) -> Result<NegotiatedFormat, ParseError>` where:

```rust
pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,        // BGRA | BGRx
    pub framerate: Option<Fraction>, // None if compositor didn't lock it
    pub modifier: Option<i64>,       // None if not advertised; DRM_FORMAT_MOD_*
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("pod is not an Object")]
    NotObject,
    #[error("pod type is not ParamFormat (got {0:?})")]
    WrongType(pipewire::spa::utils::SpaTypes),
    #[error("MediaType is not Video")]
    NotVideo,
    #[error("MediaSubtype is not Raw")]
    NotRaw,
    #[error("VideoFormat is not BGRA/BGRx (got id={0})")]
    UnsupportedFormat(u32),
    #[error("size missing")]
    MissingSize,
}
```

Implementation walks the `Object`'s `properties: Vec<Property>` and matches on `Property::key` against the relevant `FormatProperties::*` constants. Use `pipewire::spa::pod::deserialize::PodDeserializer` to read the object from raw bytes, or pattern-match on `Value::Object(obj)` if we have a deserialised `Value` in hand.

The actual call from `param_changed` looks like:

```rust
fn param_changed(stream: &Stream, _id: u32, _user_data: &mut UserData, param: Option<&Pod>) {
    let Some(p) = param else { return; };
    match crate::wayland_portal::format::parse(p) {
        Ok(neg) => {
            tracing::info!(w=neg.width, h=neg.height, fmt=?neg.format, modifier=?neg.modifier,
                "pipewire negotiated format");
            *current_size_cb.lock().unwrap() = (neg.width, neg.height);
            // Future P5B-2b: store negotiated cursor mode here too.
        }
        Err(e) => {
            tracing::warn!(error=%e, "pipewire format negotiation failed; aborting stream");
            stream.disconnect().ok();
        }
    }
}
```

### 3.3 POD build (`format::build`)

Single function `build() -> BuiltParams`. Body lifts Codex's verified sample (CCG advisor artifact `codex-p5b-2-*.md` lines 169-222) with minor adaptation:

```rust
pub fn build() -> BuiltParams {
    use pipewire::spa::{
        param::{format::{FormatProperties, MediaSubtype, MediaType}, video::VideoFormat, ParamType},
        pod::{ChoiceValue, Object, Pod, Property, Value},
        pod::serialize::PodSerializer,
        utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes},
    };
    let modifiers = vec![DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID];
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw()))),
            Property::new(FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw()))),
            Property::new(FormatProperties::VideoFormat.as_raw(),
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(VideoFormat::BGRA.as_raw()),
                        alternatives: vec![
                            Id(VideoFormat::BGRA.as_raw()),
                            Id(VideoFormat::BGRx.as_raw()),
                        ],
                    },
                )))),
            Property::new(FormatProperties::VideoSize.as_raw(), /* Range(320..7680, 240..4320) */),
            Property::new(FormatProperties::VideoFramerate.as_raw(), /* Range(15/1..60/1) */),
            Property::new(FormatProperties::VideoModifier.as_raw(),
                Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_FORMAT_MOD_LINEAR,
                        alternatives: modifiers,
                    },
                )))),
        ],
    };
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::<u8>::new()), &Value::Object(obj))
        .expect("pod serialise").0.into_inner();
    BuiltParams { bytes: vec![bytes] }
}
```

Constants:
```rust
const DRM_FORMAT_MOD_LINEAR: i64 = 0;
const DRM_FORMAT_MOD_INVALID: i64 = -1;
```

`Stream::connect` site adapts:
```rust
let params = format::build();
let pod_refs = params.as_pods();
let mut param_slice: Vec<&Pod> = pod_refs.iter().copied().collect();
stream.connect(direction, Some(node_id), flags, &mut param_slice)?;
// `params` and `pod_refs` must outlive the connect call.
```

The `BuiltParams { bytes }` ownership pattern is Codex's MEDIUM-flag avoidance: `Pod::from_bytes` borrows the bytes, so we keep the `Vec<Vec<u8>>` alive on the stack and rebuild `&Pod` references each call. **No `Vec<Pod>` returned anywhere.**

### 3.4 DMABUF receive path (`dmabuf::map_dmabuf_plane`)

```rust
use std::{io, os::fd::{FromRawFd, OwnedFd}};
use pipewire::spa::buffer::Data;

pub struct MappedPlane {
    fd: OwnedFd,             // dup'd FD; dropped on close
    ptr: *mut u8,
    len: usize,
    data_off: usize,
}

unsafe impl Send for MappedPlane {}

impl Drop for MappedPlane {
    fn drop(&mut self) {
        // SAFETY: ptr / len pair came from a successful mmap, never aliased.
        unsafe { libc::munmap(self.ptr.cast(), self.len); }
        // OwnedFd::Drop closes fd separately.
    }
}

impl MappedPlane {
    /// Slice of the valid plane bytes, from data_off to end of mapping.
    /// Callers should clamp further to chunk.offset + chunk.size for the
    /// per-frame valid region.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: ptr is non-null, len-data_off bytes are mapped read-only.
        unsafe { std::slice::from_raw_parts(self.ptr.add(self.data_off), self.len - self.data_off) }
    }
}

pub unsafe fn map_dmabuf_plane(d: &Data) -> io::Result<MappedPlane> {
    let raw = d.as_raw(); // spa_data — pipewire-rs 0.9 doesn't expose .fd() yet.
    if raw.fd < 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "spa_data has no fd"));
    }
    // F_DUPFD_CLOEXEC so the FD outlives the callback safely.
    // SAFETY: raw.fd is a valid open FD inside the callback; dup is always safe.
    let dupfd = unsafe { libc::fcntl(raw.fd as i32, libc::F_DUPFD_CLOEXEC, 3) };
    if dupfd < 0 { return Err(io::Error::last_os_error()); }

    // Map maxsize + mapoffset starting at offset 0 of the FD.
    let map_len = raw.maxsize as usize + raw.mapoffset as usize;
    // SAFETY: dupfd is valid for read; PROT_READ + MAP_PRIVATE is safe even if
    // backing object is dmabuf (kernel handles the copy semantics).
    let ptr = unsafe {
        libc::mmap(std::ptr::null_mut(), map_len, libc::PROT_READ, libc::MAP_PRIVATE, dupfd, 0)
    };
    if ptr == libc::MAP_FAILED {
        unsafe { libc::close(dupfd); }
        return Err(io::Error::last_os_error());
    }
    Ok(MappedPlane {
        fd: unsafe { OwnedFd::from_raw_fd(dupfd) },
        ptr: ptr.cast(),
        len: map_len,
        data_off: raw.mapoffset as usize,
    })
}
```

The function is `unsafe` because the caller asserts `d` is a `SPA_DATA_DmaBuf`-typed Data with a valid FD. Internally every libc call has a `// SAFETY:` rationale per project convention.

### 3.5 Listener dispatch

`process` callback's body becomes a `match` on `SpaData::type_`:

```rust
fn process(stream: &Stream, _user_data: &mut UserData) {
    let Some(mut buf) = stream.dequeue_buffer() else { return; };
    let datas = buf.datas_mut();
    let Some(d) = datas.first_mut() else { return; };
    let chunk = d.chunk();
    let stride = chunk.stride() as u32;
    let size = chunk.size() as usize;
    let offset = chunk.offset() as usize;
    let (w, h) = *current_size_cb.lock().unwrap();
    if w == 0 || h == 0 || stride == 0 || size == 0 { return; }

    // SAFETY: as_raw on a DataRef is always safe; type_ is an enum tag.
    let dtype = unsafe { d.as_raw().type_ };

    let result: Result<RawFrame, FrameError> = match dtype {
        x if x == pipewire_sys::SPA_DATA_DmaBuf => {
            // SAFETY: type_ branch confirms this is a DMABUF Data.
            match unsafe { dmabuf::map_dmabuf_plane(d) } {
                Ok(mapped) => {
                    let frame_bytes = &mapped.bytes()[offset..offset + size];
                    Ok(RawFrame::from_slice(frame_bytes, w, h, stride))
                    // (RawFrame internally stores a Vec<u8> — we still copy
                    // *once* here from the mapped region into the channel-
                    // owned buffer. The "zero-copy" claim is that the
                    // compositor never serialised the frame into a memfd
                    // and we never copied the entire framebuffer through
                    // an intermediate. The single read-side memcpy into a
                    // pool buffer remains.)
                }
                Err(e) => {
                    tracing::warn!(?e, "dmabuf mmap failed; dropping frame");
                    return;
                }
            }
        }
        x if x == pipewire_sys::SPA_DATA_MemFd || x == pipewire_sys::SPA_DATA_MemPtr => {
            // Existing path: libpipewire pre-mapped data for us when
            // STREAM_FLAG_MAP_BUFFERS is set. Just read d.data() as a slice.
            let Some(src) = d.data() else { return; };
            Ok(RawFrame::from_slice(&src[offset..offset + size], w, h, stride))
        }
        _ => {
            tracing::warn!(type_ = dtype, "unsupported SpaData type; dropping frame");
            return;
        }
    };

    if let Ok(frame) = result {
        match tx_cb.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => { /* drop-on-full */ }
            Err(mpsc::error::TrySendError::Closed(_)) => { /* producer hung up */ }
        }
    }
}
```

`RawFrame::from_slice(bytes, w, h, stride)` is a new helper that takes a pool-acquired `Vec<u8>` from `FramePool`, fills it from the slice, sets the metadata fields. Existing pool semantics unchanged.

### 3.6 Stream flags

`stream.connect(...)` keeps `StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS`. `MAP_BUFFERS` causes libpipewire to auto-map MemFd / MemPtr Datas (so `Data::data()` is non-null for those types), but for DMABUF the auto-map is skipped — we mmap ourselves. This is the documented behaviour (xdg-desktop-portal-wlr does the same — see `pipewire_screencast.c:424`).

---

## 4. Decisions

### 4.1 No NV12 multi-plane in P5B-2a

Per AskUserQuestion answer. Reasoning:

- Codex flagged NV12 as a "罠" — plane-per-fd / plane-per-modifier / plane-per-stride handling is significant complexity.
- BGRA is what all four target compositors (GNOME / KDE / Sway / Hyprland) default to or are willing to negotiate.
- NV12 has real value once the HW encoder lands (P5C) — it avoids one BGRA→I420 conversion per frame.
- Punting NV12 to P5C keeps P5B-2a's scope bounded.

### 4.2 Implicit sync only

Per CCG (both advisors). `SPA_META_SyncTimeline` + `SPA_DATA_SyncObj` are the explicit-sync path; they're sparsely deployed in 2026-05 and add real complexity (timeline reads, fence FD ownership, `DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT`). For BGRA single-plane on PROT_READ mmap, the kernel's dmabuf cache-coherency semantics handle the case correctly in practice — the compositor flushes the GPU before signalling the buffer ready event. If a future compositor needs explicit sync to avoid tearing, P5B-3 (or later) extends `dmabuf.rs` with a sync-aware helper.

### 4.3 Modifier negotiation strategy

`build_format_params` advertises BOTH `DRM_FORMAT_MOD_LINEAR (0)` and `DRM_FORMAT_MOD_INVALID (-1)`. The latter is what compositors pick when they want to hand us tiled data with no published modifier — Codex flagged this is "NOT linear guaranteed". If the compositor picks `MOD_INVALID`, the listener accepts it (sets `negotiated.modifier = Some(-1)`) and falls back to MemFd / MemPtr on the produce side instead of mmapping the DMABUF (which would be `unsafe`-but-broken for tiled data). 

Concretely: in §3.5's `process` callback, if `dtype == SPA_DATA_DmaBuf && negotiated.modifier == Some(DRM_FORMAT_MOD_INVALID)`, log a one-time warn ("compositor produced DMABUF with MOD_INVALID; cannot CPU-read tiled data; reconnecting with MemFd preference") and call `stream.disconnect()` to trigger a renegotiation. We can request a second `connect()` with a narrower modifier list (LINEAR only) — that's a follow-up if any compositor actually hits this path in smoke. **For P5B-2a, the fallback is graceful disconnect + log; the renegotiation auto-retry is a P5B-2a follow-up TODO.**

### 4.4 Build-time / runtime feature flag?

No. P5B-1 deferred pipewire entirely because Ubuntu 22.04 couldn't build it; P5B-1 successor re-added it. P5B-2a adds no new system-lib dependency (still `libpipewire-0.3-dev` + `libspa-0.2-dev` ≥ 0.3.55, same as P5B-1 successor). The dev container script handles the build env; the production binary depends only on what's already declared.

### 4.5 Cargo MSRV

Workspace stays at 1.85. No deps change. The libspa pod builder is in `pipewire 0.9.x` which we already use.

---

## 5. Testing strategy

### 5.1 Unit tests (run inside the dev container)

1. `format::tests::round_trip_bgra` — `build()` → `as_pods()` → `parse()` → assert format=BGRA, default size=1920×1080.
2. `format::tests::parse_rejects_non_video_media_type` — hand-construct a POD with `MediaType::Audio`, expect `ParseError::NotVideo`.
3. `format::tests::parse_rejects_unsupported_format` — POD with `VideoFormat::NV12` (a format we don't accept in P5B-2a), expect `ParseError::UnsupportedFormat(NV12::as_raw())`.
4. `format::tests::parse_extracts_modifier` — POD with explicit `VideoModifier(0)` (linear), expect `parsed.modifier == Some(0)`.
5. `dmabuf::tests::mapped_plane_drop_calls_munmap` — construct a `MappedPlane` from a `memfd_create` FD with known bytes; verify `bytes()` returns the right pattern; on drop, mmap region is unmapped (verified via `madvise(MADV_DONTNEED)` returning `EFAULT` on a stale ptr — a portable invariant probe).
6. `dmabuf::tests::map_dmabuf_plane_dups_fd` — set up a `memfd_create` + `ftruncate` + `write` of "P5B2A" pattern. Construct a fake `Data` via `pipewire::spa::buffer::Data`-test-builder (or `as_raw()` cast on a zeroed `spa_data` filled with our fd / sizes). Call `map_dmabuf_plane`. Assert `bytes()[0..5] == b"P5B2A"`. Verify the original fd is still open after `MappedPlane::drop()` (because dup).
7. `stream::tests::listener_dispatch_table` — table-driven, not requiring a real PipeWire daemon: a stub that simulates `process` callback receiving a `Data` of each type tag (`SPA_DATA_DmaBuf` / `MemFd` / `MemPtr`) and verifies the right code arm fires (via a per-arm counter Arc<AtomicU32>). Mock the `tx_cb.try_send` so the listener can run without a tokio runtime.

If wiring tests #6 / #7 against the real `Data` type is too painful (the `spa_data` struct may not be publicly-constructable in pipewire-rs 0.9), make `map_dmabuf_plane` take a thin trait `SpaDataLike { fn fd(&self) -> i32; fn maxsize(&self) -> u32; fn mapoffset(&self) -> u32; }` plus a blanket `impl<'a> SpaDataLike for &'a Data`, and test against a hand-written `struct TestData`. **The plan author judges this acceptable** — production behaviour is identical; test surface gains a trait.

### 5.2 Property test (optional)

If `proptest` is already on the dev-deps (it is — see `crates/transport/Cargo.toml`), add one property test: `forall (w, h, stride) in (320..7680, 240..4320, w*4..w*4+128): build_then_parse_preserves_size`. Cheap and catches off-by-one in the Choice::Range encoding.

### 5.3 Smoke walkthrough (deferred to follow-up session)

The auto-evidence ship strategy means real-compositor smoke is documented but not gated. Append a `## P5B-2a` section to `docs/superpowers/p5b1-smoke-walkthrough.md`:

- **Section D — GNOME DMABUF smoke**: on Ubuntu 24.04 GNOME, start the host with `RUST_LOG=debug`. Expect the negotiation log to show `format=BGRA modifier=Some(0)` (linear). Expect the per-frame dispatch log (if added) to show `SPA_DATA_DmaBuf` arms firing. CPU usage should drop noticeably vs P5B-1 successor's MemFd path under sustained 60fps 1080p capture.
- **Section E — MemFd fallback regression**: on a compositor that doesn't advertise DMABUF (e.g. an older xdg-desktop-portal version), verify the listener still fires the `MemFd` arm and frames continue to flow.

These sections sit as instructions for the next session; current-session DoD doesn't require them executed.

### 5.4 Regression bar

- `./scripts/dev-container.sh cargo build --workspace --target x86_64-unknown-linux-gnu` — clean.
- `./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings` — clean.
- `./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu` — green except pre-existing flaky `transport::probe_test::two_transports_find_each_other`.
- `cargo test -p prdt-media-linux --test capture_source_contract` — X11 contract test still passes (regression guard for P5B-1 T1 trait).

---

## 6. Risks & mitigations

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| 1 | pipewire-rs 0.9.2 `Pod::from_bytes` borrow lifetime breaks the `BuiltParams::as_pods()` returning `Vec<&Pod>` if not careful | HIGH | Codex flagged this. The `BuiltParams` owns `Vec<Vec<u8>>`; `as_pods` returns a fresh `Vec<&Pod>` whose borrows tie to `&self`. Annotate with explicit lifetimes. Test compiles. |
| 2 | `spa_data.fd` accessor is missing from pipewire-rs 0.9 high-level API; we drop to `as_raw().fd` via `unsafe` | HIGH | Codex flagged. Document in `dmabuf.rs` module-level comment with link to https://docs.rs/pipewire/0.9/pipewire/spa/buffer/struct.Data.html (and the upstream issue, if findable). `// SAFETY:` comment on each `as_raw` usage. |
| 3 | `DRM_FORMAT_MOD_INVALID` doesn't imply linear → CPU mmap of tiled data produces garbage | HIGH | §4.3's strategy: detect MOD_INVALID at negotiation, log warn, disconnect + future renegotiation TODO. Real-machine smoke (deferred) verifies the actual behaviour. |
| 4 | Tearing under implicit sync on certain Intel iGPU configurations | MEDIUM | CCG flagged but rare. Acceptable trade-off for P5B-2a. P5B-3 can add explicit sync if a target machine hits it. |
| 5 | `OwnedFd::from_raw_fd` semantics — accidentally closing the fd before the mmap region is unmapped causes UB | HIGH | The `MappedPlane` struct holds both. `Drop` order is field-declaration order → `fd` is declared FIRST, so `munmap` (in our explicit Drop impl) runs BEFORE `OwnedFd::drop` (auto-generated). Verified by test #5. |
| 6 | Test wiring is painful because `spa_data` isn't pub-constructable in pipewire-rs 0.9 | MEDIUM | §5.1 fallback: introduce `trait SpaDataLike` for testability. Production type still implements it via blanket impl on `&Data`. Trivial cost. |
| 7 | `param_changed` callback panics on a malformed POD from a buggy compositor | LOW | `parse` returns `Result`, `param_changed` matches on it, `stream.disconnect()` on Err. No panic surfaced to the mainloop. |

---

## 7. Implementation outline

(Not a TDD breakdown — that's the plan's job.)

1. **T1 — `format.rs` module + POD build**. `build() -> BuiltParams` returning BGRA/BGRx + size/framerate ranges + modifier enum. `as_pods(&self) -> Vec<&Pod>`. ~3 tests.
2. **T2 — POD parse**. `parse(&Pod) -> Result<NegotiatedFormat, ParseError>` walking the Object. ~3 tests (round-trip, wrong type, unsupported format).
3. **T3 — `dmabuf.rs` module + `MappedPlane`**. `map_dmabuf_plane(&Data) -> io::Result<MappedPlane>` + `MappedPlane::Drop`. Test via `memfd_create`. ~2-3 tests.
4. **T4 — Stream listener dispatch**. `process` callback gets the `SpaData::type_` match. Use `format::parse` from `param_changed`. Use `dmabuf::map_dmabuf_plane` from the DmaBuf arm. ~2 tests (dispatch table; integration).
5. **T5 — STATUS + smoke walkthrough doc**. Append Section D + E to the existing smoke walkthrough. Update `STATUS.md` with the `phase-p5b2a-libspa-pod-dmabuf-complete` entry. No code.

5 tasks total (smaller than P5B-1's 8 because the structural scaffolding is already there).

---

## 8. References

- xdg-desktop-portal ScreenCast: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
- xdg-desktop-portal-wlr `pipewire_screencast.c` (modifier list + DMABUF fallback): https://github.com/emersion/xdg-desktop-portal-wlr/blob/master/src/screencast/pipewire_screencast.c
- OBS Studio `linux-pipewire/pipewire.c` (format negotiation + DMABUF + cursor metadata): https://github.com/obsproject/obs-studio/blob/master/plugins/linux-pipewire/pipewire.c
- GNOME Remote Desktop `grd-rdp-pipewire-stream.c` (timeline sync / dup): https://github.com/GNOME/gnome-remote-desktop/blob/master/src/grd-rdp-pipewire-stream.c
- Kernel `dma-buf` CPU access semantics: https://docs.kernel.org/driver-api/dma-buf.html
- CCG synthesis artifacts (this session):
  - `.omc/artifacts/ask/codex-p5b-2-wayland-portal-capture-ffi-dmabuf-sync-libspa-pod-buil-2026-05-12T05-52-57-686Z.md`
  - `.omc/artifacts/ask/gemini-p5b-2-wayland-portal-capture-ux-multi-compositor-scope-decom-2026-05-12T05-44-06-151Z.md`

---

## 9. Open questions (for the plan author)

- **Test injection for `SpaData`**: §5.1 #6 and #7 may need the `trait SpaDataLike` workaround. Plan author verifies at T3 / T4 by trying the direct approach first (just unsafe-construct a `spa_data` from zeroed bytes and cast to `&Data`); if that's impossible or unsafe-by-design, fall through to the trait. **Acceptable either way.**
- **`pipewire_sys::SPA_DATA_*` constants**: pipewire-rs may not re-export them at the `pipewire::` root. Plan author finds the right import path during T4 (likely `pipewire::sys::*` or `libspa_sys::*`). If neither works, define our own constants matching the C ABI (`SPA_DATA_DmaBuf = 4`, `SPA_DATA_MemFd = 2`, `SPA_DATA_MemPtr = 1` — verify against `/usr/include/spa-0.2/spa/buffer/buffer.h`).
- **`Choice::Range` of `Rectangle`**: `pipewire::spa::utils::Rectangle` is `Copy`. The Choice serialiser may want `&Rectangle` vs owned — try owned first.
- **Per-frame negotiation logging cadence**: T4's listener logs `info!` on every `param_changed`. If GNOME re-issues param_changed frequently (e.g. on monitor reconfigure), this could spam. Add a `Once`-guarded version if smoke shows spam, but don't pre-emptively gate.
