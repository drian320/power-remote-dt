# P5B-2a-successor Implementation Plan — libspa POD wire-format rewrite

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`. Steps use checkbox (`- [ ]`).

**Goal:** Rewrite `wayland_portal::format::build()` against `libspa-sys` raw FFI through a new safe `PodBuilder` wrapper, replacing the `PodSerializer` path that GNOME 46 mutter rejects with `invalid message received 0 for 2`. Outcome: N100 GNOME 46 Wayland session reaches `pipewire stream: state Streaming` and feeds BGRA frames to the P5C-1 VAAPI encoder.

**Architecture:** New `crates/media-linux/src/wayland_portal/pod_builder.rs` — RAII wrapper over `spa_pod_builder` raw FFI. `format::build()` rewritten on top of it; signature + return type unchanged so `stream.rs` is untouched. `format::parse()` left as-is (inbound POD from compositor is well-formed). Verification = byte-for-byte golden fixture test (CI) + manual N100 GNOME smoke (authoritative).

**Tech Stack:** Rust 1.85, `libspa-sys 0.9.2` (pulled transitively today; promoted to direct dep), `pipewire 0.9.2` (kept for `stream.rs` typed wrappers), existing `wayland_portal::cursor.rs` FFI conventions as reference.

**Constraints:**
- All cargo invocations through `./scripts/dev-container.sh` (Debian bookworm + libpipewire-0.3-dev / libspa-0.2-dev).
- `BuiltParams` struct + `build()` signature MUST NOT change (callers in `stream.rs` and existing tests depend on the shape).
- All `add_*` / `push_object` methods on `PodBuilder` are safe; `unsafe` blocks are contained inside the module and carry `// SAFETY:` comments.
- Cross-platform CI green (Linux + Windows). The whole `wayland_portal/` tree is already `#![cfg(target_os = "linux")]` gated so Windows is unaffected, but the workspace `cargo check` on Windows must still succeed.
- Existing wayland_portal test count must stay ≥ current; no test deletion.

**Spec:** `docs/superpowers/specs/2026-05-13-p5b2a-successor-libspa-pod-rewrite-design.md` (commit `08235fe`).

---

## File map

| File | Status | Responsibility |
|---|---|---|
| `crates/media-linux/Cargo.toml` | modify | promote `libspa-sys = "0.9"` to direct dep |
| `crates/media-linux/src/wayland_portal/mod.rs` | modify | `pub mod pod_builder;` |
| `crates/media-linux/src/wayland_portal/pod_builder.rs` | **new** | safe RAII wrapper over `spa_pod_builder` |
| `crates/media-linux/src/wayland_portal/format.rs` | modify | rewrite `build()` body; keep parse() |
| `crates/media-linux/examples/dump_enum_format.rs` | **new** | one-shot fixture capture |
| `crates/media-linux/tests/format_golden.rs` | **new** | golden-bytes regression test |
| `crates/media-linux/tests/fixtures/enum_format_golden.bin` | **new** | byte fixture |
| `docs/superpowers/p5b1-smoke-walkthrough.md` | modify | + §L (GNOME 46 Wayland smoke) |
| `docs/superpowers/STATUS.md` | modify | P5B-2a-successor entry + P5C-1 known-issue resolution note |

---

## Task 1: Promote `libspa-sys` to direct dep + module wiring

**Files:**
- Modify: `crates/media-linux/Cargo.toml`
- Modify: `crates/media-linux/src/wayland_portal/mod.rs`
- Create: `crates/media-linux/src/wayland_portal/pod_builder.rs` (skeleton only)

- [ ] **Step 1: Add `libspa-sys` direct dep**

Edit `crates/media-linux/Cargo.toml` — under `[target.'cfg(target_os = "linux")'.dependencies]`, add a line below the existing `pipewire` entry:

```toml
# Raw libspa FFI for outbound POD construction (EnumFormat). Currently
# pulled transitively by pipewire 0.9; promoted to a direct dep so the
# Cargo.toml documents the dependency clearly and so a future pipewire
# bump cannot silently drop the symbols pod_builder.rs depends on.
libspa-sys = "0.9"
```

- [ ] **Step 2: Verify resolution**

```bash
./scripts/dev-container.sh bash -c 'cargo tree -p prdt-media-linux --target x86_64-unknown-linux-gnu | grep -E "libspa-sys|pipewire" | sort -u'
```

Expected output contains exactly one `libspa-sys v0.9.x` entry as a direct dep of `prdt-media-linux`, plus the existing transitive entries via pipewire.

- [ ] **Step 3: Add module declaration**

