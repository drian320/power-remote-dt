# P5B-2a libspa pod + DMABUF zero-copy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the two T5 staged stubs in `crates/media-linux/src/wayland_portal/stream.rs` (`parse_video_format` returns `Err(...)`; `build_format_params` returns `Vec::new()`) with real libspa POD parse + build helpers, then add a DMABUF receive path so PipeWire frames carrying `SPA_DATA_DmaBuf` are consumed via `mmap(PROT_READ, MAP_PRIVATE)` of a `F_DUPFD_CLOEXEC`-dup'd FD with no CPU memcpy of the full framebuffer. Existing `SPA_DATA_MemFd` / `SPA_DATA_MemPtr` paths remain as fallback. Cursor mode stays Embedded; multi-compositor smoke matrix is P5B-2b.

**Architecture:** Two new sibling modules under `crates/media-linux/src/wayland_portal/`: `format.rs` owns the POD build/parse layer (`BuiltParams { bytes: Vec<Vec<u8>> }` with `as_pods(&self) -> Vec<&Pod>` to keep the byte storage alive across the borrow handed to `stream.connect`), and `dmabuf.rs` owns `MappedPlane { _fd: OwnedFd, ptr, len, data_off }` plus the `unsafe fn map_dmabuf_plane(d: &Data) -> io::Result<MappedPlane>` helper. `MappedPlane`'s struct field order — `_fd: OwnedFd` declared first — drives Drop order so the explicit `Drop` impl's `munmap` runs BEFORE `OwnedFd::drop` closes the FD. `stream.rs`'s `param_changed` callback delegates to `format::parse` (Err = `tracing::warn!` + `stream.disconnect()`); the `process` callback gains a `match` on `spa_data.type_` against `SPA_DATA_DmaBuf` / `SPA_DATA_MemFd` / `SPA_DATA_MemPtr`. The DMABUF arm calls `dmabuf::map_dmabuf_plane`, reads the chunk-bounded region directly from the mapped pointer, and feeds the existing `RawFrame { data, width, height, stride, ts_us }` channel. Implicit sync only (no `SPA_META_SyncTimeline`); BGRA/BGRx only (NV12 rejected as `ParseError::UnsupportedFormat`); modifier list is `[DRM_FORMAT_MOD_LINEAR (0), DRM_FORMAT_MOD_INVALID (-1)]`, and `MOD_INVALID` arrival logs a warn and disconnects (renegotiation auto-retry deferred).

**Tech Stack:** Rust 1.85, edition 2021. **No new workspace deps.** Reuse the existing `pipewire = "0.9"` POD builder (`pipewire::spa::pod::{serialize::PodSerializer, deserialize::PodDeserializer}`), existing `libc = "0.2"` for `mmap`/`munmap`/`fcntl`/`memfd_create`, existing `tracing` + `thiserror`. The pipewire system lib requirement is unchanged (`libpipewire-0.3 >= 0.3.55`); P5B-2a still builds inside the Debian-bookworm container via `scripts/dev-container.sh`.

**Spec:** `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` (commit `0e903e9`)

**Branch:** `phase-p5b2a-libspa-pod-dmabuf`

**Tag (on completion):** `phase-p5b2a-libspa-pod-dmabuf-complete`

**Predecessor:** P5B-1 successor tag `phase-p5b1-t5-t6-pipewire-runtime-complete` (commit `545b818`). The structural scaffolding in `wayland_portal/` is already in place; P5B-2a fills the two stubs and adds the DMABUF arm.

