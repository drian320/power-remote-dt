# P5B-2a-successor — libspa POD wire-format rewrite (Wayland portal EnumFormat)

**Status**: design — `phase-p5b2a-successor-libspa-pod-rewrite`
**Spec date**: 2026-05-13
**Predecessor**: P5B-2a (`docs/superpowers/specs/2026-05-12-p5b2a-libspa-pod-dmabuf-design.md`)
**Successor of**: real-device smoke finding during P5C-1 (commit `1a94809`, master)

## 1. Goal

Rewrite the outbound `SPA_PARAM_EnumFormat` POD construction in
`crates/media-linux/src/wayland_portal/format.rs::build()` against a thin,
safe Rust wrapper over `libspa-sys` raw `spa_pod_builder_*` FFI, replacing
the current `pipewire::spa::pod::Object` + `PodSerializer` path that
GNOME 46 mutter rejects at the wire level with `invalid message received
0 for 2: Invalid argument` / `no more input formats`.

**End-to-end success criterion**: N100 / Ubuntu 24.04 / GNOME 46 Wayland
session reaches `pipewire stream: state Streaming` (not `→ Error`) after
the portal handshake, `WaylandPortalCapturer` emits BGRA frames to the
P5C-1 VAAPI encoder at ≥ 30 fps, and viewer shows live desktop content.

**Out of scope**

- `SPA_PARAM_Buffers` POD rewrite. The current `Buffers` path uses the
  same `Object` + `PodSerializer` machinery but has not been observed to
  fail on GNOME 46 — the wire-error happens before `param_changed`
  delivers a negotiated Format. We defer Buffers to the next branch if
  the EnumFormat fix alone proves insufficient.
- `FrameInput::Dmabuf` integration / DMABUF zero-copy — defer to P5C-2.
- Multi-compositor smoke matrix (KDE / Sway / Hyprland) — defer to P5C
  follow-up phases. Verification is GNOME 46 mutter only.
- Replacing `parse()` — the inbound negotiated Format POD from the
  compositor is well-formed; only the *outbound* EnumFormat is broken.

## 2. Why this rewrite is necessary

P5C-1 real-device smoke (2026-05-13, N100 Intel Alder Lake-N iGPU,
Ubuntu 24.04 GNOME 46 Wayland) confirmed that the P5C-1 VAAPI encoder
path is fully operational up to `encoder ready backend="linux-vaapi-h264"`.
The blocking failure is upstream in the screencast portal flow:

```text
pipewire stream: state Unconnected → Connecting → Paused → Error
pipewire stream: error invalid message received 0 for 2: Invalid argument
                no more input formats
```

Five iterations attempted in PR #15 (commits `df49812..e5515b5`) did
NOT resolve the wire-level rejection:

1. Omit `VideoModifier` from EnumFormat (`f86ed1a`)
2. Set `MANDATORY | DONT_FIXATE` on Choice props (`e5515b5`)
3. Expand `VideoFormat` alternatives from 2 (BGRA/BGRx) to 8
   (BGRA/BGRx/RGBA/RGBx/ARGB/ABGR/xRGB/xBGR)
4. Filter `param_changed` by `id` before format parse (`bf2f30a`)
5. Add `state_changed` listener for diagnosis visibility (`29a7afc`)

All adjustments succeeded on the Rust *structural* level
(`build_choice_properties_have_mandatory_dont_fixate_flags` and
`build_video_format_alternatives_cover_bgra_rgba_family` both pass), yet
mutter rejects the serialised bytes. This isolates the bug to
`pipewire::spa::pod::serialize::PodSerializer`'s encoding of
`Object` / `Choice` / `Property` flags — specifically the bytes that
encode `flags`, `pad`/`padding`, and the `Choice` header (type / flags /
child-size / child-type quartet defined by SPA POD format spec).

Reference implementations (gnome-remote-desktop, obs-pipewire-screencast,
xdg-desktop-portal-wlr) all build the same EnumFormat via libspa's
inline C helpers (`spa_pod_builder_*` / `spa_format_video_raw_init`).
None use the Rust pipewire-rs `Object` builder for non-trivial pods.
The pipewire-rs Object/Value/Choice serializer path is plausibly fine
for round-trip-against-itself test cases (which our existing 8
`format.rs` tests are) but diverges from the bytes that mutter accepts
on the wire. We treat this as an upstream-library gap and route around
it locally rather than waiting for an upstream fix.

We therefore drop the `PodSerializer` path for EnumFormat and use raw
`spa_pod_builder_*` FFI directly, isolating the unsafe boundary in a
new safe Rust wrapper (`pod_builder.rs`).