Edit `crates/media-linux/src/wayland_portal/mod.rs` — add after the existing `pub mod cursor;` line (or after `pub mod format;` if cursor isn't there):

```rust
pub mod pod_builder;
```

- [ ] **Step 4: Create `pod_builder.rs` skeleton**

Create `crates/media-linux/src/wayland_portal/pod_builder.rs` with only the module header + ABI-size compile-time check. Leave the `PodBuilder` type for Task 2.

```rust
//! Safe RAII wrapper over `libspa-sys`'s raw `spa_pod_builder_*` FFI for
//! constructing outbound `SPA_PARAM_EnumFormat` PODs. Replaces the
//! `pipewire::spa::pod::serialize::PodSerializer` path that GNOME 46
//! mutter rejects at the wire level (P5C-1 smoke 2026-05-13).
//!
//! # Why raw FFI
//!
//! pipewire-rs 0.9.2's `Object` / `Choice` serializer produces bytes
//! mutter rejects with `invalid message received 0 for 2: Invalid
//! argument`. Reference impls (gnome-remote-desktop, OBS, gst
//! pipewiresrc) all use `spa_pod_builder_*` inline C helpers via
//! libspa-sys; this module follows the same path through a Rust facade.
//!
//! # Safety convention
//!
//! All `pub` methods on `PodBuilder` and `ObjectScope` are safe — the
//! `unsafe { ... }` calls live inside this module behind `// SAFETY:`
//! block-level docstrings, mirroring `wayland_portal/cursor.rs`. The
//! frame stack is RAII-managed: `ObjectScope::drop` pops the frame, so
//! callers cannot create an imbalanced stack at the type level.
//!
//! # ABI pinning
//!
//! `spa_pod_builder_state` ABI is implicitly pinned by `libspa-sys =
//! "0.9"`. The compile-time size check below catches a future struct
//! layout drift before it can corrupt the stack.

#![cfg(target_os = "linux")]

use pipewire::spa::sys as spa_sys;

// Compile-time assertion: spa_pod_builder_state is 16 bytes on every
// libspa-sys 0.9.x build we ship. If a future bump changes this we want
// a hard compile error, not silent stack corruption.
const _: () = {
    if std::mem::size_of::<spa_sys::spa_pod_builder_state>() != 16 {
        panic!("spa_pod_builder_state ABI drift; pin libspa-sys version");
    }
};
```

- [ ] **Step 5: Verify it compiles**

```bash
./scripts/dev-container.sh bash -c 'cargo check -p prdt-media-linux --target x86_64-unknown-linux-gnu 2>&1 | tail -15'
```

Expected: `Finished ... target(s) in Xs` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/Cargo.toml crates/media-linux/src/wayland_portal/mod.rs crates/media-linux/src/wayland_portal/pod_builder.rs
git commit -m "P5B-2a-successor T1: libspa-sys direct dep + pod_builder.rs skeleton"
```

---

## Task 2: `PodBuilder::new` + `finish` + primitive `add_int`

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/pod_builder.rs`

Goal: a minimal builder that can serialize a single `SPA_TYPE_Int` POD and round-trip it back through `pipewire::spa::pod::deserialize`. This is the smallest test that proves the overflow callback + raw byte storage + offset accounting all work.

- [ ] **Step 1: Write the failing test**

Append to `crates/media-linux/src/wayland_portal/pod_builder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::pod::deserialize::PodDeserializer;
    use pipewire::spa::pod::Value;

    #[test]
    fn primitive_int_round_trip() {
        let mut b = PodBuilder::new();
        b.add_int_primitive(0x4242_4242);
        let bytes = b.finish();
        assert!(!bytes.is_empty(), "non-empty POD bytes");
        let (_consumed, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        match value {
            Value::Int(v) => assert_eq!(v, 0x4242_4242),
            other => panic!("expected Value::Int, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests::primitive_int_round_trip 2>&1 | tail -10'
```

Expected: compile error — `PodBuilder` not defined.

- [ ] **Step 3: Implement `PodBuilder::new` + `add_int_primitive` + `finish`**

Insert into `pod_builder.rs` (above the `#[cfg(test)]` block):

```rust
use std::os::raw::{c_int, c_void};

/// Owned builder backed by a heap `Vec<u8>` that auto-grows via the
/// `spa_pod_builder_callbacks::overflow` hook. Single-use: call
/// `finish()` to extract the bytes.
pub struct PodBuilder {
    buf: Box<Vec<u8>>,
    raw: spa_sys::spa_pod_builder,
    // Frame stack is small (≤ 2 in practice: object + choice). Vec is
    // fine here.
    frames: Vec<spa_sys::spa_pod_frame>,
    // Callbacks block must outlive `raw.callbacks` pointer; kept as a
    // field so its address is stable across calls.
    callbacks: spa_sys::spa_pod_builder_callbacks,
}

impl PodBuilder {
    /// Initial backing capacity, chosen to fit our largest EnumFormat
    /// POD (currently ~200 B) without triggering the overflow callback
    /// on the happy path. Overflow path is still exercised by the
    /// dedicated unit test (Task 2 step 7).
    pub const INITIAL_CAPACITY: usize = 256;

    pub fn new() -> Self {
        let buf = Box::new(Vec::<u8>::with_capacity(Self::INITIAL_CAPACITY));
        let mut raw: spa_sys::spa_pod_builder = unsafe { std::mem::zeroed() };
        let callbacks: spa_sys::spa_pod_builder_callbacks =
            unsafe { std::mem::zeroed() };
        let mut b = Self {
            buf,
            raw,
            frames: Vec::with_capacity(2),
            callbacks,
        };
        b.reinit_raw();
        b
    }

    /// Re-point `raw.data` + `raw.size` to the current `buf` allocation.
    /// Called from `new` and from the overflow callback after a realloc.
    fn reinit_raw(&mut self) {
        let cap = self.buf.capacity();
        let ptr = self.buf.as_mut_ptr() as *mut c_void;
        // SAFETY: spa_pod_builder_init is a libspa-sys exported symbol
        // taking a *mut spa_pod_builder + data ptr + size. We pass a
        // valid (non-null) buffer of `cap` bytes.
        unsafe {
            spa_sys::spa_pod_builder_init(&mut self.raw, ptr, cap as u32);
            self.callbacks.version = spa_sys::SPA_VERSION_POD_BUILDER_CALLBACKS;
            self.callbacks.overflow = Some(Self::overflow_trampoline);
            spa_sys::spa_pod_builder_set_callbacks(
                &mut self.raw,
                &self.callbacks,
                self as *mut _ as *mut c_void,
            );
        }
    }

    /// Overflow trampoline invoked by libspa when an append would exceed
    /// `raw.size`. We grow `buf` to at least `size` bytes (libspa hands
    /// us the required size), reinit the builder pointer, and return 0
    /// to indicate the retry should succeed.
    unsafe extern "C" fn overflow_trampoline(data: *mut c_void, size: u32) -> c_int {
        // SAFETY: libspa invokes this with the `data` pointer we passed
        // to `set_callbacks` (= the &mut Self pointer). Reconstruct the
        // mutable reference; libspa guarantees single-threaded reentry.
        let this = &mut *(data as *mut Self);
        // Grow to next power-of-two ≥ size, with a minimum doubling.
        let need = size as usize;
        let want = std::cmp::max(this.buf.capacity() * 2, need);
        this.buf.reserve(want - this.buf.len());
        this.reinit_raw();
        0
    }

    /// Append a bare `SPA_TYPE_Int` POD. Only used by the `add_int_primitive`
    /// path before objects are introduced (Task 2 baseline test).
    pub fn add_int_primitive(&mut self, val: i32) {
        // SAFETY: builder is valid (init'd in new()); val is plain i32.
        unsafe {
            spa_sys::spa_pod_builder_int(&mut self.raw, val);
        }
    }

    /// Drop the builder and return the serialised POD bytes truncated
    /// to the actual size written (`raw.state.offset`).
    pub fn finish(mut self) -> Vec<u8> {
        // raw.state.offset is the number of bytes written into self.buf.
        let len = self.raw.state.offset as usize;
        // SAFETY: libspa only writes within [0, capacity). Setting len
        // to offset is sound because the bytes 0..offset are initialized.
        unsafe { self.buf.set_len(len) }
        *self.buf
    }
}

impl Default for PodBuilder {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests::primitive_int_round_trip 2>&1 | tail -10'
```

Expected: `test pod_builder::tests::primitive_int_round_trip ... ok` (1 passed).

- [ ] **Step 5: Add the overflow regression test**

Append inside the `#[cfg(test)] mod tests` block:

```rust
    /// Exercises the realloc path by pushing more than INITIAL_CAPACITY
    /// bytes worth of primitives. If the overflow callback / raw
    /// reinit is wrong, libspa silently truncates or corrupts.
    #[test]
    fn overflow_path_reallocs_without_corruption() {
        let mut b = PodBuilder::new();
        let n = (PodBuilder::INITIAL_CAPACITY / 8) + 8; // > capacity in primitives
        for i in 0..n {
            b.add_int_primitive(i as i32);
        }
        let bytes = b.finish();
        assert!(
            bytes.len() > PodBuilder::INITIAL_CAPACITY,
            "expected realloc beyond INITIAL_CAPACITY, got {} bytes",
            bytes.len()
        );
        // We don't deserialise here — multiple bare primitives back-to-back
        // is not a single valid POD. The size check + non-corruption is
        // what we care about (libspa would have aborted via SIGSEGV if
        // the realloc was wrong).
    }
```

- [ ] **Step 6: Run all pod_builder tests**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -10'
```

Expected: `2 passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/media-linux/src/wayland_portal/pod_builder.rs
git commit -m "P5B-2a-successor T2: PodBuilder::new + finish + add_int_primitive + overflow"
```

---

## Task 3: `add_id` + `add_rectangle` + `add_fraction` primitives

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/pod_builder.rs`

These wrap `spa_pod_builder_id` / `_rectangle` / `_fraction`. Each test deserialises the produced POD and asserts the value matches.

- [ ] **Step 1: Write failing tests**

Append into the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn primitive_id_round_trip() {
        let mut b = PodBuilder::new();
        b.add_id_primitive(spa_sys::SPA_VIDEO_FORMAT_BGRA);
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        match value {
            Value::Id(id) => assert_eq!(id.0, spa_sys::SPA_VIDEO_FORMAT_BGRA),
            other => panic!("expected Id, got {other:?}"),
        }
    }

    #[test]
    fn primitive_rectangle_round_trip() {
        let mut b = PodBuilder::new();
        b.add_rectangle_primitive(1920, 1080);
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        match value {
            Value::Rectangle(r) => {
                assert_eq!(r.width, 1920);
                assert_eq!(r.height, 1080);
            }
            other => panic!("expected Rectangle, got {other:?}"),
        }
    }

    #[test]
    fn primitive_fraction_round_trip() {
        let mut b = PodBuilder::new();
        b.add_fraction_primitive(60, 1);
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        match value {
            Value::Fraction(f) => {
                assert_eq!(f.num, 60);
                assert_eq!(f.denom, 1);
            }
            other => panic!("expected Fraction, got {other:?}"),
        }
    }
```

Plus update the existing `use` line at the top of the `tests` mod to include `Id`, `Rectangle`, `Fraction`:

```rust
    use pipewire::spa::pod::Value;
    use pipewire::spa::utils::{Fraction, Id, Rectangle};
```

(The `Id`/`Rectangle`/`Fraction` types come from `pipewire::spa::utils`; the deserialiser returns `Value::Id(Id)`, `Value::Rectangle(Rectangle)`, `Value::Fraction(Fraction)`. Verify the actual public re-export path by skimming `pipewire 0.9.2` if the build fails.)

- [ ] **Step 2: Run tests to verify they fail**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -15'
```

Expected: 3 compile errors — `add_id_primitive` / `add_rectangle_primitive` / `add_fraction_primitive` not defined.

- [ ] **Step 3: Implement the three primitives**

Insert into the `impl PodBuilder` block (above `finish`):

```rust
    pub fn add_id_primitive(&mut self, id: u32) {
        // SAFETY: builder is valid; spa_pod_builder_id takes a u32.
        unsafe {
            spa_sys::spa_pod_builder_id(&mut self.raw, id);
        }
    }

    pub fn add_rectangle_primitive(&mut self, width: u32, height: u32) {
        // SAFETY: builder is valid; spa_pod_builder_rectangle writes
        // a SPA_TYPE_Rectangle (8 bytes) at the current offset.
        unsafe {
            spa_sys::spa_pod_builder_rectangle(&mut self.raw, width, height);
        }
    }

    pub fn add_fraction_primitive(&mut self, num: i32, denom: i32) {
        // SAFETY: builder is valid; spa_pod_builder_fraction writes
        // a SPA_TYPE_Fraction (8 bytes) at the current offset.
        // libspa-sys expects (num: u32, denom: u32) but interprets them
        // as signed int32. The current bindings type the args as u32;
        // we cast at the boundary.
        unsafe {
            spa_sys::spa_pod_builder_fraction(
                &mut self.raw,
                num as u32,
                denom as u32,
            );
        }
    }
```

> If `spa_sys::spa_pod_builder_fraction` turns out to be typed as
> `(i32, i32)` in your local bindings (libspa-sys regenerates per
> bookworm/jammy), drop the `as u32` casts. The function does not
> exist in two distinct ABIs — only the bindgen typing varies.

- [ ] **Step 4: Run tests to verify they pass**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -10'
```

Expected: `5 passed; 0 failed` (the 2 from Task 2 + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux/src/wayland_portal/pod_builder.rs
git commit -m "P5B-2a-successor T3: PodBuilder primitives — id / rectangle / fraction"
```

---

## Task 4: `push_object` + `ObjectScope` RAII + `add_prop` glue

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/pod_builder.rs`

This is the load-bearing scope machinery. Object frames must pop in LIFO order; `ObjectScope::drop` enforces that structurally.

- [ ] **Step 1: Write the failing test**

Append into the `#[cfg(test)] mod tests` block:

```rust
    /// Build an empty `SPA_TYPE_OBJECT_Format` (no properties), confirm
    /// the resulting POD deserialises to `Value::Object` with the right
    /// type/id and an empty properties vec.
    #[test]
    fn empty_object_round_trip() {
        let mut b = PodBuilder::new();
        {
            let _scope = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
        } // scope drop -> pop
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            other => panic!("expected Object, got {other:?}"),
        };
        assert_eq!(obj.type_, spa_sys::SPA_TYPE_OBJECT_Format);
        assert_eq!(obj.id, spa_sys::SPA_PARAM_EnumFormat);
        assert!(obj.properties.is_empty());
    }

    /// One scalar Id property inside an object — proves `add_prop` +
    /// primitive composition works.
    #[test]
    fn object_with_single_id_property_round_trip() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_id_property(spa_sys::SPA_FORMAT_mediaType, spa_sys::SPA_MEDIA_TYPE_video);
        }
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            other => panic!("expected Object, got {other:?}"),
        };
        assert_eq!(obj.properties.len(), 1);
        let p = &obj.properties[0];
        assert_eq!(p.key, spa_sys::SPA_FORMAT_mediaType);
        match &p.value {
            Value::Id(Id(v)) => assert_eq!(*v, spa_sys::SPA_MEDIA_TYPE_video),
            other => panic!("expected Id, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -20'
```

Expected: compile errors on `push_object` / `add_id_property`.

- [ ] **Step 3: Implement `push_object` + `ObjectScope` + `add_id_property`**

Insert into `pod_builder.rs` after the `impl Default` block:

```rust
/// RAII guard returned by `PodBuilder::push_object`. Drop pops the
/// libspa frame and decrements the builder's frame stack, structurally
/// preventing imbalanced push/pop on every caller path (including
/// panics).
pub struct ObjectScope<'a> {
    builder: &'a mut PodBuilder,
}

impl<'a> ObjectScope<'a> {
    /// Add a `SPA_TYPE_Id` property to the current object.
    pub fn add_id_property(&mut self, key: u32, id: u32) {
        // SAFETY: builder is inside an object frame (push_object set up
        // the libspa state); spa_pod_builder_prop takes (key, flags).
        // Flags=0 means a plain (non-Choice) property.
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, 0);
        }
        self.builder.add_id_primitive(id);
    }
}

impl<'a> Drop for ObjectScope<'a> {
    fn drop(&mut self) {
        // Pop the last frame we pushed. The libspa pop returns the
        // address of the closed pod inside `buf`; null indicates frame
        // stack underflow which would be an FFI bug on our side.
        let Some(mut frame) = self.builder.frames.pop() else {
            debug_assert!(false, "ObjectScope::drop with empty frame stack");
            return;
        };
        // SAFETY: frame was produced by spa_pod_builder_push_object on
        // the same builder; pop matches.
        unsafe {
            let pod = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut frame as *mut _,
            );
            debug_assert!(!pod.is_null(), "spa_pod_builder_pop returned null");
        }
    }
}

impl PodBuilder {
    /// Open a `SPA_TYPE_Object` (or any object subtype). Returns an
    /// `ObjectScope` whose `Drop` impl pops the frame. Property keys
    /// added during the scope's lifetime are appended to this object.
    pub fn push_object(
        &mut self,
        type_id: u32,
        prop_key: u32,
    ) -> ObjectScope<'_> {
        // Reserve a fresh spa_pod_frame inside our Vec so libspa can
        // write into it. Push a zero-initialised frame, then take a
        // raw pointer to the just-pushed slot.
        // SAFETY: frame layout is plain POD; zeroing is valid.
        let frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        self.frames.push(frame);
        let frame_ptr =
            self.frames.last_mut().unwrap() as *mut spa_sys::spa_pod_frame;
        // SAFETY: builder is valid; frame_ptr is valid (just pushed);
        // type_id + prop_key are u32 enum values produced by libspa-sys.
        unsafe {
            spa_sys::spa_pod_builder_push_object(
                &mut self.raw,
                frame_ptr,
                type_id,
                prop_key,
            );
        }
        ObjectScope { builder: self }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -15'
```

Expected: `7 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux/src/wayland_portal/pod_builder.rs
git commit -m "P5B-2a-successor T4: push_object + ObjectScope RAII + add_id_property"
```

---

## Task 5: Choice helpers — `add_choice_id_enum`, `add_choice_rectangle_range`, `add_choice_fraction_range`

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/pod_builder.rs`

The Choice properties carry the `MANDATORY | DONT_FIXATE` flag pair. Each helper opens a property header with those flags, pushes a Choice frame of the right type, emits `default + alternatives`, then pops.

- [ ] **Step 1: Write the failing test**

Append into the `tests` block:

```rust
    use pipewire::spa::pod::{ChoiceEnum, ChoiceValue};

    /// The Choice property must serialise with flags MANDATORY|DONT_FIXATE
    /// AND with the listed default + alternatives in the Enum body. This
    /// is the contract GNOME 46 mutter rejected when our previous build
    /// used pipewire-rs's high-level Object serializer (spec §2).
    #[test]
    fn choice_id_enum_round_trip_with_mandatory_dont_fixate() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_choice_id_enum(
                spa_sys::SPA_FORMAT_VIDEO_format,
                spa_sys::SPA_VIDEO_FORMAT_BGRA,
                &[
                    spa_sys::SPA_VIDEO_FORMAT_BGRA,
                    spa_sys::SPA_VIDEO_FORMAT_BGRx,
                ],
            );
        }
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            other => panic!("expected Object, got {other:?}"),
        };
        assert_eq!(obj.properties.len(), 1);
        let p = &obj.properties[0];
        assert_eq!(p.key, spa_sys::SPA_FORMAT_VIDEO_format);
        // pipewire::spa::pod::PropertyFlags::MANDATORY = 8,
        // ::DONT_FIXATE = 16 (matches SPA_POD_PROP_FLAG_*).
        let expected_flags: u32 = 8 | 16;
        assert_eq!(
            p.flags.bits(),
            expected_flags,
            "MANDATORY|DONT_FIXATE flags missing on Choice property"
        );
        match &p.value {
            Value::Choice(ChoiceValue::Id(c)) => match &c.1 {
                ChoiceEnum::Enum { default, alternatives } => {
                    assert_eq!(default.0, spa_sys::SPA_VIDEO_FORMAT_BGRA);
                    let alt_vals: Vec<u32> = alternatives.iter().map(|id| id.0).collect();
                    assert_eq!(
                        alt_vals,
                        vec![
                            spa_sys::SPA_VIDEO_FORMAT_BGRA,
                            spa_sys::SPA_VIDEO_FORMAT_BGRx,
                        ]
                    );
                }
                other => panic!("expected ChoiceEnum::Enum, got {other:?}"),
            },
            other => panic!("expected Choice<Id>, got {other:?}"),
        }
    }

    #[test]
    fn choice_rectangle_range_round_trip() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_choice_rectangle_range(
                spa_sys::SPA_FORMAT_VIDEO_size,
                (1920, 1080),
                (320, 240),
                (7680, 4320),
            );
        }
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            _ => panic!(),
        };
        let p = &obj.properties[0];
        assert_eq!(p.flags.bits(), 8 | 16);
        match &p.value {
            Value::Choice(ChoiceValue::Rectangle(c)) => match &c.1 {
                ChoiceEnum::Range { default, min, max } => {
                    assert_eq!(default.width, 1920);
                    assert_eq!(default.height, 1080);
                    assert_eq!(min.width, 320);
                    assert_eq!(max.width, 7680);
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn choice_fraction_range_round_trip() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_choice_fraction_range(
                spa_sys::SPA_FORMAT_VIDEO_framerate,
                (60, 1),
                (15, 1),
                (60, 1),
            );
        }
        let bytes = b.finish();
        let (_n, value) =
            PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            _ => panic!(),
        };
        let p = &obj.properties[0];
        assert_eq!(p.flags.bits(), 8 | 16);
        match &p.value {
            Value::Choice(ChoiceValue::Fraction(c)) => match &c.1 {
                ChoiceEnum::Range { default, min, max } => {
                    assert_eq!((default.num, default.denom), (60, 1));
                    assert_eq!((min.num, min.denom), (15, 1));
                    assert_eq!((max.num, max.denom), (60, 1));
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -15'
```

Expected: 3 compile errors on `add_choice_id_enum` / `add_choice_rectangle_range` / `add_choice_fraction_range`.

- [ ] **Step 3: Implement the three Choice helpers**

Add to `impl<'a> ObjectScope<'a>` block (alongside `add_id_property`):

```rust
    /// Append a `Choice<Id>` property (Enum form) carrying the
    /// `MANDATORY | DONT_FIXATE` flag pair. This is the contract
    /// libspa expects for "pick one of these alternatives".
    pub fn add_choice_id_enum(
        &mut self,
        key: u32,
        default: u32,
        alternatives: &[u32],
    ) {
        let flags = spa_sys::SPA_POD_PROP_FLAG_MANDATORY
            | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE;
        // SAFETY: same as add_id_property; builder is inside an object frame.
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, flags);
        }
        let mut choice_frame: spa_sys::spa_pod_frame =
            unsafe { std::mem::zeroed() };
        // SAFETY: push_choice opens a SPA_TYPE_Choice subframe. Pop
        // matches via the local frame variable below.
        unsafe {
            spa_sys::spa_pod_builder_push_choice(
                &mut self.builder.raw,
                &mut choice_frame as *mut _,
                spa_sys::SPA_CHOICE_Enum,
                0, // flags on the Choice header itself
            );
        }
        // Default first, then each alternative. libspa stores them
        // back-to-back as the child pods of the Choice.
        self.builder.add_id_primitive(default);
        for &alt in alternatives {
            self.builder.add_id_primitive(alt);
        }
        // SAFETY: pop matches the push_choice above.
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut choice_frame as *mut _,
            );
        }
    }

    pub fn add_choice_rectangle_range(
        &mut self,
        key: u32,
        default: (u32, u32),
        min: (u32, u32),
        max: (u32, u32),
    ) {
        let flags = spa_sys::SPA_POD_PROP_FLAG_MANDATORY
            | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE;
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, flags);
        }
        let mut frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        unsafe {
            spa_sys::spa_pod_builder_push_choice(
                &mut self.builder.raw,
                &mut frame as *mut _,
                spa_sys::SPA_CHOICE_Range,
                0,
            );
        }
        self.builder.add_rectangle_primitive(default.0, default.1);
        self.builder.add_rectangle_primitive(min.0, min.1);
        self.builder.add_rectangle_primitive(max.0, max.1);
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut frame as *mut _,
            );
        }
    }

    pub fn add_choice_fraction_range(
        &mut self,
        key: u32,
        default: (i32, i32),
        min: (i32, i32),
        max: (i32, i32),
    ) {
        let flags = spa_sys::SPA_POD_PROP_FLAG_MANDATORY
            | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE;
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, flags);
        }
        let mut frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        unsafe {
            spa_sys::spa_pod_builder_push_choice(
                &mut self.builder.raw,
                &mut frame as *mut _,
                spa_sys::SPA_CHOICE_Range,
                0,
            );
        }
        self.builder.add_fraction_primitive(default.0, default.1);
        self.builder.add_fraction_primitive(min.0, min.1);
        self.builder.add_fraction_primitive(max.0, max.1);
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut frame as *mut _,
            );
        }
    }