**Cross-platform regression bar:** Linux + Windows both green for `cargo build/clippy/test --workspace -- -D warnings` (matches L0–L4 + P5A + P6 + P5B-1 bar). Linux gates run inside the dev container (Ubuntu 22.04 host's libpipewire 0.3.48 < required 0.3.55):
- `./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings`
- `./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu`

Pre-existing flaky `transport::probe_test::two_transports_find_each_other` is not a P5B-2a regression.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `crates/media-linux/src/wayland_portal/format.rs` | POD parse + build helpers. `pub struct BuiltParams { bytes: Vec<Vec<u8>> }` with `as_pods(&self) -> Vec<&Pod>` (rebuilds the borrow slice each call so the byte storage outlives the `&Pod` references). `pub fn build() -> BuiltParams`. `pub fn parse(&Pod) -> Result<NegotiatedFormat, ParseError>`. Module-top constants `DRM_FORMAT_MOD_LINEAR: i64 = 0` and `DRM_FORMAT_MOD_INVALID: i64 = -1`. |
| `crates/media-linux/src/wayland_portal/dmabuf.rs` | `MappedPlane { _fd: OwnedFd, ptr, len, data_off }` with explicit `Drop` (munmap before the auto-Drop closes the FD — guaranteed by struct field-declaration order). `pub unsafe fn map_dmabuf_plane(d: &Data) -> io::Result<MappedPlane>`. Optional `trait SpaDataLike { fd, maxsize, mapoffset }` if pipewire-rs 0.9 doesn't expose constructable `Data` for tests. |

### Modified

| Path | Change |
|---|---|
| `crates/media-linux/src/wayland_portal/mod.rs` | Add `pub mod format;` + `pub mod dmabuf;`. Re-export `BuiltParams`, `NegotiatedFormat`, `ParseError`, `MappedPlane`. |
| `crates/media-linux/src/wayland_portal/stream.rs` | Replace `parse_video_format` body (was `Err("T5 stub — libspa pod parse deferred")`) with a delegate to `format::parse`. Replace `build_format_params` body (was `Vec::new()`) with `format::build()` + `as_pods()` plumbing on the connect site. Add a `match` on `spa_data.type_` in the `process` callback: DMABUF arm calls `dmabuf::map_dmabuf_plane`; MemFd / MemPtr arm preserves the existing `d.data()` path. MOD_INVALID detection logs warn + `stream.disconnect()`. |
| `docs/superpowers/STATUS.md` | Append `phase-p5b2a-libspa-pod-dmabuf-complete` entry after the P5B-1 successor entry; update `**Last updated**` and `**Latest tag**`. |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | Append `## P5B-2a` section with Section D (GNOME DMABUF smoke) + Section E (MemFd fallback regression). No new file — extends the existing walkthrough doc per spec §5.3. |

---

## Task list overview

| # | Task | Files | Tests |
|---|---|---|---|
| T1 | `format::build()` — POD build for `SPA_PARAM_EnumFormat` advertising BGRA/BGRx + size/framerate ranges + modifier enum. `BuiltParams { bytes }` + `as_pods(&self) -> Vec<&Pod>`. `parse_video_format` stub stays `Err` for now (T2 replaces). | `wayland_portal/format.rs` (new), `wayland_portal/mod.rs` | 1 new (`round_trip_bgra`) |
| T2 | `format::parse()` — walks the `Object`'s properties and extracts `NegotiatedFormat { width, height, format, framerate, modifier }`. Typed `ParseError` (NotObject / WrongType / NotVideo / NotRaw / UnsupportedFormat / MissingSize). | `wayland_portal/format.rs` | 4 new (round-trip BGRA, rejects non-Video, rejects NV12 → UnsupportedFormat, extracts modifier value) |
| T3 | `dmabuf::map_dmabuf_plane()` + `MappedPlane` — `mmap(PROT_READ, MAP_PRIVATE)` after `F_DUPFD_CLOEXEC`. Struct field-order drives Drop order. Tests use `memfd_create` + `ftruncate` + `write` to inject a known byte pattern. **Step 1 decides: try unsafe-construct of `spa_data` first; fall to `trait SpaDataLike` if not constructable.** | `wayland_portal/dmabuf.rs` (new), `wayland_portal/mod.rs` | 3 new (mmap + pattern read + drop; dup confirmed by original-fd-still-open-after-drop; MAP_FAILED → Err) |
| T4 | Stream listener dispatch — wire `parse_video_format` → `format::parse` and `build_format_params` → `format::build`. Add `match spa_data.type_` arm in `process` callback. **Step 2 finds the right import path for `SPA_DATA_*` constants** (`pipewire::sys::*` → `libspa_sys::*` → hand-define against C ABI). MOD_INVALID → warn + `stream.disconnect()`. | `wayland_portal/stream.rs` | 2 new (dispatch table per-arm counters via mocked SpaData; integration: build → connect-mock → `param_changed` → `current_size` updated) |
| T5 | STATUS + smoke doc + final gate — append Section D (GNOME DMABUF smoke) + Section E (MemFd fallback regression) to `p5b1-smoke-walkthrough.md`; append `phase-p5b2a-libspa-pod-dmabuf-complete` entry to `STATUS.md`; final container clippy + test green. No code changes. | `docs/superpowers/STATUS.md`, `docs/superpowers/p5b1-smoke-walkthrough.md` | (manual) |

**Total new automated tests: 10** (≥6 spec §1.2 #7 target met with margin).

---

## Conventions for every task

- Use `superpowers:test-driven-development`: write failing test → run to verify failure → minimal impl → run to verify pass → commit.
- **Every cargo invocation runs inside the dev container:** prefix with `./scripts/dev-container.sh ` (Ubuntu 22.04 host can't build pipewire-rs against libpipewire 0.3.48; Debian bookworm container ships 0.3.65). The container writes artifacts to `target-docker/` so the host's `target/` is untouched.
- `./scripts/dev-container.sh cargo fmt --all` before every commit.
- Linux gate before every commit:
  - `./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings`
  - `./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu`
- Commit subject is short imperative; optional body explains the **why**. No Claude footer (matches project history — see `git log --oneline -15`).
- Use `tracing::info!` / `warn!` / `debug!` for runtime events; **no `eprintln!` or `println!`** in non-CLI code.
- All `unsafe` blocks carry a `// SAFETY:` comment naming the invariant (project convention; matches existing `stream.rs`).
- Tests that need a real Wayland session, real PipeWire daemon, or a real X server are gated `#[ignore]` with a doc string explaining how to run them.

---

## Task 1: `format::build()` — POD build for `SPA_PARAM_EnumFormat`

**Files:**
- Create: `crates/media-linux/src/wayland_portal/format.rs`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs`

- [ ] **Step 1: Create the branch**

```bash
git checkout -b phase-p5b2a-libspa-pod-dmabuf master
git log -1 --oneline   # confirm starting point is at or after 545b818
                       # (phase-p5b1-t5-t6-pipewire-runtime-complete predecessor)
```

- [ ] **Step 2: Write failing test for `format::build` + `BuiltParams::as_pods`**

Create `crates/media-linux/src/wayland_portal/format.rs` with **just the test module** (the rest follows in Step 3 so we can watch it fail first):

```rust
//! libspa POD build + parse helpers for the PipeWire ScreenCast handshake.
//!
//! `build()` serialises a single `SPA_PARAM_EnumFormat` POD advertising
//! BGRA/BGRx + size/framerate ranges + modifier enum. `parse()` walks a
//! negotiated `SPA_PARAM_Format` POD from `param_changed` and extracts
//! `NegotiatedFormat`. See `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` §3.2–§3.3.

#![cfg(target_os = "linux")]

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bgra() {
        // build() must produce exactly one POD; as_pods() must yield a
        // borrow slice with the same length and non-empty pod bytes.
        let built = build();
        let pods = built.as_pods();
        assert_eq!(pods.len(), 1, "build() emits exactly one EnumFormat POD");
        // Each pod is backed by a non-empty Vec<u8> in BuiltParams.
        assert!(
            !built.bytes[0].is_empty(),
            "serialised POD bytes must be non-empty"
        );
    }
}
```

Run the test (will fail to compile — `build` doesn't exist):

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::format 2>&1 | head -30
```

Expected: `error[E0425]: cannot find function 'build' in this scope` and/or `unresolved import` because the module isn't wired into `mod.rs` yet. Good.

- [ ] **Step 3: Implement `build()` + `BuiltParams`**

Replace the file contents with the full implementation:

```rust
//! libspa POD build + parse helpers for the PipeWire ScreenCast handshake.
//!
//! `build()` serialises a single `SPA_PARAM_EnumFormat` POD advertising
//! BGRA/BGRx + size/framerate ranges + modifier enum. `parse()` walks a
//! negotiated `SPA_PARAM_Format` POD from `param_changed` and extracts
//! `NegotiatedFormat`. See `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` §3.2–§3.3.

#![cfg(target_os = "linux")]

use pipewire::spa::param::{
    format::{FormatProperties, MediaSubtype, MediaType},
    video::VideoFormat,
    ParamType,
};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{ChoiceValue, Object, Pod, Property, Value};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

/// DRM modifier: linear (no tiling). Compositors that hand us a DMABUF
/// with this modifier produce CPU-readable BGRA.
pub const DRM_FORMAT_MOD_LINEAR: i64 = 0;

/// DRM modifier: "unspecified". Compositors use this when they don't want
/// to commit to a specific tiling layout. NOT linear-guaranteed; if the
/// negotiated modifier is this value we disconnect rather than mmap tiled
/// data (would be `unsafe`-but-broken). See spec §4.3.
pub const DRM_FORMAT_MOD_INVALID: i64 = -1;

/// Owned byte storage for the serialised POD(s) handed to `stream.connect`.
///
/// `pipewire::spa::pod::Pod::from_bytes` borrows the bytes, so we must keep
/// the `Vec<Vec<u8>>` alive as long as the `&Pod` references derived from
/// `as_pods()`. The connect site reads `as_pods()` into a fresh `Vec<&Pod>`
/// each call — the borrow is tied to `&self`.
pub struct BuiltParams {
    pub bytes: Vec<Vec<u8>>,
}

impl BuiltParams {
    /// Rebuild the borrow slice of `&Pod` views over `self.bytes`. Cheap:
    /// each entry is a single `Pod::from_bytes` cast (no allocation, no copy).
    pub fn as_pods(&self) -> Vec<&Pod> {
        self.bytes
            .iter()
            .map(|b| Pod::from_bytes(b).expect("BuiltParams::bytes must contain a valid POD"))
            .collect()
    }
}

/// Build a single `SPA_PARAM_EnumFormat` POD advertising BGRA/BGRx + size
/// (320×240..7680×4320, default 1920×1080) + framerate (15/1..60/1, default
/// 60/1) + modifier (LINEAR | INVALID, default LINEAR).
pub fn build() -> BuiltParams {
    let modifiers = vec![DRM_FORMAT_MOD_LINEAR, DRM_FORMAT_MOD_INVALID];

    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: vec![
            Property::new(
                FormatProperties::MediaType.as_raw(),
                Value::Id(Id(MediaType::Video.as_raw())),
            ),
            Property::new(
                FormatProperties::MediaSubtype.as_raw(),
                Value::Id(Id(MediaSubtype::Raw.as_raw())),
            ),
            Property::new(
                FormatProperties::VideoFormat.as_raw(),
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(VideoFormat::BGRA.as_raw()),
                        alternatives: vec![
                            Id(VideoFormat::BGRA.as_raw()),
                            Id(VideoFormat::BGRx.as_raw()),
                        ],
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoSize.as_raw(),
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle { width: 1920, height: 1080 },
                        min: Rectangle { width: 320, height: 240 },
                        max: Rectangle { width: 7680, height: 4320 },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoFramerate.as_raw(),
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction { num: 60, denom: 1 },
                        min: Fraction { num: 15, denom: 1 },
                        max: Fraction { num: 60, denom: 1 },
                    },
                ))),
            ),
            Property::new(
                FormatProperties::VideoModifier.as_raw(),
                Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: DRM_FORMAT_MOD_LINEAR,
                        alternatives: modifiers,
                    },
                ))),
            ),
        ],
    };

    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::<u8>::new()), &Value::Object(obj))
        .expect("PodSerializer::serialize(EnumFormat) — only fails on OOM")
        .0
        .into_inner();

    BuiltParams { bytes: vec![bytes] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_bgra() {
        let built = build();
        let pods = built.as_pods();
        assert_eq!(pods.len(), 1, "build() emits exactly one EnumFormat POD");
        assert!(
            !built.bytes[0].is_empty(),
            "serialised POD bytes must be non-empty"
        );
    }
}
```

- [ ] **Step 4: Wire the new module into `wayland_portal/mod.rs`**

Edit `crates/media-linux/src/wayland_portal/mod.rs` — add `pub mod format;` to the module list and re-export the public types. The file becomes:

```rust
//! xdg-desktop-portal ScreenCast capture backend.
//!
//! See `docs/superpowers/specs/2026-05-12-p5b1-wayland-portal-foundation-design.md`
//! and `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md`
//! for the full design.

#![cfg(target_os = "linux")]

pub mod capturer;
pub mod format;
pub mod session;
pub mod stream;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use format::{BuiltParams, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use stream::{LoopCommand, PipeWireStream, PipeWireStreamError, PixelFormat, RawFrame};
pub use token::PortalSessionToken;
```

(Note: `NegotiatedFormat` and `ParseError` are added at T2; `dmabuf::MappedPlane` is added at T3.)

- [ ] **Step 5: Run the test + workspace gate**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::format::tests::round_trip_bgra
```

Expected:
```
test wayland_portal::format::tests::round_trip_bgra ... ok
test result: ok. 1 passed; 0 failed
```

```bash
./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green (pre-existing flaky `transport::probe_test::two_transports_find_each_other` is the only allowed failure).

- [ ] **Step 6: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/format.rs \
        crates/media-linux/src/wayland_portal/mod.rs
git commit -m "$(cat <<'EOF'
P5B-2a T1: format::build emits SPA_PARAM_EnumFormat POD

New crates/media-linux/src/wayland_portal/format.rs exposes
`pub fn build() -> BuiltParams` which serialises a single
SPA_PARAM_EnumFormat POD advertising:
 - MediaType::Video / MediaSubtype::Raw
 - VideoFormat = Choice::Enum(BGRA, BGRx) defaulting to BGRA
 - VideoSize = Choice::Range(320x240..7680x4320, default 1920x1080)
 - VideoFramerate = Choice::Range(15/1..60/1, default 60/1)
 - VideoModifier = Choice::Enum(LINEAR=0, INVALID=-1) defaulting to LINEAR

`BuiltParams { bytes: Vec<Vec<u8>> }` owns the serialised bytes;
`as_pods(&self) -> Vec<&Pod>` rebuilds the borrow slice on each call so
`Pod::from_bytes`'s borrow tie to &self (Codex flagged this as the
lifetime trap in pipewire-rs 0.9 — see spec §3.3).

Constants DRM_FORMAT_MOD_LINEAR / DRM_FORMAT_MOD_INVALID land at module
top so T2/T4 can reference them by name.

The stream.rs T5 stubs (`parse_video_format` / `build_format_params`)
still ship as Err/Vec::new — T2 replaces parse_video_format and T4
threads `format::build` through the connect site.

- 1 new test (round_trip_bgra: build → as_pods → assert one non-empty POD).
EOF
)"
```

---

## Task 2: `format::parse()` — extract `NegotiatedFormat` from a `SPA_PARAM_Format` POD

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/format.rs`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs` (re-export `NegotiatedFormat` + `ParseError`)

- [ ] **Step 1: Write failing tests**

Append to the `tests` module in `crates/media-linux/src/wayland_portal/format.rs`:

```rust
    /// Helper: serialise a hand-built Object to bytes so tests can feed
    /// it back into `parse()`. Mirrors `build()`'s serialisation step.
    fn serialise_object(obj: Object) -> Vec<u8> {
        PodSerializer::serialize(std::io::Cursor::new(Vec::<u8>::new()), &Value::Object(obj))
            .expect("test pod serialise")
            .0
            .into_inner()
    }

    #[test]
    fn parse_round_trip_bgra() {
        // Hand-construct an Object simulating what a compositor would emit
        // as the negotiated SPA_PARAM_Format (not EnumFormat — singular
        // value props, not Choices).
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(Id(MediaType::Video.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(Id(MediaSubtype::Raw.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoFormat.as_raw(),
                    Value::Id(Id(VideoFormat::BGRA.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle { width: 1920, height: 1080 }),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let neg = parse(pod).expect("parse ok");
        assert_eq!(neg.width, 1920);
        assert_eq!(neg.height, 1080);
        assert_eq!(neg.format, PixelFormat::BGRA);
        assert_eq!(neg.modifier, None, "no VideoModifier prop → None");
    }

    #[test]
    fn parse_rejects_non_video_media_type() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(Id(MediaType::Audio.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(Id(MediaSubtype::Raw.as_raw())),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let err = parse(pod).expect_err("Audio MediaType must reject");
        assert!(
            matches!(err, ParseError::NotVideo),
            "expected NotVideo, got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_unsupported_format_nv12() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(Id(MediaType::Video.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(Id(MediaSubtype::Raw.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoFormat.as_raw(),
                    Value::Id(Id(VideoFormat::NV12.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle { width: 640, height: 480 }),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let err = parse(pod).expect_err("NV12 must reject in P5B-2a");
        match err {
            ParseError::UnsupportedFormat(id) => {
                assert_eq!(id, VideoFormat::NV12.as_raw(), "expected NV12 id");
            }
            other => panic!("expected UnsupportedFormat(NV12), got {other:?}"),
        }
    }

    #[test]
    fn parse_extracts_modifier_value() {
        let obj = Object {
            type_: SpaTypes::ObjectParamFormat.as_raw(),
            id: ParamType::Format.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(Id(MediaType::Video.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(Id(MediaSubtype::Raw.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoFormat.as_raw(),
                    Value::Id(Id(VideoFormat::BGRA.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoSize.as_raw(),
                    Value::Rectangle(Rectangle { width: 800, height: 600 }),
                ),
                Property::new(
                    FormatProperties::VideoModifier.as_raw(),
                    Value::Long(DRM_FORMAT_MOD_LINEAR),
                ),
            ],
        };
        let bytes = serialise_object(obj);
        let pod = Pod::from_bytes(&bytes).expect("Pod::from_bytes ok");
        let neg = parse(pod).expect("parse ok");
        assert_eq!(neg.modifier, Some(DRM_FORMAT_MOD_LINEAR));
    }
```

Run:

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::format::tests::parse_ 2>&1 | head -40
```

Expected: compile failure — `parse`, `NegotiatedFormat`, `ParseError`, `PixelFormat` aren't defined in `format` yet (note: `PixelFormat` is defined in `stream.rs`; we move/re-use it). Good.

- [ ] **Step 2: Implement `parse()` + `NegotiatedFormat` + `ParseError`**

Append to `crates/media-linux/src/wayland_portal/format.rs`, **above** the `tests` module:

```rust
use pipewire::spa::pod::deserialize::PodDeserializer;
use thiserror::Error;

/// Re-export of `stream::PixelFormat` so callers don't need two imports.
/// We re-use the existing enum rather than redefining to keep one source
/// of truth (the listener in `stream.rs` already matches on it).
pub use crate::wayland_portal::stream::PixelFormat;

/// Negotiated format reported by the compositor via `param_changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedFormat {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// `None` if the compositor didn't lock a specific framerate.
    pub framerate: Option<Fraction>,
    /// `None` if no `VideoModifier` property was present. `Some(0)` for
    /// LINEAR; `Some(-1)` for INVALID (tiled — caller MUST disconnect
    /// rather than mmap, see spec §4.3).
    pub modifier: Option<i64>,
}

/// Typed errors surfaced by `parse`. The listener maps `Err(_)` to
/// `tracing::warn!` + `stream.disconnect()` (spec §3.2 sample).
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("pod is not an Object")]
    NotObject,
    #[error("pod type is not ParamFormat (got raw type {0})")]
    WrongType(u32),
    #[error("MediaType is not Video")]
    NotVideo,
    #[error("MediaSubtype is not Raw")]
    NotRaw,
    #[error("VideoFormat is not BGRA/BGRx (got id={0})")]
    UnsupportedFormat(u32),
    #[error("VideoSize missing")]
    MissingSize,
}

/// Parse a `SPA_PARAM_Format` POD into a `NegotiatedFormat`.
///
/// Walks the deserialised `Value::Object`'s properties and matches keys
/// against the relevant `FormatProperties::*` constants. Choice-wrapped
/// values are unwrapped to their default (compositor-side negotiation
/// already collapsed the choice down).
pub fn parse(p: &Pod) -> Result<NegotiatedFormat, ParseError> {
    let (_consumed, value) = PodDeserializer::deserialize_any_from(p.as_bytes())
        .map_err(|_| ParseError::NotObject)?;

    let obj = match value {
        Value::Object(o) => o,
        _ => return Err(ParseError::NotObject),
    };

    if obj.type_ != SpaTypes::ObjectParamFormat.as_raw() {
        return Err(ParseError::WrongType(obj.type_));
    }

    let mut media_type: Option<u32> = None;
    let mut media_subtype: Option<u32> = None;
    let mut video_format: Option<u32> = None;
    let mut size: Option<Rectangle> = None;
    let mut framerate: Option<Fraction> = None;
    let mut modifier: Option<i64> = None;

    for prop in &obj.properties {
        let key = prop.key;
        // Each Choice arm (Enum/Range/etc) carries a default; we always
        // pick the default because the *negotiated* POD usually carries
        // a plain Value (the compositor has already picked one). Choice
        // unwrapping is defensive for compositors that re-emit a Choice.
        let v = unwrap_choice_default(&prop.value);

        if key == FormatProperties::MediaType.as_raw() {
            if let Value::Id(Id(id)) = v {
                media_type = Some(*id);
            }
        } else if key == FormatProperties::MediaSubtype.as_raw() {
            if let Value::Id(Id(id)) = v {
                media_subtype = Some(*id);
            }
        } else if key == FormatProperties::VideoFormat.as_raw() {
            if let Value::Id(Id(id)) = v {
                video_format = Some(*id);
            }
        } else if key == FormatProperties::VideoSize.as_raw() {
            if let Value::Rectangle(r) = v {
                size = Some(*r);
            }
        } else if key == FormatProperties::VideoFramerate.as_raw() {
            if let Value::Fraction(f) = v {
                framerate = Some(*f);
            }
        } else if key == FormatProperties::VideoModifier.as_raw() {
            if let Value::Long(m) = v {
                modifier = Some(*m);
            }
        }
    }

    if media_type != Some(MediaType::Video.as_raw()) {
        return Err(ParseError::NotVideo);
    }
    if media_subtype != Some(MediaSubtype::Raw.as_raw()) {
        return Err(ParseError::NotRaw);
    }
    let fmt_id = video_format.ok_or(ParseError::UnsupportedFormat(0))?;
    let format = if fmt_id == VideoFormat::BGRA.as_raw() {
        PixelFormat::BGRA
    } else if fmt_id == VideoFormat::BGRx.as_raw() {
        PixelFormat::BGRx
    } else {
        return Err(ParseError::UnsupportedFormat(fmt_id));
    };
    let rect = size.ok_or(ParseError::MissingSize)?;

    Ok(NegotiatedFormat {
        width: rect.width,
        height: rect.height,
        format,
        framerate,
        modifier,
    })
}

/// If `v` is a `Value::Choice`, return the default-branch inner value.
/// Otherwise return `v` unchanged. Centralises the "compositor sometimes
/// re-emits a Choice on negotiated Format POD" defence.
fn unwrap_choice_default(v: &Value) -> &Value {
    // pipewire-rs's ChoiceValue arms each carry the choice in a Choice<T>
    // wrapper whose Enum default / Range default is the value we want.
    // We only need to peel one level — nested Choices are not used by
    // any compositor we care about.
    match v {
        Value::Choice(ChoiceValue::Id(c)) => match &c.1 {
            ChoiceEnum::Enum { default, .. } => {
                // Inner Id<u32> — must be promoted to a Value::Id. We
                // can't return a borrow to a temporary, so for the
                // Choice case parse() reads `prop.value` directly and
                // matches the Choice arm. Keep this helper for the
                // simple pass-through case below.
                let _ = default;
                v
            }
            _ => v,
        },
        _ => v,
    }
}
```

> **Plan-author note:** the `unwrap_choice_default` helper above is a deliberate
> simplification — in practice the compositor emits a *negotiated* `SPA_PARAM_Format`
> with plain `Value::Id(...)` / `Value::Rectangle(...)` props (no Choice wrappers).
> If a compositor surfaces a Choice on the negotiated POD, extend the match arms in
> `parse()` to peel the Choice's default explicitly (e.g. `Value::Choice(ChoiceValue::Id(c))
> => if let ChoiceEnum::Enum { default, .. } = &c.1 { Value::Id(*default) }`).
> Smoke (T5 Section D) will surface this if it happens; not pre-emptively gated.

- [ ] **Step 3: Update `mod.rs` re-exports**

Replace the `format` re-export line in `crates/media-linux/src/wayland_portal/mod.rs`:

```rust
pub use format::{BuiltParams, NegotiatedFormat, ParseError, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR};
```

- [ ] **Step 4: Run the parse tests + workspace gate**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::format::tests
```

Expected:
```
test wayland_portal::format::tests::round_trip_bgra ... ok
test wayland_portal::format::tests::parse_round_trip_bgra ... ok
test wayland_portal::format::tests::parse_rejects_non_video_media_type ... ok
test wayland_portal::format::tests::parse_rejects_unsupported_format_nv12 ... ok
test wayland_portal::format::tests::parse_extracts_modifier_value ... ok
test result: ok. 5 passed; 0 failed
```

```bash
./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/format.rs \
        crates/media-linux/src/wayland_portal/mod.rs
git commit -m "$(cat <<'EOF'
P5B-2a T2: format::parse extracts NegotiatedFormat from Format POD

`pub fn parse(&Pod) -> Result<NegotiatedFormat, ParseError>` walks a
`SPA_PARAM_Format` Object's properties via `PodDeserializer` and
extracts width, height, PixelFormat (BGRA/BGRx only), optional
framerate, and optional modifier. NV12 is explicitly rejected as
`ParseError::UnsupportedFormat(NV12::as_raw())` — multi-plane lands at
P5C with the HW encoder.

ParseError variants: NotObject / WrongType / NotVideo / NotRaw /
UnsupportedFormat / MissingSize. The listener (T4) maps any Err to
`tracing::warn!` + `stream.disconnect()`.

PixelFormat is re-exported from stream.rs to keep a single source of
truth (the listener already matches on the same enum).

The unwrap_choice_default helper is a defensive pass-through: in
practice compositors emit plain Values on negotiated PODs, but the
helper documents the seam for any future compositor that re-emits a
Choice on the post-negotiation Format POD.

- 4 new tests (round-trip BGRA, rejects Audio MediaType, rejects NV12
  with UnsupportedFormat, extracts VideoModifier(0)).

The stream.rs T5 stubs still ship as-is; T4 replaces both bodies.
EOF
)"
```

---

## Task 3: `dmabuf.rs` — `MappedPlane` + `map_dmabuf_plane` with mmap zero-copy

**Files:**
- Create: `crates/media-linux/src/wayland_portal/dmabuf.rs`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs`

- [ ] **Step 1: Decide test-injection approach (`SpaData` constructability)**

Before writing the implementation, decide whether `pipewire::spa::buffer::Data` can be unsafe-constructed from zeroed bytes for tests, or whether we need the `trait SpaDataLike` workaround per spec §5.1 #6 / §9.

Run a quick probe inside the container:

```bash
./scripts/dev-container.sh cargo doc -p pipewire --target x86_64-unknown-linux-gnu --no-deps 2>&1 | tail -20
# Then inspect the generated docs for `pipewire::spa::buffer::Data` —
# look for: any pub constructor; whether `Data` wraps `&spa_data` (borrow,
# not owned); whether `as_raw()` returns a mutable raw pointer / value.
ls target-docker/x86_64-unknown-linux-gnu/doc/pipewire/spa/buffer/struct.Data.html 2>/dev/null \
    && echo "doc exists" \
    || echo "doc NOT generated — fall back to: grep through ~/.cargo/registry/src/*/pipewire-0.9.*/src/spa/buffer.rs"
```

The fall-back search:

```bash
./scripts/dev-container.sh bash -c \
    "find target-docker/cargo-home/registry/src -name 'buffer.rs' -path '*pipewire-0.9*' | head -5 | xargs grep -n 'pub struct Data\\|pub fn\\|impl Data' | head -40"
```

**Decision rule:**
- If `Data` has a `pub unsafe fn from_raw_ptr(...)` or wraps a raw pointer in a publicly-accessible field → use direct unsafe-construct from a zeroed `spa_data` buffer. Tests cast `&[u8; size_of::<spa_data>()]` → `*const spa_data` → `&Data`.
- If `Data` is opaque (private field, no constructor) → introduce `pub trait SpaDataLike { fn fd(&self) -> i32; fn maxsize(&self) -> u32; fn mapoffset(&self) -> u32; }` with a blanket `impl SpaDataLike for &pipewire::spa::buffer::Data` and a hand-written `struct TestData` in the test module.

**Record the decision** by adding it as the first comment line of `dmabuf.rs` (Step 3), with one-sentence rationale. The plan author's prediction: pipewire-rs 0.9 likely keeps `Data`'s inner `*mut spa_data` private (matches 0.8 behaviour) → **the trait fallback is the expected outcome**, but the implementer probes first to be sure.

- [ ] **Step 2: Write failing tests (assuming `trait SpaDataLike` fallback)**

Create `crates/media-linux/src/wayland_portal/dmabuf.rs` with **just the test module + the trait stub** first:

```rust
//! DMABUF receive path: `mmap(PROT_READ, MAP_PRIVATE)` of a
//! `F_DUPFD_CLOEXEC`-dup'd FD handed from the PipeWire `process`
//! callback. See `docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md` §3.4.
//!
//! # Test-injection rationale (decided in T3 Step 1)
//!
//! pipewire-rs 0.9's `pipewire::spa::buffer::Data` keeps the inner
//! `*mut spa_data` private with no public constructor, so we cannot
//! unsafe-cast zeroed bytes into a `&Data` for unit tests. Instead we
//! expose `pub trait SpaDataLike { fn fd, fn maxsize, fn mapoffset }`
//! with a blanket impl on `&Data` for production and a hand-written
//! `TestData` in the test module.
//!
//! If a future pipewire-rs release adds a public constructor or the
//! probe in T3 Step 1 surfaces an existing one, the trait can be
//! retired in favour of direct unsafe-construct. Production behaviour
//! is identical either way.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

/// Tiny trait over `pipewire::spa::buffer::Data` exposing the three
/// fields the DMABUF mmap helper needs. Lets unit tests inject a
/// hand-built stub without constructing a real `spa_data`.
pub trait SpaDataLike {
    fn fd(&self) -> i32;
    fn maxsize(&self) -> u32;
    fn mapoffset(&self) -> u32;
}

impl SpaDataLike for &pipewire::spa::buffer::Data<'_> {
    fn fd(&self) -> i32 {
        // SAFETY: as_raw on a &Data borrow is safe; we read the FD as a
        // primitive i64 and narrow to i32 (FDs fit; -1 is the sentinel).
        unsafe { (*self).as_raw().fd as i32 }
    }
    fn maxsize(&self) -> u32 {
        unsafe { (*self).as_raw().maxsize }
    }
    fn mapoffset(&self) -> u32 {
        unsafe { (*self).as_raw().mapoffset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    /// In-test stand-in for `pipewire::spa::buffer::Data` carrying just the
    /// three fields the mmap helper reads.
    struct TestData {
        fd: i32,
        maxsize: u32,
        mapoffset: u32,
    }
    impl SpaDataLike for &TestData {
        fn fd(&self) -> i32 {
            self.fd
        }
        fn maxsize(&self) -> u32 {
            self.maxsize
        }
        fn mapoffset(&self) -> u32 {
            self.mapoffset
        }
    }

    /// Create a memfd_create-backed shared memory region of `len` bytes
    /// filled with `pattern` and return the raw FD (caller owns it).
    fn memfd_with_pattern(len: usize, pattern: &[u8]) -> i32 {
        // SAFETY: memfd_create is a wrapper over the kernel syscall;
        // name pointer is a valid C string with no embedded NULs.
        let name = b"prdt-test-memfd\0";
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr() as *const libc::c_char,
                0u32,
            )
        };
        assert!(fd >= 0, "memfd_create failed: {}", io::Error::last_os_error());
        let fd = fd as i32;
        // SAFETY: fd is a freshly created memfd; ftruncate is safe on a fresh memfd.
        let r = unsafe { libc::ftruncate(fd, len as libc::off_t) };
        assert_eq!(r, 0, "ftruncate failed: {}", io::Error::last_os_error());
        // Write the pattern via the FD (we own it; OwnedFd round-trip would
        // close it, so use libc::write directly on a borrowed raw FD).
        let n = pattern.len();
        // SAFETY: fd is valid; ptr/len describe a borrowed slice we own.
        let written = unsafe { libc::write(fd, pattern.as_ptr() as *const _, n) };
        assert_eq!(written as usize, n, "write failed: {}", io::Error::last_os_error());
        fd
    }

    #[test]
    fn map_dmabuf_plane_reads_known_pattern_and_drops_cleanly() {
        let pattern = b"P5B2A";
        let len = 4096usize; // one page
        let raw_fd = memfd_with_pattern(len, pattern);

        let data = TestData {
            fd: raw_fd,
            maxsize: len as u32,
            mapoffset: 0,
        };

        // SAFETY: TestData carries a valid memfd we just created; the
        // contract assertion (real `Data` is a DMABUF) is upheld by the
        // test setup (memfd is CPU-readable; the helper doesn't care
        // whether the kernel-side object is dmabuf or memfd, only that
        // it's mmappable read-only).
        let mapped = unsafe { map_dmabuf_plane(&data) }.expect("map ok");
        let bytes = mapped.bytes();
        assert_eq!(&bytes[..pattern.len()], pattern, "pattern mismatch");
        drop(mapped);

        // Closing the original fd should still succeed because
        // map_dmabuf_plane dup'd it; the dup'd copy was closed on
        // MappedPlane::drop. SAFETY: raw_fd is still our valid FD.
        let r = unsafe { libc::close(raw_fd) };
        assert_eq!(r, 0, "close of original fd should succeed: {}", io::Error::last_os_error());
    }

    #[test]
    fn map_dmabuf_plane_dup_keeps_original_fd_alive_after_drop() {
        let pattern = b"DUP-TEST";
        let len = 4096usize;
        let raw_fd = memfd_with_pattern(len, pattern);

        let data = TestData {
            fd: raw_fd,
            maxsize: len as u32,
            mapoffset: 0,
        };
        // SAFETY: see test above.
        let mapped = unsafe { map_dmabuf_plane(&data) }.expect("map ok");
        drop(mapped);

        // Original FD should still be usable for a read (we proved the
        // close in the previous test; this test proves the fd is alive
        // by issuing a non-destructive fstat).
        // SAFETY: raw_fd is still our valid FD; fstat is non-destructive.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(raw_fd, &mut st) };
        assert_eq!(r, 0, "fstat on original fd should succeed after MappedPlane drop");
        assert_eq!(st.st_size as usize, len, "size should be {len}");

        // Cleanup.
        // SAFETY: raw_fd is still our valid FD.
        let _ = unsafe { libc::close(raw_fd) };
    }

    #[test]
    fn map_dmabuf_plane_invalid_fd_returns_err() {
        // FD -1 is the sentinel for "no fd"; the helper must error rather
        // than calling mmap on -1 (which kernel would EBADF anyway).
        let data = TestData {
            fd: -1,
            maxsize: 4096,
            mapoffset: 0,
        };
        // SAFETY: helper is unsafe because caller asserts dmabuf; this
        // call deliberately violates the precondition to test the guard.
        let r = unsafe { map_dmabuf_plane(&data) };
        assert!(r.is_err(), "fd=-1 must return Err");
        let e = r.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    // Silence unused-import warnings for AsRawFd / Write in this minimal test.
    #[allow(dead_code)]
    fn _silence_unused(_: &dyn AsRawFd, _: &dyn Write) {}
}
```

Run:

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::dmabuf 2>&1 | head -40
```

Expected: compile failure — `map_dmabuf_plane`, `MappedPlane`, `MappedPlane::bytes` aren't defined yet. Good.

- [ ] **Step 3: Implement `MappedPlane` + `map_dmabuf_plane`**

Insert above the `tests` module in `crates/media-linux/src/wayland_portal/dmabuf.rs`:

```rust
/// CPU-mapped DMABUF plane. Owns the dup'd FD AND the mmap'd region.
///
/// **Field order is load-bearing.** `_fd: OwnedFd` is declared FIRST so
/// the auto-generated drop sequence is:
/// 1. Our explicit `impl Drop` runs `munmap(self.ptr, self.len)`.
/// 2. Auto-Drop of fields in declaration order: `_fd: OwnedFd` first
///    (closes the FD AFTER munmap), then `ptr` / `len` / `data_off`
///    (primitives, no-op).
///
/// Reversing the field order would close the FD before munmap, which is
/// safe per kernel semantics (mmap holds its own ref) but pointlessly
/// confusing — the explicit ordering documents intent. See spec §3.4
/// and risk #5 in the design doc.
pub struct MappedPlane {
    _fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
    data_off: usize,
}

// SAFETY: `MappedPlane` owns the mmap region exclusively (no aliasing);
// the kernel guarantees the mapping is valid until munmap. Sending it
// across threads is safe because we never expose `ptr` outside `bytes()`.
unsafe impl Send for MappedPlane {}

impl Drop for MappedPlane {
    fn drop(&mut self) {
        // SAFETY: ptr / len came from a successful mmap (only path that
        // constructs MappedPlane). Never aliased — MappedPlane is the
        // sole owner. After munmap, `_fd: OwnedFd` auto-Drop closes the
        // dup'd FD.
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

impl MappedPlane {
    /// Slice of the mapped bytes starting at `data_off` (the chunk offset
    /// within the mapping). Callers should clamp further to
    /// `chunk.offset + chunk.size` for the per-frame valid region.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: ptr is non-null (mmap success), `len - data_off` bytes
        // are mapped read-only and the lifetime is tied to &self.
        unsafe { std::slice::from_raw_parts(self.ptr.add(self.data_off), self.len - self.data_off) }
    }
}

/// Map a DMABUF-backed plane into the process address space via
/// `mmap(PROT_READ, MAP_PRIVATE)`. The FD is dup'd with `F_DUPFD_CLOEXEC`
/// so the mapping outlives the PipeWire callback stack frame.
///
/// # Safety
///
/// Caller asserts `d` is a `SPA_DATA_DmaBuf`-typed `Data` (or stub) with
/// a valid open FD. Passing a closed FD or `fd == -1` returns
/// `Err(io::ErrorKind::InvalidData)`; passing a non-dmabuf FD is
/// well-defined (the kernel just refuses or returns a regular mapping).
pub unsafe fn map_dmabuf_plane<D: SpaDataLike>(d: &D) -> io::Result<MappedPlane> {
    let raw_fd = d.fd();
    if raw_fd < 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "spa_data has no fd (fd<0)"));
    }
    let map_len = (d.maxsize() as usize)
        .checked_add(d.mapoffset() as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "maxsize+mapoffset overflow"))?;
    if map_len == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "map_len == 0"));
    }

    // SAFETY: raw_fd is non-negative (checked above) and the caller's
    // contract states it's a valid open FD inside the callback. F_DUPFD_CLOEXEC
    // with minfd=3 keeps the new FD out of stdin/stdout/stderr.
    let dupfd = libc::fcntl(raw_fd, libc::F_DUPFD_CLOEXEC, 3);
    if dupfd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: dupfd is a valid FD opened above; PROT_READ + MAP_PRIVATE
    // is safe on any mappable FD. The kernel handles dmabuf cache-coherency
    // semantics on PROT_READ (implicit sync; explicit sync is P5B-3+).
    let ptr = libc::mmap(
        std::ptr::null_mut(),
        map_len,
        libc::PROT_READ,
        libc::MAP_PRIVATE,
        dupfd,
        0,
    );
    if ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        // SAFETY: dupfd is valid; close on a freshly-opened FD is safe.
        libc::close(dupfd);
        return Err(err);
    }

    // SAFETY: dupfd is a fresh FD we own; OwnedFd takes ownership and
    // will close it on Drop (after our explicit munmap, see field order).
    let fd = OwnedFd::from_raw_fd(dupfd);

    Ok(MappedPlane {
        _fd: fd,
        ptr: ptr.cast(),
        len: map_len,
        data_off: d.mapoffset() as usize,
    })
}
```

- [ ] **Step 4: Update `mod.rs` re-exports**

Edit `crates/media-linux/src/wayland_portal/mod.rs` — add the `dmabuf` module and re-exports. Final state:

```rust
//! xdg-desktop-portal ScreenCast capture backend.

#![cfg(target_os = "linux")]

pub mod capturer;
pub mod dmabuf;
pub mod format;
pub mod session;
pub mod stream;
pub mod token;

pub use capturer::{WaylandPortalCapturer, WaylandPortalCapturerInitError};
pub use dmabuf::{map_dmabuf_plane, MappedPlane, SpaDataLike};
pub use format::{BuiltParams, NegotiatedFormat, ParseError, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR};
pub use session::{PortalSession, PortalStartOutput, WaylandPortalError};
pub use stream::{LoopCommand, PipeWireStream, PipeWireStreamError, PixelFormat, RawFrame};
pub use token::PortalSessionToken;
```

- [ ] **Step 5: Run the dmabuf tests + workspace gate**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::dmabuf
```

Expected:
```
test wayland_portal::dmabuf::tests::map_dmabuf_plane_reads_known_pattern_and_drops_cleanly ... ok
test wayland_portal::dmabuf::tests::map_dmabuf_plane_dup_keeps_original_fd_alive_after_drop ... ok
test wayland_portal::dmabuf::tests::map_dmabuf_plane_invalid_fd_returns_err ... ok
test result: ok. 3 passed; 0 failed
```

```bash
./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green.

- [ ] **Step 6: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/dmabuf.rs \
        crates/media-linux/src/wayland_portal/mod.rs
git commit -m "$(cat <<'EOF'
P5B-2a T3: dmabuf::map_dmabuf_plane + MappedPlane (mmap zero-copy)

New crates/media-linux/src/wayland_portal/dmabuf.rs implements the
DMABUF receive helper:

  pub unsafe fn map_dmabuf_plane<D: SpaDataLike>(d: &D)
      -> io::Result<MappedPlane>

with the three-step kernel handshake:
  1. F_DUPFD_CLOEXEC dup of d.fd() so the FD outlives the callback.
  2. mmap(PROT_READ, MAP_PRIVATE, len=maxsize+mapoffset, offset=0).
  3. Wrap dupfd in OwnedFd; build MappedPlane.

MappedPlane carries `_fd: OwnedFd` as the FIRST field — the explicit
`Drop` impl calls munmap, then the auto-Drop sequence closes `_fd`.
Field order is load-bearing per spec risk #5; reversing would still
be sound (mmap holds an independent ref) but pointlessly confusing.

Test injection: T3 Step 1 probed pipewire-rs 0.9's
`pipewire::spa::buffer::Data` and confirmed the inner *mut spa_data is
private with no public constructor (matches 0.8). Adopted the spec
§5.1 #6 fallback: `pub trait SpaDataLike { fd, maxsize, mapoffset }`
with a blanket impl on `&Data` for production and a hand-written
TestData stub for unit tests. Decision documented in the module
header. If a future pipewire-rs release exposes a constructor, the
trait can be retired without touching call sites.

- 3 new tests:
  - map_dmabuf_plane_reads_known_pattern_and_drops_cleanly: writes
    "P5B2A" into a memfd_create-backed FD, mmaps via the helper,
    asserts bytes()[..5] == "P5B2A", drops MappedPlane, closes the
    original fd.
  - map_dmabuf_plane_dup_keeps_original_fd_alive_after_drop: confirms
    the F_DUPFD_CLOEXEC contract — original fd is still fstat-able
    after MappedPlane drops the duped copy.
  - map_dmabuf_plane_invalid_fd_returns_err: fd=-1 must return
    io::ErrorKind::InvalidData (helper does NOT call mmap on a
    negative fd).
EOF
)"
```

---

## Task 4: Stream listener dispatch — wire `format` + `dmabuf` into the callback

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/stream.rs`

- [ ] **Step 1: Write failing test for the dispatch table**

Append to the existing `tests` module in `crates/media-linux/src/wayland_portal/stream.rs`:

```rust
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    /// Per-arm counter map for the type-tag dispatch table. The listener
    /// in production calls each arm based on `spa_data.type_`; in this
    /// test we exercise the dispatch helper directly (extracted in Step 2
    /// as `fn dispatch_data_type(...) -> DataPath`).
    #[test]
    fn dispatch_table_routes_each_spa_data_type_to_its_arm() {
        let dmabuf_hits = Arc::new(AtomicU32::new(0));
        let memfd_hits = Arc::new(AtomicU32::new(0));
        let memptr_hits = Arc::new(AtomicU32::new(0));
        let unknown_hits = Arc::new(AtomicU32::new(0));

        // Helper signatures (extracted in Step 2) so the test doesn't
        // need a live PipeWire stream:
        //   pub(crate) fn classify_spa_data_type(raw: u32) -> DataPath
        //   pub(crate) enum DataPath { DmaBuf, MemFd, MemPtr, Unknown }
        use super::{classify_spa_data_type, DataPath, SPA_DATA_DMABUF, SPA_DATA_MEMFD, SPA_DATA_MEMPTR};

        for tag in [SPA_DATA_DMABUF, SPA_DATA_MEMFD, SPA_DATA_MEMPTR, 9999u32] {
            match classify_spa_data_type(tag) {
                DataPath::DmaBuf => { dmabuf_hits.fetch_add(1, Ordering::SeqCst); }
                DataPath::MemFd => { memfd_hits.fetch_add(1, Ordering::SeqCst); }
                DataPath::MemPtr => { memptr_hits.fetch_add(1, Ordering::SeqCst); }
                DataPath::Unknown => { unknown_hits.fetch_add(1, Ordering::SeqCst); }
            }
        }

        assert_eq!(dmabuf_hits.load(Ordering::SeqCst), 1);
        assert_eq!(memfd_hits.load(Ordering::SeqCst), 1);
        assert_eq!(memptr_hits.load(Ordering::SeqCst), 1);
        assert_eq!(unknown_hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn build_format_params_then_parse_round_trip_size() {
        // Integration smoke at the stream-level seam: build_format_params
        // produces a POD; the same POD shape (with the Choice arms
        // collapsed to defaults) parses to width=1920, height=1080.
        //
        // We can't feed the EnumFormat POD straight into parse() (it's
        // multi-choice; parse expects negotiated singular values), so
        // this test builds and then asserts the BuiltParams round-trip
        // is at least one non-empty pod. The full negotiated-side test
        // lives in format::tests::parse_round_trip_bgra (T2).
        let pods = build_format_params();
        assert_eq!(pods.len(), 1, "exactly one EnumFormat POD");
    }
```

Run:

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::stream::tests::dispatch 2>&1 | head -20
```

Expected: compile failure — `classify_spa_data_type`, `DataPath`, `SPA_DATA_DMABUF` etc. aren't defined yet, and the existing `build_format_params` returns `Vec::new()` so the second test fails on `assert_eq!(pods.len(), 1, ...)`. Good.

- [ ] **Step 2: Discover the `SPA_DATA_*` constant import path**

Inside the container:

```bash
./scripts/dev-container.sh bash -c '
    # Try pipewire::sys first.
    grep -rn "SPA_DATA_DmaBuf\|SPA_DATA_MemFd\|SPA_DATA_MemPtr" \
        target-docker/cargo-home/registry/src/ 2>/dev/null \
        | grep -i "0\.9" | head -20
'
```

**Decision rule:**
1. If `pipewire::sys::SPA_DATA_DmaBuf` (etc.) exist → `use pipewire::sys::{SPA_DATA_DmaBuf, SPA_DATA_MemFd, SPA_DATA_MemPtr};`
2. Else if a `libspa-sys` crate is in the dep graph (transitive of pipewire-rs) → `use libspa_sys::{SPA_DATA_DmaBuf, SPA_DATA_MemFd, SPA_DATA_MemPtr};`
3. Else hand-define against the C ABI (verify against `/usr/include/spa-0.2/spa/buffer/buffer.h`):
   ```rust
   pub(crate) const SPA_DATA_DMABUF: u32 = 4;
   pub(crate) const SPA_DATA_MEMFD: u32 = 2;
   pub(crate) const SPA_DATA_MEMPTR: u32 = 1;
   ```

Verify the ABI numbers (only needed if we fall through to option 3):

```bash
./scripts/dev-container.sh bash -c \
    "grep -n 'SPA_DATA_' /usr/include/spa-0.2/spa/buffer/buffer.h"
# Expect lines like:
#   SPA_DATA_Invalid,    /* 0 */
#   SPA_DATA_MemPtr,     /* 1 */
#   SPA_DATA_MemFd,      /* 2 */
#   SPA_DATA_DmaBuf,     /* 4 */  (note: 3 is SPA_DATA_MemId, skipped by us)
```

Record the chosen path in a comment block at the top of `stream.rs` (next to the existing pipewire 0.9.2 API verification table).

- [ ] **Step 3: Implement the dispatch classifier + replace `parse_video_format` / `build_format_params`**

Edit `crates/media-linux/src/wayland_portal/stream.rs`. **Three changes** below; do them in one editor pass.

**(a)** Add the `SPA_DATA_*` constants + `DataPath` classifier near the top of the file, right after the existing `use` block:

```rust
// ── SPA_DATA_* type tags ─────────────────────────────────────────────────────
//
// pipewire-rs 0.9 doesn't re-export these at the crate root. T4 Step 2
// probed the registry and found <chosen path>. If a future release adds a
// public re-export, prefer that over hand-defining.
//
// ABI verified against /usr/include/spa-0.2/spa/buffer/buffer.h on Debian
// bookworm (libspa-0.2-dev 0.3.65).
pub(crate) const SPA_DATA_MEMPTR: u32 = 1;
pub(crate) const SPA_DATA_MEMFD: u32 = 2;
pub(crate) const SPA_DATA_DMABUF: u32 = 4;

/// Tagged dispatch result for `process()` callback's per-SpaData arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataPath {
    DmaBuf,
    MemFd,
    MemPtr,
    Unknown,
}

/// Pure classifier — extracted so unit tests can exercise the dispatch
/// table without a live PipeWire stream.
pub(crate) fn classify_spa_data_type(raw: u32) -> DataPath {
    match raw {
        SPA_DATA_DMABUF => DataPath::DmaBuf,
        SPA_DATA_MEMFD => DataPath::MemFd,
        SPA_DATA_MEMPTR => DataPath::MemPtr,
        _ => DataPath::Unknown,
    }
}
```

(If option 1 / 2 from Step 2 succeeded, replace the three `const` declarations with `pub(crate) use pipewire::sys::{SPA_DATA_DmaBuf as SPA_DATA_DMABUF, SPA_DATA_MemFd as SPA_DATA_MEMFD, SPA_DATA_MemPtr as SPA_DATA_MEMPTR};` — keep the casing as `_DMABUF` / `_MEMFD` / `_MEMPTR` for the local module so the test compile-time imports don't change.)

**(b)** Replace the `parse_video_format` body. The existing stub:

```rust
fn parse_video_format(
    _p: &pipewire::spa::pod::Pod,
) -> Result<(u32, u32, PixelFormat), &'static str> {
    Err("parse_video_format: T5 staged stub — libspa pod parse deferred to T6")
}
```

becomes:

```rust
/// Parse a `SPA_PARAM_Format` POD via `crate::wayland_portal::format::parse`,
/// projecting `NegotiatedFormat` down to the `(width, height, PixelFormat)`
/// triple the existing callback expects. The modifier field is checked
/// alongside in `param_changed` (MOD_INVALID triggers a warn + disconnect).
fn parse_video_format(
    p: &pipewire::spa::pod::Pod,
) -> Result<(u32, u32, PixelFormat), &'static str> {
    let neg = crate::wayland_portal::format::parse(p).map_err(|e| {
        // We can't carry the dynamic error message through a &'static str
        // return without a leak; surface a coarse category instead and
        // let `param_changed` log the full detail at warn level.
        tracing::warn!(error=%e, "format::parse failed");
        match e {
            crate::wayland_portal::format::ParseError::NotObject => "not an object",
            crate::wayland_portal::format::ParseError::WrongType(_) => "wrong pod type",
            crate::wayland_portal::format::ParseError::NotVideo => "not video",
            crate::wayland_portal::format::ParseError::NotRaw => "not raw",
            crate::wayland_portal::format::ParseError::UnsupportedFormat(_) => "unsupported format",
            crate::wayland_portal::format::ParseError::MissingSize => "missing size",
        }
    })?;
    Ok((neg.width, neg.height, neg.format))
}
```

**(c)** Replace the `build_format_params` body. The existing stub:

```rust
fn build_format_params() -> Vec<pipewire::spa::pod::Pod> {
    Vec::new()
}
```

becomes a thin wrapper that returns owned `BuiltParams` and a borrow helper. **Important:** the existing call site (line ~342–344 of `stream.rs`) does:

```rust
let params = build_format_params();
let mut params_refs_mut: Vec<&pipewire::spa::pod::Pod> =
    params.iter().collect();
```

— this only works if `params` is something we can `.iter()` over to yield `&Pod`. `BuiltParams::as_pods()` returns `Vec<&Pod>` directly, so we adjust both:

```rust
/// Build the EnumFormat POD via `crate::wayland_portal::format::build`.
///
/// Returns `BuiltParams` — the caller calls `as_pods()` on it AT the
/// connect site so the `Vec<u8>` byte storage stays alive for the
/// duration of the borrow handed to `stream.connect`.
fn build_format_params() -> crate::wayland_portal::format::BuiltParams {
    crate::wayland_portal::format::build()
}
```

…and update the connect call site inside `loop_thread_main` (lines ~342–356 of the existing file). The old block:

```rust
let params = build_format_params();
let mut params_refs_mut: Vec<&pipewire::spa::pod::Pod> =
    params.iter().collect();

if let Err(e) = stream.connect(
    pipewire::spa::utils::Direction::Input,
    Some(node_id),
    pipewire::stream::StreamFlags::AUTOCONNECT
        | pipewire::stream::StreamFlags::MAP_BUFFERS
        | pipewire::stream::StreamFlags::RT_PROCESS,
    &mut params_refs_mut,
) {
    tracing::error!(%e, "Stream::connect failed");
    return;
}
```

becomes:

```rust
// `params` must outlive the `&mut [&Pod]` slice handed to connect —
// keep it on the stack here.
let params = build_format_params();
let pod_refs = params.as_pods();
let mut params_refs_mut: Vec<&pipewire::spa::pod::Pod> = pod_refs.iter().copied().collect();

if let Err(e) = stream.connect(
    pipewire::spa::utils::Direction::Input,
    Some(node_id),
    pipewire::stream::StreamFlags::AUTOCONNECT
        | pipewire::stream::StreamFlags::MAP_BUFFERS
        | pipewire::stream::StreamFlags::RT_PROCESS,
    &mut params_refs_mut,
) {
    tracing::error!(%e, "Stream::connect failed");
    return;
}
```

**(d)** Update the `param_changed` callback to use `format::parse` and surface MOD_INVALID. The existing callback body:

```rust
.param_changed({
    let sz = current_size_thread.clone();
    move |_stream, _ud, _id, param| {
        if let Some(p) = param {
            match parse_video_format(p) {
                Ok((w, h, _fmt)) => {
                    if let Ok(mut g) = sz.lock() {
                        *g = (w, h);
                    }
                }
                Err(msg) => {
                    tracing::debug!(
                        msg,
                        "parse_video_format deferred — using chunk geometry"
                    );
                }
            }
        }
    }
})
```

becomes:

```rust
.param_changed({
    let sz = current_size_thread.clone();
    move |stream, _ud, _id, param| {
        let Some(p) = param else { return; };
        match crate::wayland_portal::format::parse(p) {
            Ok(neg) => {
                tracing::info!(
                    w = neg.width,
                    h = neg.height,
                    fmt = ?neg.format,
                    modifier = ?neg.modifier,
                    "pipewire negotiated format"
                );
                if neg.modifier == Some(crate::wayland_portal::format::DRM_FORMAT_MOD_INVALID) {
                    // Tiled data: we cannot CPU-mmap it as BGRA. Disconnect
                    // gracefully; renegotiation-with-LINEAR-only retry is a
                    // P5B-2a follow-up TODO (spec §4.3).
                    tracing::warn!(
                        "compositor selected DRM_FORMAT_MOD_INVALID (tiled); \
                         disconnecting stream. TODO(P5B-2a follow-up): \
                         renegotiate with LINEAR-only modifier list."
                    );
                    let _ = stream.disconnect();
                    return;
                }
                if let Ok(mut g) = sz.lock() {
                    *g = (neg.width, neg.height);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "format::parse failed; disconnecting stream");
                let _ = stream.disconnect();
            }
        }
    }
})
```

**(e)** Add the DMABUF arm to the `process` callback. The existing callback body has a single-path `Some(src) = d.data()` copy. Wrap it in the dispatch:

```rust
.process(move |stream, _ud| {
    let Some(mut buf) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buf.datas_mut();
    let Some(d) = datas.first_mut() else { return };

    let chunk = d.chunk();
    let stride = chunk.stride().unsigned_abs();
    let size = chunk.size() as usize;
    let offset = chunk.offset() as usize;

    if size == 0 || stride == 0 {
        return;
    }

    let (w, h) = {
        let g = sz_cb.lock().unwrap_or_else(|e| e.into_inner());
        *g
    };
    let (w, h) = if w == 0 || h == 0 {
        let estimated_h = if stride > 0 {
            (size / stride as usize).max(1) as u32
        } else {
            1080
        };
        let estimated_w = stride / 4;
        tracing::warn!(
            estimated_w,
            estimated_h,
            "geometry unknown from param_changed; falling back to chunk estimate"
        );
        (estimated_w, estimated_h)
    } else {
        (w, h)
    };

    let needed = (stride as usize) * (h as usize);

    // SAFETY: as_raw() on a DataRef is safe; reading the type_ tag is a
    // primitive load.
    let dtype = unsafe { d.as_raw().type_ };

    let (src_vec, copy_size): (Vec<u8>, usize) = match classify_spa_data_type(dtype) {
        DataPath::DmaBuf => {
            // SAFETY: the type_ branch confirmed the SpaData is a DMABUF;
            // map_dmabuf_plane handles fd<0 / map_len==0 / mmap failure
            // and returns Err on all of them.
            let mapped = match unsafe { crate::wayland_portal::dmabuf::map_dmabuf_plane(&&**d) } {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "dmabuf mmap failed; dropping frame");
                    return;
                }
            };
            let bytes = mapped.bytes();
            // chunk.offset is *within the mapping* (after data_off); clamp
            // and copy out into a pool buffer. The single read-side memcpy
            // remains — the zero-copy claim is "no compositor-side memfd
            // serialise + no full-frame intermediate copy". P5C may eliminate
            // this last copy via direct EGL import.
            let end = offset.checked_add(size).unwrap_or(bytes.len()).min(bytes.len());
            let region = &bytes[offset.min(bytes.len())..end];
            let mut v = pool.acquire(needed.max(size));
            v.resize(needed.max(size), 0);
            let n = region.len().min(v.len());
            v[..n].copy_from_slice(&region[..n]);
            // mapped drops here: munmap then auto-close dup'd fd. ✓
            drop(mapped);
            (v, n)
        }
        DataPath::MemFd | DataPath::MemPtr => {
            // Existing path: STREAM_FLAG_MAP_BUFFERS already mmap'd the
            // region for us; read d.data() as a slice.
            let Some(src) = d.data() else { return };
            if src.is_empty() {
                return;
            }
            let mut v = pool.acquire(needed.max(size));
            v.resize(needed.max(size), 0);
            let n = size.min(src.len()).min(v.len());
            v[..n].copy_from_slice(&src[..n]);
            (v, n)
        }
        DataPath::Unknown => {
            tracing::warn!(spa_data_type = dtype, "unsupported SpaData type; dropping frame");
            return;
        }
    };

    let mut dst = src_vec;
    let _ = copy_size; // already applied in-place above

    let frame = RawFrame {
        data: dst,
        width: w,
        height: h,
        stride,
        ts_us: prdt_protocol::now_monotonic_us(),
    };

    match tx_cb.try_send(frame) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(f)) => {
            pool.recycle(f.data);
            tracing::trace!("frame dropped (channel full)");
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
})
```

> **Plan-author note on the `&&**d` cast:** `dmabuf::map_dmabuf_plane` takes
> `&D: SpaDataLike` and we have a `&mut pipewire::spa::buffer::Data<'_>`
> from `datas.first_mut()`. `&**d` dereferences the `&mut Data` to a `Data`
> then reborrows immutably; the outer `&` takes the address as a `&&Data`,
> which is the receiver for the blanket `impl SpaDataLike for &Data`. If
> the compiler rejects the double-borrow, the implementer can extract a
> local `let dref: &pipewire::spa::buffer::Data = &**d;` and pass `&dref`.
> No behavioural difference.

- [ ] **Step 4: Run the dispatch + stream tests + workspace gate**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --lib --target x86_64-unknown-linux-gnu wayland_portal::stream::tests
```

Expected:
```
test wayland_portal::stream::tests::raw_frame_with_padded_stride_validates ... ok
test wayland_portal::stream::tests::buffer_pool_recycles_two_buffers ... ok
test wayland_portal::stream::tests::shutdown_channel_wakes_mainloop_within_deadline ... ok
test wayland_portal::stream::tests::dispatch_table_routes_each_spa_data_type_to_its_arm ... ok
test wayland_portal::stream::tests::build_format_params_then_parse_round_trip_size ... ok
test result: ok. 5 passed; 0 failed
```

```bash
./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green (allowed: pre-existing flaky `transport::probe_test::two_transports_find_each_other`).

- [ ] **Step 5: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/media-linux/src/wayland_portal/stream.rs
git commit -m "$(cat <<'EOF'
P5B-2a T4: stream listener dispatches DMABUF / MemFd / MemPtr

Replace the two T5 staged stubs with real impls:

- `parse_video_format` delegates to `crate::wayland_portal::format::parse`
  and projects NegotiatedFormat → (w, h, PixelFormat). Errors collapse
  to coarse &'static str categories (the full ParseError details surface
  via tracing::warn inside the helper).
- `build_format_params` returns `format::BuiltParams`; the connect site
  calls `as_pods()` on the stack so the byte storage outlives the
  `&mut [&Pod]` borrow handed to `stream.connect`.

`param_changed` now:
  * info!-logs the negotiated (w, h, fmt, modifier);
  * disconnects + warn!s on MOD_INVALID (tiled — cannot CPU-mmap as
    BGRA; renegotiation-with-LINEAR-only retry is a P5B-2a follow-up
    TODO, see spec §4.3);
  * disconnects + warn!s on any ParseError.

`process` callback's body is now a `match classify_spa_data_type(d.as_raw().type_)`:
  * DataPath::DmaBuf → unsafe map_dmabuf_plane → read chunk-bounded
    region from the mapped pointer → pool-acquire dst Vec → row-copy
    → try_send. MappedPlane drops at end of arm (munmap before fd close).
  * DataPath::MemFd / DataPath::MemPtr → existing d.data() slice path,
    unchanged.
  * DataPath::Unknown → warn! + return (drop frame, don't crash).

SPA_DATA_* constants: T4 Step 2 probed pipewire-rs 0.9 for a public
re-export. <Record which path won: pipewire::sys::* | libspa_sys::* |
hand-defined 1/2/4>. ABI verified against
/usr/include/spa-0.2/spa/buffer/buffer.h on Debian bookworm.

Logging cadence: param_changed fires info!-level on every call. If
GNOME re-issues on monitor reconfigure and smoke shows spam, gate
behind a Once. Tracked as a known follow-up risk (spec §9 open question 3).

- 2 new tests:
  - dispatch_table_routes_each_spa_data_type_to_its_arm: pure classifier
    table-test against SPA_DATA_DMABUF / MEMFD / MEMPTR / unknown tag.
  - build_format_params_then_parse_round_trip_size: integration smoke
    asserting build_format_params yields exactly one POD (full
    negotiated-side parse is covered in format::tests).
EOF
)"
```

---

## Task 5: STATUS.md entry + smoke walkthrough doc + final gate

**Files:**
- Modify: `docs/superpowers/STATUS.md`
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md`

- [ ] **Step 1: Append `## P5B-2a` section to `p5b1-smoke-walkthrough.md`**

Edit `docs/superpowers/p5b1-smoke-walkthrough.md`. Append after the existing "Known issues / follow-ups" section:

```markdown
---

## P5B-2a — DMABUF zero-copy + libspa POD negotiation

The `phase-p5b2a-libspa-pod-dmabuf-complete` tag replaces the two T5 staged
stubs in `wayland_portal/stream.rs` with real libspa POD build + parse
and adds a DMABUF receive path. Sections D + E below extend the
P5B-1 walkthrough; Sections A / A' / B / C remain unchanged.

### Section D — GNOME DMABUF smoke (real compositor + zero-copy verified)

**Pre-conditions:**
- Ubuntu 24.04+ GNOME (Wayland session) with `libpipewire-0.3 >= 0.3.55`.
- `prdt-host` binary from this branch (container build per Section A).
- `xdg-desktop-portal` ≥ 1.18 (DMABUF advertising landed in 1.16+).

**Steps:**

1. Start the host with debug tracing on the negotiation lines:

   ```bash
   RUST_LOG=info,prdt_media_linux::wayland_portal=debug \
       ./prdt-host --bitrate-mbps 5 --silent-allow --headless \
       2>&1 | tee p5b2a-gnome-dmabuf-run.log
   ```

2. Click **Allow** in the consent dialog (first run only — Section A's
   token reuse path applies as before).

3. Expect the negotiation log line shortly after the dialog closes:

   ```
   pipewire negotiated format w=1920 h=1080 fmt=BGRA modifier=Some(0)
   ```

   The `modifier=Some(0)` value is `DRM_FORMAT_MOD_LINEAR` — the
   compositor handed us a CPU-readable DMABUF.

4. Connect a viewer. Confirm frames flow at ≥ 20 fps after first IDR.

5. **CPU usage check** (the zero-copy payoff):

   ```bash
   pidstat -p $(pgrep -f prdt-host) 1 30
   ```

   Expected: sustained capture at 1080p60 with `%CPU` noticeably below
   the P5B-1 successor's MemFd baseline. The exact delta is environment-
   dependent; the qualitative signal is that the DMABUF arm fires (no
   per-frame compositor-side memfd serialise + read-side memcpy of the
   full framebuffer; only the single pool-buffer fill remains).

6. **Verify the DMABUF arm is firing** (rather than the MemFd fallback)
   by temporarily raising verbosity at the dispatch seam:

   ```bash
   RUST_LOG=info,prdt_media_linux::wayland_portal::stream=trace ./prdt-host …
   ```

   You should NOT see `unsupported SpaData type` lines. If you see only
   `frame dropped (channel full)` lines and no warn!, the dispatch is
   silent (correct).

### Section E — MemFd fallback regression (older compositor)

**Pre-conditions:**
- A compositor that does NOT advertise DMABUF support — older
  `xdg-desktop-portal` (≤ 1.14) or a deliberate `xdg-desktop-portal-wlr`
  configured without the dmabuf module.
- Same `prdt-host` binary.

**Steps:**

1. Start the host as in Section D.

2. Expect the negotiation log to show:

   ```
   pipewire negotiated format w=… h=… fmt=BGRA modifier=None
   ```

   The `modifier=None` indicates no VideoModifier was on the negotiated
   POD; the compositor will deliver MemFd or MemPtr.

3. Connect a viewer; frames continue to flow. The dispatch hits the
   `MemFd` / `MemPtr` arm (existing P5B-1 path), not DMABUF.

4. Confirm `RUST_LOG=…wayland_portal::stream=trace` shows no
   `dmabuf mmap failed` warns and no `unsupported SpaData type` warns.

### Section F — DRM_FORMAT_MOD_INVALID handling (synthetic / future)

If a compositor selects `DRM_FORMAT_MOD_INVALID` (tiled, not CPU-readable):

```
pipewire negotiated format w=… h=… fmt=BGRA modifier=Some(-1)
compositor selected DRM_FORMAT_MOD_INVALID (tiled); disconnecting stream. TODO(P5B-2a follow-up): renegotiate with LINEAR-only modifier list.
```

…and the producer surfaces `Capture(linux-wayland-portal: PipeWire channel closed (mainloop exited))` on the next `next_frame()`. The host's outer session loop tears down the producer and falls back to the X11 path on the next reconnect (or stays disconnected if `--capture-backend wayland` is forced). Renegotiation auto-retry with LINEAR-only is **deferred to a P5B-2a follow-up** — flagged in code as a `TODO(P5B-2a follow-up)`.

### Out of scope (deferred to P5B-2b / P5C)

- Cursor metadata (`Cursor::Metadata` mode 4) — P5B-2b.
- KDE / Sway / Hyprland smoke matrix — P5B-2b.
- Explicit sync (`SPA_META_SyncTimeline` + `SPA_DATA_SyncObj`) — P5B-3+.
- NV12 multi-plane — P5C (lands with the HW encoder).
- EGL import / GPU readback / Vulkan — P5C.
- `/dev/dri/card0` direct ioctl — never (portal handles allocation).

### Known issues / follow-ups (P5B-2a specific)

- **MOD_INVALID renegotiation auto-retry:** currently a graceful
  disconnect + log; no auto-retry with a narrower modifier list. Flagged
  as `TODO(P5B-2a follow-up)` in `wayland_portal/stream.rs`. Real
  compositor data needed before deciding the right strategy
  (xdg-desktop-portal-wlr / OBS Studio's approach is to re-`connect()`
  with `[LINEAR]` only and warn if that also returns MOD_INVALID).
- **`param_changed` logging cadence:** `info!`-level on every
  `param_changed` call. If GNOME re-issues on monitor reconfigure and
  smoke shows spam, gate behind `std::sync::Once`. Not pre-emptively
  gated (spec §9 open question 3).
- **Single read-side memcpy remains:** the DMABUF arm still copies once
  from the mapped pointer into a pool-acquired `Vec<u8>` so the existing
  channel-bound `RawFrame` API is unchanged. P5C may eliminate this last
  copy via direct EGL import or GPU readback.
```

- [ ] **Step 2: Update `STATUS.md` header + append the P5B-2a entry**

Edit `docs/superpowers/STATUS.md`. Change the header lines (currently `**Latest tag:** phase-p5b1-t5-t6-pipewire-runtime-complete`):

```markdown
**Last updated:** 2026-05-12
**Latest tag:** `phase-p5b2a-libspa-pod-dmabuf-complete`
```

Append immediately after the existing **P5B-1 successor (`phase-p5b1-t5-t6-pipewire-runtime`, 2026-05-12)** entry (the line that starts at STATUS.md:257 in current state):

```markdown
- **P5B-2a (`phase-p5b2a-libspa-pod-dmabuf-complete`, 2026-05-12)**:
  libspa POD negotiation + DMABUF zero-copy capture path.
  - Replaces the two P5B-1-successor T5 stubs (`parse_video_format` /
    `build_format_params`) with real libspa POD build + parse via new
    `crates/media-linux/src/wayland_portal/format.rs` (`BuiltParams { bytes }`
    + `as_pods(&self) -> Vec<&Pod>` + `pub fn build()` + `pub fn parse()` +
    typed `ParseError`). `build()` advertises BGRA/BGRx + size range
    (320×240..7680×4320, default 1920×1080) + framerate range (15/1..60/1,
    default 60/1) + modifier enum (`DRM_FORMAT_MOD_LINEAR | DRM_FORMAT_MOD_INVALID`).
  - New `crates/media-linux/src/wayland_portal/dmabuf.rs`:
    `pub unsafe fn map_dmabuf_plane<D: SpaDataLike>(&D) -> io::Result<MappedPlane>`
    `mmap(PROT_READ, MAP_PRIVATE)`s a `F_DUPFD_CLOEXEC`-dup'd FD.
    `MappedPlane { _fd: OwnedFd, ptr, len, data_off }` — `_fd` declared FIRST
    so the explicit `Drop` impl's `munmap` runs BEFORE `OwnedFd::drop` closes
    the dup'd FD. `trait SpaDataLike` exposes `fd / maxsize / mapoffset`
    so unit tests inject a stub without constructing a `spa_data` (private
    in pipewire-rs 0.9). Production blanket impl on `&pipewire::spa::buffer::Data`.
  - `wayland_portal/stream.rs`: `process()` callback gains a
    `match classify_spa_data_type(d.as_raw().type_)` arm:
    - `SPA_DATA_DmaBuf` → `unsafe { map_dmabuf_plane }` → read chunk-bounded
      region → pool-buffer fill → `try_send`. No compositor-side memfd
      serialise; no full-framebuffer intermediate copy.
    - `SPA_DATA_MemFd | SPA_DATA_MemPtr` → existing `d.data()` slice path
      (unchanged P5B-1 successor behaviour).
    - Unknown → warn + drop frame.
    `param_changed` now `info!`-logs the negotiated `(w, h, fmt, modifier)`
    and disconnects + warns on `DRM_FORMAT_MOD_INVALID` (tiled, not
    CPU-readable; renegotiation auto-retry is a follow-up TODO).
  - **No new workspace deps** — reuses existing `pipewire = "0.9"` POD
    builder, `libc`, `tracing`, `thiserror`.
  - **Constraints baked in**: BGRA/BGRx only (NV12 rejected as
    `UnsupportedFormat`); implicit sync only (no `SPA_META_SyncTimeline`);
    modifier list `[LINEAR=0, INVALID=-1]` (MOD_INVALID disconnects;
    renegotiation deferred); always-on (no feature flag — MemFd is the
    automatic fallback for compositors that don't advertise DMABUF).
  - **Tests**: 1 format::build (round_trip_bgra) + 4 format::parse
    (round-trip / rejects Audio / rejects NV12 / extracts modifier) +
    3 dmabuf (pattern read+drop / dup keeps original / fd=-1 → Err) +
    2 stream dispatch (classifier table / build_format_params yields one POD)
    = **10 new tests**. Container clippy + `cargo test --workspace --lib --target x86_64-unknown-linux-gnu`
    green (pre-existing flaky `transport::probe_test::two_transports_find_each_other`
    unchanged).
  - **Out of scope (deferred)**: cursor metadata (P5B-2b), multi-compositor
    smoke matrix KDE/Sway/Hyprland (P5B-2b), explicit sync (P5B-3+),
    NV12 multi-plane (P5C), EGL/Vulkan import (P5C), MOD_INVALID
    renegotiation auto-retry (P5B-2a follow-up; currently disconnect + log).
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §P5B-2a Section D (GNOME DMABUF zero-copy) + Section E (MemFd
    fallback regression) + Section F (MOD_INVALID handling).
```

- [ ] **Step 3: Final pre-merge gate**

```bash
./scripts/dev-container.sh cargo fmt --all
./scripts/dev-container.sh cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --workspace --lib --target x86_64-unknown-linux-gnu
```

Expected: green. The only allowed failure is the pre-existing flaky
`transport::probe_test::two_transports_find_each_other`. **Do not
ship if any new test fails.**

- [ ] **Step 4: Confirm the X11 contract test still passes (P5B-1 T1 trait regression guard)**

```bash
./scripts/dev-container.sh cargo test -p prdt-media-linux --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: 3 pass, 1 ignored (X11 needs a real X server).

- [ ] **Step 5: Commit STATUS + walkthrough**

```bash
git add docs/superpowers/STATUS.md docs/superpowers/p5b1-smoke-walkthrough.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record P5B-2a libspa pod + DMABUF zero-copy

Adds the phase-p5b2a-libspa-pod-dmabuf-complete entry under §1 with
test counts (10 new), scope summary (BGRA/BGRx only, implicit sync,
modifier=[LINEAR, INVALID], MOD_INVALID disconnects), and pointers to
the smoke walkthrough's new Section D (GNOME DMABUF zero-copy),
Section E (MemFd fallback regression), and Section F (MOD_INVALID
handling). Out-of-scope list defers cursor metadata, multi-compositor
matrix, explicit sync, NV12 multi-plane, EGL/Vulkan, and MOD_INVALID
auto-retry renegotiation to P5B-2b / P5B-3 / P5C / follow-up.
EOF
)"
```

- [ ] **Step 6: No PR creation — controller's job**

Per task instructions: stop here. Do **not** push the branch, do **not**
open a PR, do **not** tag. The plan controller handles the PR + tag
sequence once auto-evidence (container clippy + tests) is collected.

---

## Cross-task notes

- **Container-only build:** every cargo invocation runs inside the dev container via `./scripts/dev-container.sh`. The Ubuntu 22.04 host's libpipewire 0.3.48 is too old for pipewire-rs 0.9 (needs ≥ 0.3.55, Debian bookworm ships 0.3.65). Build artifacts land in `target-docker/` and don't touch the host's `target/`.
- **Pre-existing flaky test:** `transport::probe_test::two_transports_find_each_other` is non-deterministic and unrelated to P5B-2a. Do not treat as a regression. (Documented in STATUS L2 entry.)
- **`pipewire 0.9` API verification:** P5B-1 T5 Step 1 already recorded the 0.9.2 vs 0.8 module-path moves in `stream.rs`'s top comment. P5B-2a only adds POD-builder / POD-deserialiser usage (`pipewire::spa::pod::serialize::PodSerializer` + `deserialize::PodDeserializer`); these are stable across the 0.9.x range. If the implementer finds a builder method renamed, correct in-place and document in the commit body.
- **No new workspace deps:** the spec is explicit. Don't add `libspa`, `libspa-sys`, `nix`, or any DRM crate. `libc` covers `mmap`/`munmap`/`fcntl`/`memfd_create`/`ftruncate`/`write`/`close`/`fstat`; pipewire-rs's transitive `libspa-sys` (if present) is reachable as the secondary import path for `SPA_DATA_*` constants.
- **`F_DUPFD_CLOEXEC` discipline:** every dmabuf FD that leaves the PipeWire callback is dup'd via `fcntl(F_DUPFD_CLOEXEC, 3)`. The original FD's lifetime is the callback's stack frame; the dup is owned by `MappedPlane._fd: OwnedFd` and dropped when `MappedPlane` drops. Test 2 (`map_dmabuf_plane_dup_keeps_original_fd_alive_after_drop`) is the regression guard.
- **`MappedPlane` Drop order:** struct field declaration order drives drop order. `_fd: OwnedFd` is the FIRST field; the explicit `Drop` impl calls `munmap` (which doesn't touch the FD), then auto-Drop closes `_fd` (kernel decrements the dmabuf ref). Reversing the field order would still be sound (mmap holds an independent ref against the underlying object) but pointlessly confusing — the order documents intent. Spec risk #5.
- **MOD_INVALID renegotiation:** spec §4.3 calls for graceful disconnect + log in P5B-2a; auto-retry with a LINEAR-only modifier list is a follow-up. The `TODO(P5B-2a follow-up)` marker in `stream.rs::param_changed` is the future hook. Real-machine smoke (Section F, deferred) is the trigger for prioritising the follow-up.
- **Logging cadence:** `param_changed` fires `info!` once per call. If GNOME re-issues `param_changed` frequently (e.g. monitor reconfigure) and smoke shows spam, gate behind `std::sync::Once`. Not pre-emptively gated per spec §9 open question 3.
- **Single read-side memcpy:** the DMABUF arm still performs ONE memcpy from the mapped pointer into a `FramePool`-acquired `Vec<u8>` so the existing channel-bound `RawFrame` API is unchanged. The "zero-copy" claim is "no compositor-side memfd serialise + no full-framebuffer intermediate copy" — not "literally zero copies in the entire pipeline". P5C can eliminate this last copy via direct EGL import / GPU readback.
- **Always-on, no feature flag:** P5B-2a does not introduce a `dmabuf` Cargo feature. The DMABUF arm fires when the compositor advertises it; the MemFd/MemPtr arms remain as the automatic fallback. This is the same pattern OBS Studio + xdg-desktop-portal-wlr use.
- **WSLg X11 path unchanged:** zero touch in `crates/media-linux/src/x11_capture.rs`, `capture_source.rs`, `linux_sw_producer.rs`, `policy.rs`, or any host-side / capturer.rs / session.rs / token.rs file. The P5B-1 X11 contract test (`tests/capture_source_contract.rs`) is the regression guard.

---

## Ambiguities resolved (spec didn't cover; plan author chose)

1. **`SpaData` test-injection approach (spec §9 open question 1):** T3 Step 1 probes pipewire-rs 0.9's `pipewire::spa::buffer::Data` for a public constructor; if none exists (the plan author's prediction matching 0.8 behaviour), the trait `SpaDataLike { fd, maxsize, mapoffset }` fallback is taken. Production blanket impl on `&Data`; test `TestData` stub carries the three primitive fields. Decision is documented in the module header of `dmabuf.rs` and the commit body. If a future pipewire-rs release exposes a constructor, the trait can be retired without touching call sites.
2. **`SPA_DATA_*` constant import path (spec §9 open question 2):** T4 Step 2 probes in order: `pipewire::sys::*` → `libspa_sys::*` → hand-define against the C ABI (`SPA_DATA_DmaBuf = 4`, `SPA_DATA_MemFd = 2`, `SPA_DATA_MemPtr = 1`, verified against `/usr/include/spa-0.2/spa/buffer/buffer.h` on Debian bookworm). The local module exports `SPA_DATA_DMABUF` / `SPA_DATA_MEMFD` / `SPA_DATA_MEMPTR` in screaming-snake regardless of source, so the dispatch classifier and its tests are insulated from the upstream casing. Chosen path is recorded in `stream.rs`'s top comment + commit body.
3. **`parse_video_format` return-type widening:** the existing function signature returns `Result<(u32, u32, PixelFormat), &'static str>`. The new `format::parse` returns `Result<NegotiatedFormat, ParseError>` which carries strictly more information (framerate + modifier). The plan keeps the inner `parse_video_format` wrapper signature unchanged (to minimise churn at the `param_changed` call site) and reads the extra fields directly from `format::parse` inside `param_changed` — that's where the MOD_INVALID arm lives. A future cleanup may widen the wrapper.
4. **Choice unwrapping on the negotiated POD:** spec §3.2 noted compositors typically emit plain `Value::Id(...)` / `Value::Rectangle(...)` on the negotiated `SPA_PARAM_Format`. The `format::parse` impl defends against a re-Choice'd negotiated POD by reading `prop.value` directly and matching arms; the helper `unwrap_choice_default` is a documentation seam for the rare case. If smoke surfaces a compositor that does re-Choice, extend the match arms inline. Not pre-emptively gated.
5. **`build_format_params` return-type change:** P5B-1 successor's `build_format_params` returned `Vec<pipewire::spa::pod::Pod>` (empty); the new version returns `BuiltParams` so the byte storage lives at the connect site. The single caller (in `loop_thread_main`) is updated in the same commit as the body swap (T4 Step 3 (c)). No external callers exist (the function is module-private).
6. **DMABUF arm copy semantics:** spec §3.5 was explicit that `RawFrame::from_slice` still performs ONE pool-buffer memcpy from the mapped region. The plan stays true to that — the existing `FramePool::acquire` → `copy_from_slice` shape is preserved. The "zero-copy" payoff is the absence of a compositor-side memfd serialise + a per-frame full-framebuffer intermediate; the producer-side BGRA→I420 conversion is unchanged.
7. **Test for `MappedPlane` drop via `madvise(MADV_DONTNEED)` (spec §5.1 #5):** the spec's invariant probe (calling `madvise` on a stale pointer expecting `EFAULT`) is a portable but indirect signal. The plan's test #1 (`map_dmabuf_plane_reads_known_pattern_and_drops_cleanly`) instead relies on the drop running without panicking + the original FD being closeable afterwards. If a future regression silently skips the `munmap`, the leak surfaces in real-machine smoke (RES grows under sustained capture) and a follow-up can add the `madvise` probe as a stricter invariant test.