## 3. Architecture

### 3.1 New module — `wayland_portal/pod_builder.rs`

Single struct exposing a safe Rust facade over libspa-sys raw FFI:

```rust
pub struct PodBuilder {
    buf: Vec<u8>,                                  // owned backing storage
    raw: spa_sys::spa_pod_builder,                 // libspa state (offset, callbacks)
    frames: Vec<spa_sys::spa_pod_frame>,           // open object / array stack
}

impl PodBuilder {
    pub fn new() -> Self;
    pub fn push_object(&mut self, type_id: u32, prop_key: u32) -> ObjectScope<'_>;
    pub fn add_id(&mut self, key: u32, id: u32);
    pub fn add_int(&mut self, key: u32, value: i32);
    pub fn add_rectangle(&mut self, key: u32, width: u32, height: u32);
    pub fn add_fraction(&mut self, key: u32, num: i32, denom: i32);
    pub fn add_choice_id_enum(
        &mut self, key: u32, flags: u32, default: u32, alternatives: &[u32],
    );
    pub fn add_choice_rectangle_range(
        &mut self, key: u32, flags: u32,
        default: (u32, u32), min: (u32, u32), max: (u32, u32),
    );
    pub fn add_choice_fraction_range(
        &mut self, key: u32, flags: u32,
        default: (i32, i32), min: (i32, i32), max: (i32, i32),
    );
    pub fn finish(self) -> Vec<u8>;
}

pub struct ObjectScope<'a> { /* &'a mut PodBuilder */ }
impl Drop for ObjectScope<'_> { /* spa_pod_builder_pop */ }
```

- All `add_*` and `push_object` methods are safe; unsafe `spa_pod_builder_*`
  calls live behind `// SAFETY:` block-level docstrings inside the module,
  matching the convention established by `wayland_portal/cursor.rs`.
- `ObjectScope` is an RAII guard that calls `spa_pod_builder_pop` on drop,
  so frame stack imbalance is structurally prevented at the type level.
- `PodBuilder::new` installs an `spa_pod_builder_callbacks` overflow
  handler that does `Vec::reserve` + re-points `raw.data` to the new
  buffer. This is the same pattern libspa uses internally and matches
  the reference impl in `pipewire-utils.c`.
- `finish` truncates `self.buf` to the actual POD size
  (`raw.offset as usize`) and returns the owned `Vec<u8>`.

### 3.2 Rewrite — `format.rs::build()`

`build()`'s signature and return type (`BuiltParams`) are unchanged so
the caller in `stream.rs` is not touched. Body switches from
`Object { properties: vec![Property { key, flags, value: Choice(...) }, ...] }`
+ `PodSerializer::serialize` to a sequenced `PodBuilder` script:

```rust
pub fn build() -> BuiltParams {
    use pipewire::spa::sys as spa_sys;
    const F_MANDATORY: u32 = spa_sys::SPA_POD_PROP_FLAG_MANDATORY;
    const F_DONT_FIXATE: u32 = spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE;
    let choice_flags = F_MANDATORY | F_DONT_FIXATE;

    let mut b = PodBuilder::new();
    {
        let mut o = b.push_object(
            spa_sys::SPA_TYPE_OBJECT_Format,
            spa_sys::SPA_PARAM_EnumFormat,
        );
        o.add_id(spa_sys::SPA_FORMAT_mediaType,    spa_sys::SPA_MEDIA_TYPE_video);
        o.add_id(spa_sys::SPA_FORMAT_mediaSubtype, spa_sys::SPA_MEDIA_SUBTYPE_raw);
        o.add_choice_id_enum(
            spa_sys::SPA_FORMAT_VIDEO_format, choice_flags,
            spa_sys::SPA_VIDEO_FORMAT_BGRA,
            &[
                spa_sys::SPA_VIDEO_FORMAT_BGRA, spa_sys::SPA_VIDEO_FORMAT_BGRx,
                spa_sys::SPA_VIDEO_FORMAT_RGBA, spa_sys::SPA_VIDEO_FORMAT_RGBx,
                spa_sys::SPA_VIDEO_FORMAT_ARGB, spa_sys::SPA_VIDEO_FORMAT_ABGR,
                spa_sys::SPA_VIDEO_FORMAT_xRGB, spa_sys::SPA_VIDEO_FORMAT_xBGR,
            ],
        );
        o.add_choice_rectangle_range(
            spa_sys::SPA_FORMAT_VIDEO_size, choice_flags,
            (1920, 1080), (320, 240), (7680, 4320),
        );
        o.add_choice_fraction_range(
            spa_sys::SPA_FORMAT_VIDEO_framerate, choice_flags,
            (60, 1), (15, 1), (60, 1),
        );
    } // ObjectScope::drop → spa_pod_builder_pop

    BuiltParams { bytes: vec![b.finish()] }
}
```