```

- [ ] **Step 4: Run all pod_builder tests**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu pod_builder::tests 2>&1 | tail -15'
```

Expected: `10 passed; 0 failed` (7 from earlier tasks + 3 new Choice tests).

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux/src/wayland_portal/pod_builder.rs
git commit -m "P5B-2a-successor T5: Choice helpers — id-enum + rectangle-range + fraction-range"
```

---

## Task 6: Rewrite `format::build()` on `PodBuilder`

**Files:**
- Modify: `crates/media-linux/src/wayland_portal/format.rs`

The 3 existing build-side tests (`round_trip_bgra`,
`build_choice_properties_have_mandatory_dont_fixate_flags`,
`build_video_format_alternatives_cover_bgra_rgba_family`) MUST keep
passing without modification. The 5 parse-side tests must also stay
green (they exercise `parse()` which is unchanged).

- [ ] **Step 1: Confirm baseline tests**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu wayland_portal::format::tests 2>&1 | tail -15'
```

Expected: all 8 existing tests pass on master state (5 parse + 3 build).

- [ ] **Step 2: Rewrite `build()`**

Edit `crates/media-linux/src/wayland_portal/format.rs`. Replace the entire `pub fn build() -> BuiltParams { ... }` body (lines ~79-161 in the current file) with the new implementation while keeping the signature and the long doc comment above it untouched:

```rust
pub fn build() -> BuiltParams {
    use crate::wayland_portal::pod_builder::PodBuilder;
    use pipewire::spa::sys as spa_sys;

    let mut b = PodBuilder::new();
    {
        let mut o = b.push_object(
            spa_sys::SPA_TYPE_OBJECT_Format,
            spa_sys::SPA_PARAM_EnumFormat,
        );

        // MediaType / MediaSubtype: scalar Id properties, no Choice.
        o.add_id_property(spa_sys::SPA_FORMAT_mediaType, spa_sys::SPA_MEDIA_TYPE_video);
        o.add_id_property(spa_sys::SPA_FORMAT_mediaSubtype, spa_sys::SPA_MEDIA_SUBTYPE_raw);

        // VideoFormat: Choice<Id> Enum over the full 32-bit BGRA/RGBA
        // family so compositors with iHD-style framebuffer ordering can
        // match without falling back to "no more input formats".
        o.add_choice_id_enum(
            spa_sys::SPA_FORMAT_VIDEO_format,
            spa_sys::SPA_VIDEO_FORMAT_BGRA,
            &[
                spa_sys::SPA_VIDEO_FORMAT_BGRA,
                spa_sys::SPA_VIDEO_FORMAT_BGRx,
                spa_sys::SPA_VIDEO_FORMAT_RGBA,
                spa_sys::SPA_VIDEO_FORMAT_RGBx,
                spa_sys::SPA_VIDEO_FORMAT_ARGB,
                spa_sys::SPA_VIDEO_FORMAT_ABGR,
                spa_sys::SPA_VIDEO_FORMAT_xRGB,
                spa_sys::SPA_VIDEO_FORMAT_xBGR,
            ],
        );

        // VideoSize: Choice<Rectangle> Range, default 1920x1080.
        o.add_choice_rectangle_range(
            spa_sys::SPA_FORMAT_VIDEO_size,
            (1920, 1080),
            (320, 240),
            (7680, 4320),
        );

        // VideoFramerate: Choice<Fraction> Range, default 60/1.
        o.add_choice_fraction_range(
            spa_sys::SPA_FORMAT_VIDEO_framerate,
            (60, 1),
            (15, 1),
            (60, 1),
        );
    } // ObjectScope drop -> pop

    BuiltParams { bytes: vec![b.finish()] }
}
```