### 3.3 `Cargo.toml` change

`crates/media-linux/Cargo.toml`: add `libspa-sys = { version = "0.9" }`
as an explicit direct dependency (currently transitive through
`pipewire`). The `v0_3_33` feature on `pipewire` is retained because
`stream.rs` still uses pipewire-rs typed wrappers for stream
construction.

### 3.4 `parse()` — unchanged

The negotiated `SPA_PARAM_Format` POD that arrives via `param_changed`
is well-formed (compositor-generated). The existing `parse()` continues
to use `PodDeserializer` + pipewire-rs typed wrappers. The asymmetry
(custom FFI builder for outbound, library wrapper for inbound) is
intentional and scoped — symmetric rewrite is deferred unless future
breakage proves it necessary.

## 4. Verification

### 4.1 Golden-bytes regression test (CI gate)

`crates/media-linux/tests/format_golden.rs`:

- Loads `crates/media-linux/tests/fixtures/enum_format_golden.bin` (a
  fixture committed to the repo)
- Asserts `format::build().bytes[0] == fixture` byte-for-byte
- The fixture is captured one-shot via a small `examples/dump_pod.rs`
  binary built on top of the SAME `PodBuilder` and committed alongside
  the test. The fixture's authority is "the bytes our builder produces
  the moment we verified end-to-end success on the N100" — not an
  external reference. Cross-checked against the POD format spec
  comments in `libspa/pod/builder.h` and against a hexdump from
  `pw-cli dump` of a known-good EnumFormat to be sure it conforms to
  the SPA POD wire spec.
- Catches any wire-format drift on every `cargo test --workspace` run
  with zero environment dependency (no Wayland, no DRI, no pipewire
  daemon needed).

This is the load-bearing CI guard — the bug we are fixing is exactly
the kind that only manifests at the byte level, so we test at the byte
level.

### 4.2 N100 GNOME 46 Wayland smoke (manual; documented)

New section `§L` added to `docs/superpowers/p5b1-smoke-walkthrough.md`:

1. Verify GNOME 46 Wayland session: `echo $XDG_SESSION_TYPE` → `wayland`.
2. Start host with explicit Vaapi: `./prdt host --encoder vaapi
   --bitrate-mbps 5 --silent-allow 2>&1 | tee p5b2as.log`
3. Accept the portal consent prompt.
4. Expect log line transitions: `Unconnected → Connecting → Paused →
   Streaming` (NOT `→ Error`).
5. Confirm `param_changed` delivers a valid Format POD: log line
   `param_changed: SPA_PARAM_Format negotiated: BGRA 1920x1080 60/1`.
6. Connect viewer: `./prdt connect --host <ip>:9000 --decoder openh264
   --codec h264`.
7. Confirm frame flow at ≥ 30 fps in viewer with live desktop content.
8. `pidstat -p $(pgrep -f prdt) 1 30`: host %CPU significantly below
   the OpenH264 SW baseline (target: < 10 % at 1080p60).
9. Cleanup: viewer Ctrl+C, host watchdog kills session within 5 s.

The walkthrough explicitly notes this is GNOME-only — KDE / Sway /
Hyprland verification is deferred.

## 5. Components / interfaces

| File | Change | LOC estimate |
|---|---|---|
| `crates/media-linux/Cargo.toml` | + `libspa-sys = "0.9"` direct dep | +1 line |
| `crates/media-linux/src/wayland_portal/mod.rs` | + `pub mod pod_builder;` | +1 line |
| `crates/media-linux/src/wayland_portal/pod_builder.rs` | new module | ~220 |
| `crates/media-linux/src/wayland_portal/format.rs::build()` | rewrite body, keep signature | ~50 lines changed (-80 +50 net) |
| `crates/media-linux/examples/dump_pod.rs` | one-shot fixture capture binary | ~30 |
| `crates/media-linux/tests/format_golden.rs` | new golden test | ~30 |
| `crates/media-linux/tests/fixtures/enum_format_golden.bin` | binary fixture | ~256 B |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | + §L | ~80 lines |
| `docs/superpowers/STATUS.md` | update §P5C-1 known-issue + new entry | ~20 lines |

Total: ~470 LoC + 256 B binary, 6-8 days.