Also remove the now-unused imports at the top of the file:

```rust
// Delete these lines (no longer needed; PodBuilder owns the construction):
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{ChoiceValue, Object, Property, PropertyFlags};
use pipewire::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, SpaTypes};
```

Keep these imports — `parse()` and the tests still need them:

```rust
use pipewire::spa::param::{
    format::{FormatProperties, MediaSubtype, MediaType},
    video::VideoFormat,
    ParamType,
};
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::pod::{Pod, Value};
use pipewire::spa::utils::{Fraction, Id, Rectangle};
use thiserror::Error;
```

If `parse()`'s `unwrap_choice_default` helper references `ChoiceValue`/`ChoiceEnum`, keep those imports too — only delete the ones that are truly unused after the rewrite.

- [ ] **Step 3: Run all format.rs tests**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu wayland_portal::format::tests 2>&1 | tail -20'
```

Expected: `8 passed; 0 failed`. The 3 build-side tests confirm `build()` still emits an EnumFormat with the MANDATORY|DONT_FIXATE Choice properties and the 8 VideoFormat alternatives.

- [ ] **Step 4: Run the entire wayland_portal test suite**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu wayland_portal:: 2>&1 | tail -10'
```

Expected: all wayland_portal tests pass (count is whatever was green before the branch started; nothing should regress).

- [ ] **Step 5: Clippy clean**

```bash
./scripts/dev-container.sh bash -c 'cargo clippy -p prdt-media-linux --target x86_64-unknown-linux-gnu -- -D warnings 2>&1 | tail -15'
```

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/src/wayland_portal/format.rs
git commit -m "P5B-2a-successor T6: rewrite format::build() on PodBuilder (libspa-sys FFI)"
```

---

## Task 7: Golden-bytes fixture + regression test

**Files:**
- Create: `crates/media-linux/examples/dump_enum_format.rs`
- Create: `crates/media-linux/tests/fixtures/enum_format_golden.bin`
- Create: `crates/media-linux/tests/format_golden.rs`

This is the CI guard: the test runs in the container (no Wayland, no DRI) and fails loudly if `build()` ever drifts byte-for-byte. The fixture's authority is anchored by the manual smoke in Task 8 — we capture only after the smoke succeeds.

> **For the implementer:** Tasks 7 and 8 are interlocked. Do Task 7 Step 1 (write the example), then jump to Task 8 to run the smoke. **Only after Task 8 confirms `state: Streaming` end-to-end, come back here and run Step 2 to capture the fixture.** This ordering ensures the fixture documents bytes that are known to work, not bytes that merely round-trip through Rust.

- [ ] **Step 1: Write the fixture-capture example binary**

Create `crates/media-linux/examples/dump_enum_format.rs`:

```rust
//! Capture the bytes that `wayland_portal::format::build()` currently
//! produces and write them to a binary file. Used to refresh the
//! golden fixture under `tests/fixtures/enum_format_golden.bin` after
//! a verified end-to-end smoke success.
//!
//! Run via: `cargo run -p prdt-media-linux --example dump_enum_format -- <out_path>`

#![cfg(target_os = "linux")]

use std::io::Write;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("usage: dump_enum_format <out_path>"))?;
    let built = prdt_media_linux::wayland_portal::format::build();
    let bytes = &built.bytes[0];
    let mut f = std::fs::File::create(out)?;
    f.write_all(bytes)?;
    eprintln!("wrote {} bytes to {out}", bytes.len());
    Ok(())
}
```

Add an `[[example]]` table to `crates/media-linux/Cargo.toml`:

```toml
[[example]]
name = "dump_enum_format"
required-features = []
```

Verify the example compiles:

```bash
./scripts/dev-container.sh bash -c 'cargo build -p prdt-media-linux --target x86_64-unknown-linux-gnu --example dump_enum_format 2>&1 | tail -5'
```

Expected: `Finished ...`.

You also need `wayland_portal::format` to be reachable as a public path. If `wayland_portal` isn't `pub` at the crate root, add `pub use wayland_portal;` (or `pub mod wayland_portal { pub use crate::wayland_portal::*; }`) — verify by reading `crates/media-linux/src/lib.rs` and following the existing module visibility convention. If `format::build` is already reachable via a public path (`prdt_media_linux::wayland_portal::format::build`), no lib.rs change is needed.

- [ ] **Step 2: Capture the fixture — AFTER Task 8 reports `state: Streaming`**

This step is gated on Task 8 success. Do NOT run it until the N100 GNOME 46 smoke has confirmed `state: Streaming` and frames are flowing. Then:

```bash
mkdir -p crates/media-linux/tests/fixtures
./scripts/dev-container.sh bash -c 'cargo run -p prdt-media-linux --target x86_64-unknown-linux-gnu --example dump_enum_format -- crates/media-linux/tests/fixtures/enum_format_golden.bin'
file crates/media-linux/tests/fixtures/enum_format_golden.bin
xxd crates/media-linux/tests/fixtures/enum_format_golden.bin | head -20
```

Expected: a binary file ~150–250 bytes long. The hexdump header should start with the SPA_TYPE_Object pod header (`size: u32; type: u32 = SPA_TYPE_Object (262144)`); the body should contain MediaType, MediaSubtype, the VideoFormat Choice, etc. Eyeball-check the first 16 bytes to confirm a non-zero size and the expected type tag — full structural verification is the deserialise round-trip in Step 3.

- [ ] **Step 3: Write the golden-bytes regression test**

Create `crates/media-linux/tests/format_golden.rs`:

```rust
//! Byte-for-byte regression test for `wayland_portal::format::build()`.
//!
//! The fixture `tests/fixtures/enum_format_golden.bin` was captured
//! immediately after the N100 GNOME 46 Wayland smoke confirmed
//! `pipewire stream: state Streaming` and BGRA frames flowing through
//! to the P5C-1 VAAPI encoder. Any change to `build()` that produces
//! different bytes risks reintroducing the "no more input formats"
//! wire-error and will fail this test loudly in CI.

#![cfg(target_os = "linux")]

const FIXTURE: &[u8] = include_bytes!("fixtures/enum_format_golden.bin");

#[test]
fn enum_format_matches_golden_fixture() {
    let built = prdt_media_linux::wayland_portal::format::build();
    let actual = &built.bytes[0];
    if actual != FIXTURE {
        // Surface a helpful diff hint — length first, then first
        // diverging offset — so an implementer who intentionally
        // changes `build()` can refresh the fixture confidently.
        eprintln!(
            "build() bytes diverged from fixture (actual={} bytes, fixture={} bytes)",
            actual.len(),
            FIXTURE.len()
        );
        let n = actual.len().min(FIXTURE.len());
        let mut first_diff = None;
        for i in 0..n {
            if actual[i] != FIXTURE[i] {
                first_diff = Some(i);
                break;
            }
        }
        if let Some(off) = first_diff {
            eprintln!("first divergence at byte {off}: actual=0x{:02x} fixture=0x{:02x}",
                      actual[off], FIXTURE[off]);
        } else {
            eprintln!("prefix matches; lengths differ at offset {n}");
        }
        eprintln!(
            "To refresh after a verified end-to-end smoke: \n  cargo run -p prdt-media-linux --example dump_enum_format -- crates/media-linux/tests/fixtures/enum_format_golden.bin"
        );
        panic!("build() bytes diverged from golden fixture");
    }
}
```

- [ ] **Step 4: Run the golden test**

```bash
./scripts/dev-container.sh bash -c 'cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu --test format_golden 2>&1 | tail -10'
```

Expected: `test enum_format_matches_golden_fixture ... ok`.

- [ ] **Step 5: Run the entire workspace test suite to confirm no regressions**

```bash
./scripts/dev-container.sh bash -c 'cargo test --workspace --target x86_64-unknown-linux-gnu 2>&1 | tail -20'
```

Expected: all tests pass; the count is at least `(previous workspace total) + 10 new (pod_builder) + 1 new (golden) = previous + 11`.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/Cargo.toml \
        crates/media-linux/examples/dump_enum_format.rs \
        crates/media-linux/tests/format_golden.rs \
        crates/media-linux/tests/fixtures/enum_format_golden.bin
git commit -m "P5B-2a-successor T7: golden fixture + format_golden regression test"
```

---

## Task 8: N100 GNOME 46 Wayland real-device smoke

**Files:**
- (none directly modified — manual procedure; results documented in Task 9)

Do this AFTER T6 (build rewrite) but BEFORE T7 step 2 (fixture capture). The smoke is the ground truth: if mutter still rejects the POD, the rewrite isn't done yet and we have to debug before freezing the fixture.

- [ ] **Step 1: Build the release artifact**

```bash
gh workflow run release.yml --ref phase-p5b2a-successor-libspa-pod-rewrite -f ref=phase-p5b2a-successor-libspa-pod-rewrite 2>&1 | tail -3
gh run watch --exit-status 2>&1 | tail -10
```

Expected: `✓ <run-id>` (run succeeds). Note the run id for the artifact download URL.

Download on the N100 (user runs this themselves):

```bash
# On the N100, replace <run-id> with the actual id and download via the
# workflow_run artifact UI on github.com/drian320/power-remote-dt/actions
# Save as prdt-linux.tar.gz, extract to ./prdt.
```

- [ ] **Step 2: Verify Wayland session**

On the N100, in a GNOME 46 Wayland login session:

```bash
echo $XDG_SESSION_TYPE
# Expected: wayland
echo $XDG_CURRENT_DESKTOP
# Expected: GNOME or ubuntu:GNOME
gnome-shell --version
# Expected: GNOME Shell 46.x
```

- [ ] **Step 3: Run the host with explicit VAAPI**

```bash
./prdt host \
    --encoder vaapi \
    --bitrate-mbps 5 \
    --silent-allow \
    2>&1 | tee p5b2as-smoke.log
```

When the GNOME portal consent dialog appears, click "Share" / "Allow".

- [ ] **Step 4: Confirm `state: Streaming`**

In a second terminal:

```bash
grep -E "pipewire stream: state|param_changed|encoder ready|WaylandPortalCapturer" p5b2as-smoke.log
```

Expected, in order:

```text
portal session: started has_token=true
encoder ready backend="linux-vaapi-h264"
pipewire stream: state Unconnected → Connecting
pipewire stream: state Connecting → Paused
param_changed: SPA_PARAM_Format negotiated: ...
pipewire stream: state Paused → Streaming
```

If you see `→ Error` or `no more input formats`: the rewrite is incomplete. Capture the full `PIPEWIRE_DEBUG=4 PIPEWIRE_LOG=stderr ./prdt host ...` output and stop — go back to T2-T6 and debug. Common failure modes:
- `MANDATORY|DONT_FIXATE` flags missing on a Choice property: re-check T5 step 3.
- VideoFormat alternatives list order differs from what mutter expects: T6's list pins this.
- Object header type/id mismatch: confirm `SPA_TYPE_OBJECT_Format` (262147) + `SPA_PARAM_EnumFormat` (3) in T6.