## 6. Error handling

- `PodBuilder::add_*` cannot fail at user-visible API level. Internal
  `Vec` reallocation panics on OOM — same contract as the current
  `PodSerializer::serialize.expect("only fails on OOM")`. The single
  user-visible failure surface is `PodBuilder::finish` which is
  infallible (returns owned `Vec<u8>`).
- `spa_pod_builder_pop` returning a null pod inside `ObjectScope::drop`
  would indicate FFI misuse (frame stack underflow). Handled via
  `debug_assert!(!pod.is_null())` in dev builds, silent drop in
  release — same convention as `cursor.rs`.
- Overflow callback (`spa_pod_builder_callbacks::overflow`): re-allocs
  `self.buf` with double capacity, re-points `raw.data` to the new
  buffer. Cannot fail except via OOM (Rust allocator panics).
- No new failure modes surface to `stream.rs`. The caller's existing
  error-handling path (`build()` returns `BuiltParams`, never `Result`)
  is preserved.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `spa_pod_builder` ABI / struct layout drift in a future libspa-sys bump | low | Pin `libspa-sys = "0.9"`. Add a `const _: () = assert!(mem::size_of::<spa_pod_builder_state>() == 16, ...)` compile-time check inside `pod_builder.rs`. No new crate dep needed. |
| Golden fixture captures a still-broken POD ("we verified what we built, not what mutter expects") | medium | After T5 (build() rewrite), run the N100 smoke FIRST. Only commit the fixture after observing `state: Streaming`. The fixture's authority is the live-smoke success, not the byte-pattern itself. |
| pipewire-rs 0.9.x eventually ships a working `Object`/`Choice` serializer and makes our wrapper redundant | low | Acceptable. The `PodBuilder` wrapper is contained; collapsing back to pipewire-rs is a one-file change in a follow-up. |
| `spa_pod_builder_add_object` style varargs C signature is hard to bind cleanly | low | We bind only the primitive `push_object` / `pop` / `add_*` calls, never the varargs `add_object` convenience macro. Each property is added individually. This is the same pattern OBS uses. |
| Container `cargo test` can't run the golden test without libspa headers | low | `libspa-sys` is already in the dev container (pulled by `pipewire` for P5B-2a). No new system dep. |

## 8. Cross-platform CI bar

- Linux clippy must stay green (same bar as L0-L4 + P5A + P5B-1 + P5B-2a + P5C-1).
- Windows clippy unaffected — the entire `wayland_portal/` tree is gated
  on `#![cfg(target_os = "linux")]`.
- Container `cargo test --workspace` must run the new `tests/format_golden.rs`
  and pass.
- Existing tests must all stay green: 25 P5C-1 unit tests +
  ~30 wayland_portal tests + workspace baseline.

## 9. DoD checklist

- [ ] `build()` rewritten on `PodBuilder`; signature unchanged
- [ ] `pod_builder.rs` covers `push_object`, primitives (Id / Int /
      Rectangle / Fraction), and the 3 Choice helpers needed by `build()`
- [ ] All 3 existing `format.rs` build-side tests still pass without
      modification (`round_trip_bgra`,
      `build_choice_properties_have_mandatory_dont_fixate_flags`,
      `build_video_format_alternatives_cover_bgra_rgba_family`)
- [ ] All 5 existing `format.rs` parse-side tests still pass
- [ ] New `tests/format_golden.rs` passes against committed fixture
- [ ] Compile-time size assertion on `spa_pod_builder_state` in `pod_builder.rs`
- [ ] N100 GNOME 46 Wayland smoke reaches `state: Streaming` + ≥ 30 fps
      BGRA frames + viewer shows live desktop
- [ ] Walkthrough §L documented; STATUS.md §P5C-1 known-issue resolution
      note + new §P5B-2a-successor entry
- [ ] Cross-platform CI (Linux + Windows) green
- [ ] No new clippy warnings; no `#[allow(...)]` added without justification

## 10. Follow-ups (out of scope)

- `SPA_PARAM_Buffers` POD rewrite if EnumFormat alone proves insufficient
  on KDE / Sway / Hyprland (P5B-2a-successor-2 or rolled into P5C-2)
- `FrameInput::Dmabuf` arm + EGL/mmap import for true zero-copy (P5C-2)
- Multi-compositor smoke matrix (P5C / P5C-3)
- pipewire-rs upstream contribution: fix the `Object`/`Choice`
  serializer so future projects don't hit this. Out of scope for this
  spec; tracked as a "would be nice" item in STATUS.

## 11. Open questions

None at spec time.