- [ ] **Step 5: Connect the viewer**

From a second machine (or the same N100 in a second login):

```bash
./prdt connect \
    --host <n100-ip>:9000 \
    --decoder openh264 \
    --codec h264 \
    --host-pubkey <pubkey-from-host-startup-log>
```

Expected: viewer window shows the N100's live desktop content within ~3 seconds.

- [ ] **Step 6: Confirm CPU savings (the VAAPI payoff)**

In a third terminal on the N100, during active streaming:

```bash
pidstat -p $(pgrep -f "prdt host") 1 30
```

Expected: host `%CPU` significantly below the OpenH264 SW baseline. On Intel N100 / 1080p60 / 5 Mbps the target is < 10 % (vs OpenH264 SW typically 25-40 %). Record the observed average in Task 9's STATUS update.

- [ ] **Step 7: Clean tear-down**

Viewer Ctrl+C → expect host watchdog to log `session timed out (5s silence)` and idle within ~6 seconds. Run a second viewer connect to confirm clean re-session.

- [ ] **Step 8: Capture the fixture (Task 7 Step 2 unblock)**

If all steps 4-7 succeeded, the bytes `build()` emitted ARE the right bytes. Go back to Task 7 Step 2 and capture `tests/fixtures/enum_format_golden.bin` from the same build. Commit per Task 7 Step 6.

If anything in steps 4-7 failed, the rewrite isn't done. Do not capture the fixture.

- [ ] **Step 9: No code commit here — the deliverable is the result data**

The commit for this task lives in Task 9 (STATUS + walkthrough update) with the smoke timestamps and pidstat readings captured here.

---

## Task 9: Walkthrough §L + STATUS update

**Files:**
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md` (append §L)
- Modify: `docs/superpowers/STATUS.md` (P5B-2a-successor entry + P5C-1 known-issue resolution note)

- [ ] **Step 1: Append §L to the walkthrough**

Open `docs/superpowers/p5b1-smoke-walkthrough.md` and find the end of the file (currently the P5C-1 §K block). Append a new top-level section:

```markdown

## P5B-2a-successor — Wayland portal libspa POD wire-format rewrite (GNOME 46 smoke)

### Section L — GNOME 46 Wayland real-device smoke

**Pre-conditions:**
- Linux host running a GNOME 46 Wayland session (Ubuntu 24.04 or
  equivalent).
- Intel iGPU (Tigerlake+) or AMD APU (Renoir+) for VAAPI encoder.
- User in `render` (or `video`) group so `/dev/dri/renderD128` is
  RW-accessible.
- `prdt host` + `prdt connect` binaries from the
  `phase-p5b2a-successor-libspa-pod-rewrite` branch release artifact.

**Steps:**

1. Verify Wayland: `echo $XDG_SESSION_TYPE` → `wayland`.

2. Start host with explicit Vaapi:
   ```bash
   ./prdt host --encoder vaapi --bitrate-mbps 5 --silent-allow 2>&1 | tee p5b2as.log
   ```

3. Click "Share" on the GNOME portal consent dialog.

4. Confirm log line transitions:
   - `portal session: started has_token=true`
   - `encoder ready backend="linux-vaapi-h264"`
   - `pipewire stream: state Unconnected → Connecting → Paused → Streaming`
   - `param_changed: SPA_PARAM_Format negotiated: <FORMAT> <W>x<H> <NUM>/<DEN>`

5. From a second machine, connect viewer:
   ```bash
   ./prdt connect --host <ip>:9000 --decoder openh264 --codec h264 --host-pubkey <pubkey>
   ```

6. Confirm viewer shows live desktop content at ≥ 30 fps.

7. `pidstat -p $(pgrep -f "prdt host") 1 30`: host `%CPU` typically < 10 %
   on Intel N100 1080p60 (vs OpenH264 SW 25-40 %).

8. Tear down: viewer Ctrl+C → host watchdog logs session timeout
   within ~6 s. Reconnect to confirm clean re-session.

### Known issues / follow-ups (P5B-2a-successor)

- **SPA_PARAM_Buffers POD**: still constructed via pipewire-rs's
  `PodSerializer`. Not yet observed to fail (the wire-error happens
  before `param_changed` delivers a negotiated Format) but the same
  pipewire-rs `Object` serializer is in the path. Track as
  "potentially needs the same rewrite if multi-compositor smoke flags it".

- **Multi-compositor verification deferred**: KDE 6 (kwin), Sway
  (xdg-desktop-portal-wlr), and Hyprland are NOT verified here. Each
  compositor backend has its own EnumFormat validator and may accept
  or reject our bytes differently. See P5C / P5C-3 smoke matrix.

- **DMABUF / FrameInput::Dmabuf**: still uses BGRA mmap + CPU memcpy.
  Zero-copy DMABUF lands together with the encoder-side
  `FrameInput::Dmabuf` arm in P5C-2.
```

- [ ] **Step 2: Update STATUS.md — P5C-1 known-issue resolution + new P5B-2a-successor entry**

Open `docs/superpowers/STATUS.md`. Find the existing P5C-1 entry's
"Real-device smoke" paragraph (around line 500-518 in the merged
master state) and append at the very end of that paragraph (before
the `**P5B-2a-successor (next branch, not P5C-1)**` block):

```markdown
    Resolved by P5B-2a-successor (commit <merge-sha-tbd>, branch
    `phase-p5b2a-successor-libspa-pod-rewrite`): `wayland_portal::format::build()`
    rewritten on a `libspa-sys` raw-FFI PodBuilder; GNOME 46 mutter
    accepts the rewritten EnumFormat POD and the pipewire stream
    reaches `state: Streaming` with BGRA frames flowing to the VAAPI
    encoder. See walkthrough §L.
```

Then add a new top-level entry under the `## 2. Phase 別状態` section,
inserted in chronological order after P5C-1 (around line 502):

```markdown
- **P5B-2a-successor (`phase-p5b2a-successor-libspa-pod-rewrite`, 2026-05-13)**:
  Resolves the GNOME 46 Wayland frame-ingestion blocker called out in
  the P5C-1 known-issue list. Rewrites
  `crates/media-linux/src/wayland_portal/format.rs::build()` against a
  new `libspa-sys` raw-FFI safe wrapper (`pod_builder.rs`) so the
  outbound `SPA_PARAM_EnumFormat` POD matches the byte layout mutter
  expects. The `pipewire::spa::pod::serialize::PodSerializer` path is
  retained for `parse()` (inbound) and for `SPA_PARAM_Buffers` (not yet
  observed to fail).
  - New `crates/media-linux/src/wayland_portal/pod_builder.rs`: safe
    RAII wrapper over `spa_pod_builder_*` raw FFI. `PodBuilder::new` +
    `push_object` + `ObjectScope` (drop = pop) + `add_id_property` /
    `add_choice_id_enum` / `add_choice_rectangle_range` /
    `add_choice_fraction_range`. Overflow callback grows the backing
    `Vec<u8>` and reinits the builder pointer. All `unsafe` calls are
    contained inside this module behind `// SAFETY:` comments
    (cursor.rs convention).
  - `format.rs::build()` rewritten on PodBuilder. Signature + return
    type (`BuiltParams`) unchanged so the caller in `stream.rs` is not
    touched. `parse()` and all 5 parse-side tests are unchanged.
  - **Compile-time ABI pin**: `const _: () = { if size_of::<spa_pod_builder_state>() != 16 { panic!(...) } };`
    catches any future libspa-sys struct-layout drift at build time.
  - `crates/media-linux/Cargo.toml`: `libspa-sys = "0.9"` promoted to
    direct dep (currently transitive via pipewire).
  - **Tests**: 10 new unit tests in `pod_builder::tests` (2 primitive
    round-trip + overflow + 2 object scope + 3 Choice helpers) + 1 new
    integration test `tests/format_golden.rs` (byte-for-byte against
    `tests/fixtures/enum_format_golden.bin`) + all 8 existing
    `format::tests` still green. Workspace `cargo test --workspace`
    clean. Cross-platform CI (Linux + Windows) green.
  - **Real-device smoke (N100 Intel Alder Lake-N iGPU, Ubuntu 24.04
    GNOME 46 Wayland, 2026-05-13)**: pipewire stream reaches
    `state: Streaming`, `param_changed` delivers a valid negotiated
    Format POD, `WaylandPortalCapturer` emits BGRA frames to the
    VAAPI encoder, viewer renders live desktop content at ≥ 30 fps.
    Host CPU on 1080p60 5 Mbps: **<observed-pct>%** (target < 10 %).
  - **Out of scope (deferred)**: `SPA_PARAM_Buffers` POD rewrite
    (defer until proven needed), `FrameInput::Dmabuf` integration
    (P5C-2), multi-compositor matrix (KDE/Sway/Hyprland — P5C /
    P5C-3), pipewire-rs upstream contribution to fix the Object/Choice
    serializer.
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §L (GNOME 46 Wayland VAAPI end-to-end).
```

Replace `<merge-sha-tbd>` and `<observed-pct>` with the real values
once the PR merges + the smoke logs the pidstat result.

- [ ] **Step 3: Commit docs**

```bash
git add docs/superpowers/p5b1-smoke-walkthrough.md docs/superpowers/STATUS.md
git commit -m "P5B-2a-successor T9: walkthrough §L + STATUS entry + P5C-1 known-issue resolution"
```

- [ ] **Step 4: Open the PR**

```bash
gh pr create \
    --base master \
    --head phase-p5b2a-successor-libspa-pod-rewrite \
    --title "P5B-2a-successor: libspa POD wire-format rewrite (Wayland portal EnumFormat)" \
    --body "$(cat <<'EOF'
Resolves the GNOME 46 Wayland frame-ingestion blocker called out as a
known issue in P5C-1 (merge commit 1a94809).

## Summary
- New `crates/media-linux/src/wayland_portal/pod_builder.rs` (safe
  RAII wrapper over `spa_pod_builder_*` raw FFI)
- `wayland_portal::format::build()` rewritten on top of `PodBuilder`;
  signature unchanged so `stream.rs` is not touched
- `libspa-sys = "0.9"` promoted to direct dep (currently transitive)
- 10 new pod_builder unit tests + 1 byte-for-byte golden integration
  test (`tests/format_golden.rs` against
  `tests/fixtures/enum_format_golden.bin`)
- N100 GNOME 46 Wayland real-device smoke verified: pipewire stream
  reaches `state: Streaming`, BGRA frames flow to the P5C-1 VAAPI
  encoder, viewer renders live desktop, walkthrough §L documents the
  full procedure.

## Test plan
- [x] Cargo test workspace clean on Linux container
- [x] Clippy clean on Linux container
- [x] Cross-platform CI (Linux + Windows) green
- [x] Real-device smoke on N100 / GNOME 46 / Ubuntu 24.04 reaches
      `state: Streaming` and renders live desktop in viewer
- [x] Host CPU measured under VAAPI: <observed-pct>% (< 10 % target)

Spec: docs/superpowers/specs/2026-05-13-p5b2a-successor-libspa-pod-rewrite-design.md
Plan: docs/superpowers/plans/2026-05-13-p5b2a-successor-libspa-pod-rewrite.md

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Replace `<observed-pct>` with the recorded value before submitting.
The PR auto-runs the Windows + Linux CI checks; both must be green
before merge.

---

## Self-review

**Spec coverage (§ refs to design doc):**
- §1 Goal — covered by T6 (build rewrite) + T8 (N100 smoke DoD)
- §3.1 PodBuilder API — T2 (new + finish + add_int_primitive), T3 (add_id/rectangle/fraction primitives), T4 (push_object + ObjectScope + add_id_property), T5 (3 Choice helpers)
- §3.2 build() rewrite — T6
- §3.3 Cargo.toml direct dep — T1
- §3.4 parse() unchanged — confirmed by T6 step 3 (existing parse-side tests pass without modification)
- §4.1 Golden test — T7
- §4.2 N100 smoke — T8 + T9 §L walkthrough
- §5 Component table — matches T1-T9 file map at the top of this plan
- §6 Error handling — overflow callback covered by T2 step 5 + 6, OOM panic contract documented in T2 step 3 comment, debug_assert pop-null in T4 step 3
- §7 Risks — ABI drift mitigation = T1 step 4 compile-time size check; fixture-vs-smoke ordering = T7/T8 interlock comment; varargs avoidance = noted in T5 step 3 by using single-property builder ops
- §8 Cross-platform CI bar — T9 step 4 PR step verifies; T6 step 4 + T7 step 5 cover Linux workspace tests
- §9 DoD checklist — every item maps to a step above
- §10/§11 follow-ups / open questions — captured in T9 §L "Known issues / follow-ups" block

**Placeholder scan:** Two intentional `<placeholder>` tokens (`<observed-pct>`, `<merge-sha-tbd>`, `<n100-ip>`, `<pubkey-from-host-startup-log>`) appear only in command examples where the actual value is environment-specific and must be filled in at execution time. These are not "fill in details" plan failures — they are explicitly-marked runtime substitutions. No "TBD" / "TODO" / "implement later" / "add appropriate error handling" anywhere.

**Type consistency:**
- `PodBuilder` / `ObjectScope` types used consistently from T2 onward
- Method names: `add_int_primitive`, `add_id_primitive`, `add_rectangle_primitive`, `add_fraction_primitive`, `push_object`, `add_id_property`, `add_choice_id_enum`, `add_choice_rectangle_range`, `add_choice_fraction_range`, `finish` — all defined in T2-T5 and referenced unchanged in T6/T7
- `BuiltParams` (existing struct) preserved by T6 step 2
- `spa_sys::*` constants (`SPA_POD_PROP_FLAG_MANDATORY=8`, `SPA_POD_PROP_FLAG_DONT_FIXATE=16`, `SPA_TYPE_OBJECT_Format=262147`, `SPA_PARAM_EnumFormat=3`, `SPA_VIDEO_FORMAT_*`, `SPA_FORMAT_*`, `SPA_MEDIA_TYPE_video`, `SPA_MEDIA_SUBTYPE_raw`, `SPA_CHOICE_Enum`, `SPA_CHOICE_Range`, `SPA_VERSION_POD_BUILDER_CALLBACKS`) — all verified to exist in the bindings file we surveyed during planning
- `pipewire::spa::pod::{Pod, Value, ChoiceValue, ChoiceEnum}` and `pipewire::spa::utils::{Fraction, Id, Rectangle}` — same imports the existing `format.rs` already uses
