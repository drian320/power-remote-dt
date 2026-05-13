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

use std::os::raw::{c_int, c_void};

use pipewire::spa::sys as spa_sys;

// Compile-time assertion: spa_pod_builder_state is 16 bytes on every
// libspa-sys 0.9.x build we ship. If a future bump changes this we want
// a hard compile error, not silent stack corruption.
const _: () = {
    if std::mem::size_of::<spa_sys::spa_pod_builder_state>() != 16 {
        panic!("spa_pod_builder_state ABI drift; pin libspa-sys version");
    }
};

/// Owned builder backed by a heap `Vec<u8>` that auto-grows via the
/// `spa_pod_builder_callbacks::overflow` hook. Single-use: call
/// `finish()` to extract the bytes.
pub struct PodBuilder {
    // Plain `Vec<u8>` — no Box needed. The load-bearing invariant is that
    // `buf`'s *heap allocation* (returned by `Vec::as_mut_ptr`) remains at
    // the same address while libspa is writing into it. `Vec<u8>` already
    // provides that: the heap pointer is stable across moves of the `Vec`
    // itself, because a move only copies the three-word (ptr/len/cap) header,
    // leaving the heap buffer in place. libspa points into the Vec heap via
    // `raw.data`, and the `Self` address is stable once `init_if_needed` has
    // run because every write method holds `&mut self`, preventing any further
    // move.
    buf: Vec<u8>,
    raw: spa_sys::spa_pod_builder,
    // Frame stack is small (≤ 2 in practice: object + choice). Vec is
    // fine here. Used by `push_object` / `ObjectScope`.
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
        let buf = Vec::<u8>::with_capacity(Self::INITIAL_CAPACITY);
        let raw: spa_sys::spa_pod_builder = unsafe { std::mem::zeroed() };
        let callbacks: spa_sys::spa_pod_builder_callbacks =
            unsafe { std::mem::zeroed() };
        // NOTE: We do NOT call spa_pod_builder_init or set_callbacks here.
        // `self` is stack-allocated and will be moved on return; storing
        // `&self as *mut _` as the callback data would produce a dangling
        // pointer. Full initialisation is deferred to `init_if_needed`,
        // called from the first write method after `self` has settled.
        Self {
            buf,
            raw,
            frames: Vec::with_capacity(2),
            callbacks,
        }
    }

    /// Idempotent full initialisation: called at the start of every write
    /// method. After the first call, `raw.data` is non-null so subsequent
    /// calls are skipped via the null-pointer check.
    fn init_if_needed(&mut self) {
        if !self.raw.data.is_null() {
            return;
        }
        let cap = self.buf.capacity();
        let ptr = self.buf.as_mut_ptr() as *mut c_void;
        // SAFETY: `self` is at its final address (callers hold `&mut self`
        // so no further move will occur while this reference is live).
        // spa_pod_builder_init zeroes the builder and points it at the buf.
        unsafe {
            spa_sys::spa_pod_builder_init(&mut self.raw, ptr, cap as u32);
        }
        self.reattach_callbacks();
    }

    /// Re-point only `raw.data` and `raw.size` after a realloc.
    /// Must NOT call `spa_pod_builder_init` because that resets
    /// `state.offset` and the frame chain, losing all progress.
    fn repoint_buf(&mut self) {
        // SAFETY: &mut self is exclusive and aliases nothing.
        unsafe { Self::repoint_buf_raw(self as *mut Self) }
        // Re-register callbacks because spa_pod_builder_set_callbacks writes
        // into raw.callbacks (spa_callbacks), which stores our `self` ptr.
        self.reattach_callbacks();
    }

    /// Raw-pointer variant of `repoint_buf` used from `overflow_trampoline`.
    /// Does NOT call `reattach_callbacks` — the callbacks slot already points
    /// at `Self` and re-calling `set_callbacks` from inside the overflow
    /// callback would be reentrant into libspa.
    ///
    /// # Safety
    /// `this` must be a valid, non-null `*mut PodBuilder`. The caller must
    /// ensure no Rust reference to `*this` (or any of its fields) exists
    /// concurrently.
    unsafe fn repoint_buf_raw(this: *mut Self) {
        let cap = (*this).buf.capacity();
        let ptr = (*this).buf.as_mut_ptr() as *mut c_void;
        // SAFETY: We write directly into the two pointer/size fields of the
        // builder. libspa's `spa_pod_builder_raw` only reads `data`,
        // `size`, and `state.offset`; the frame chain and callback pointers
        // remain valid because the builder struct itself has not moved.
        (*this).raw.data = ptr;
        (*this).raw.size = cap as u32;
    }

    /// Register `overflow_trampoline` on the builder. Must be called after
    /// every `spa_pod_builder_init` (which zeroes the callbacks field).
    fn reattach_callbacks(&mut self) {
        self.callbacks.version = spa_sys::SPA_VERSION_POD_BUILDER_CALLBACKS;
        self.callbacks.overflow = Some(Self::overflow_trampoline);
        // SAFETY: `self.callbacks` lives inside `self` (stable address while
        // the caller holds `&mut self`). `self` ptr is valid for the same
        // reason. libspa stores these pointers in `raw.callbacks`.
        unsafe {
            spa_sys::spa_pod_builder_set_callbacks(
                &mut self.raw,
                &self.callbacks,
                self as *mut _ as *mut c_void,
            );
        }
    }

    /// Overflow trampoline invoked by libspa when an append would exceed
    /// `raw.size`. We grow `buf` to at least `size` bytes (libspa hands
    /// us `offset + append_size` as the required total), update only
    /// `raw.data` / `raw.size`, and return 0 so libspa's `memcpy` proceeds.
    unsafe extern "C" fn overflow_trampoline(data: *mut c_void, size: u32) -> c_int {
        // SAFETY: libspa calls this with the `data` pointer we passed to
        // `set_callbacks` (= raw *mut Self). libspa guarantees single-threaded
        // non-reentrant invocation. We deliberately avoid materializing a
        // `&mut Self` here because libspa retains an aliasing `*mut spa_pod_builder`
        // into the same struct for the duration of this call; reconstituting
        // a Rust `&mut` while that pointer is live would violate stacked
        // borrows. All field accesses go through `*mut Self` raw projection.
        let this = data as *mut Self;
        let need = size as usize;
        let cur_cap = (*this).buf.capacity();
        if cur_cap < need {
            // Grow to at least `size` bytes, minimum doubling current capacity.
            // buf.len() is always 0 (we never call set_len during writes, only
            // in finish()). reserve(n) guarantees capacity >= len + n = n.
            let want = std::cmp::max(cur_cap * 2, need);
            (*this).buf.reserve(want);
            // Only update data/size pointers — do NOT reset state.offset or
            // frames, and do NOT call reattach_callbacks (reentrant into libspa).
            Self::repoint_buf_raw(this);
        }
        0
    }

    /// Append a bare `SPA_TYPE_Int` POD. Only used by the `add_int_primitive`
    /// path before objects are introduced (Task 2 baseline test).
    pub fn add_int_primitive(&mut self, val: i32) {
        self.init_if_needed();
        // SAFETY: builder is valid (init_if_needed guarantees raw is set up);
        // val is plain i32.
        unsafe {
            spa_sys::spa_pod_builder_int(&mut self.raw, val);
        }
    }

    /// Append a bare `SPA_TYPE_Id` POD.
    pub fn add_id_primitive(&mut self, id: u32) {
        // SAFETY: builder is valid; spa_pod_builder_id takes a u32.
        self.init_if_needed();
        unsafe {
            spa_sys::spa_pod_builder_id(&mut self.raw, id);
        }
    }

    /// Append a bare `SPA_TYPE_Rectangle` POD.
    pub fn add_rectangle_primitive(&mut self, width: u32, height: u32) {
        // SAFETY: builder is valid; spa_pod_builder_rectangle writes
        // a SPA_TYPE_Rectangle (8 bytes) at the current offset.
        self.init_if_needed();
        unsafe {
            spa_sys::spa_pod_builder_rectangle(&mut self.raw, width, height);
        }
    }

    /// Append a bare `SPA_TYPE_Long` POD.
    pub fn add_long_primitive(&mut self, val: i64) {
        // SAFETY: builder is valid (init'd via init_if_needed); val is plain i64.
        self.init_if_needed();
        unsafe {
            spa_sys::spa_pod_builder_long(&mut self.raw, val);
        }
    }

    /// Append a bare `SPA_TYPE_Fraction` POD.
    pub fn add_fraction_primitive(&mut self, num: i32, denom: i32) {
        // SAFETY: builder is valid; spa_pod_builder_fraction writes
        // a SPA_TYPE_Fraction (8 bytes) at the current offset. libspa-sys
        // 0.9.2 bindings type the args as u32 although the SPA POD spec
        // treats them as signed; cast at the FFI boundary.
        self.init_if_needed();
        unsafe {
            spa_sys::spa_pod_builder_fraction(
                &mut self.raw,
                num as u32,
                denom as u32,
            );
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
        self.buf
    }
}

impl Default for PodBuilder {
    fn default() -> Self {
        Self::new()
    }
}

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

    /// Append a `Choice<Id>` property (Enum form). The caller supplies
    /// `flags` explicitly (e.g. `MANDATORY | DONT_FIXATE` for VideoFormat/
    /// VideoModifier, or `0` for size/framerate using the OBS pattern).
    pub fn add_choice_id_enum(
        &mut self,
        key: u32,
        flags: u32,
        default: u32,
        alternatives: &[u32],
    ) {
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

    /// Append a `Choice<Rectangle>` property (Range form). The caller
    /// supplies `flags` explicitly (e.g. `MANDATORY | DONT_FIXATE` or `0`).
    pub fn add_choice_rectangle_range(
        &mut self,
        key: u32,
        flags: u32,
        default: (u32, u32),
        min: (u32, u32),
        max: (u32, u32),
    ) {
        // SAFETY: same as add_id_property; builder is inside an object frame.
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, flags);
        }
        let mut frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        // SAFETY: push_choice opens a SPA_TYPE_Choice subframe. Pop
        // matches via the local frame variable below.
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
        // SAFETY: pop matches the push_choice above.
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut frame as *mut _,
            );
        }
    }

    /// Append a `Choice<Long>` property (Enum form). The caller supplies
    /// `flags` explicitly. Used for `VideoModifier` where the value type
    /// is signed i64 (DRM modifiers).
    pub fn add_choice_long_enum(
        &mut self,
        key: u32,
        flags: u32,
        default: i64,
        alternatives: &[i64],
    ) {
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
                0,
            );
        }
        self.builder.add_long_primitive(default);
        for &alt in alternatives {
            self.builder.add_long_primitive(alt);
        }
        // SAFETY: pop matches the push_choice above.
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut choice_frame as *mut _,
            );
        }
    }

    /// Add a scalar `SPA_TYPE_Fraction` property to the current object
    /// (no Choice wrapper). Used for `VideoFramerate` where mutter
    /// expects a scalar `0/1` ("no fixed rate") rather than a Choice
    /// Range. The actual rate range is then declared via a separate
    /// `VideoMaxFramerate` property as a Choice<Fraction> Range.
    pub fn add_fraction_property(&mut self, key: u32, num: i32, denom: i32) {
        // SAFETY: builder is inside an object frame; spa_pod_builder_prop
        // with flags=0 emits a non-Choice property.
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, 0);
        }
        self.builder.add_fraction_primitive(num, denom);
    }

    /// Append a `Choice<Fraction>` property (Range form). The caller
    /// supplies `flags` explicitly (e.g. `MANDATORY | DONT_FIXATE` or `0`).
    pub fn add_choice_fraction_range(
        &mut self,
        key: u32,
        flags: u32,
        default: (i32, i32),
        min: (i32, i32),
        max: (i32, i32),
    ) {
        // SAFETY: same as add_id_property; builder is inside an object frame.
        unsafe {
            spa_sys::spa_pod_builder_prop(&mut self.builder.raw, key, flags);
        }
        let mut frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        // SAFETY: push_choice opens a SPA_TYPE_Choice subframe. Pop
        // matches via the local frame variable below.
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
        // SAFETY: pop matches the push_choice above.
        unsafe {
            let _ = spa_sys::spa_pod_builder_pop(
                &mut self.builder.raw,
                &mut frame as *mut _,
            );
        }
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
        // Ensure the builder is initialized (lazy-init pattern).
        self.init_if_needed();
        // Reserve a fresh spa_pod_frame inside our Vec so libspa can
        // write into it. Push a zero-initialised frame, then take a
        // raw pointer to the just-pushed slot.
        // SAFETY: frame layout is plain POD; zeroing is valid.
        let frame: spa_sys::spa_pod_frame = unsafe { std::mem::zeroed() };
        // Pre-condition: capacity must be sufficient to push without reallocating.
        // If frames reallocates between push and pop, the raw `*mut spa_pod_frame`
        // libspa retains (via `raw.state`) becomes a dangling pointer into the
        // moved-from buffer. The Vec is pre-allocated with `with_capacity(2)` in
        // `new()`; deeper nesting requires bumping that capacity (or rewriting
        // the frame stack to use a stable-address container).
        debug_assert!(
            self.frames.len() < self.frames.capacity(),
            "PodBuilder frame stack would reallocate (len={}, cap={}); \
             increase `Vec::with_capacity(N)` in `PodBuilder::new` for deeper nesting",
            self.frames.len(),
            self.frames.capacity()
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::pod::deserialize::PodDeserializer;
    use pipewire::spa::pod::{ChoiceValue, Value};
    use pipewire::spa::utils::{ChoiceEnum, Id};

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
                spa_sys::SPA_POD_PROP_FLAG_MANDATORY | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
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
                spa_sys::SPA_POD_PROP_FLAG_MANDATORY | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
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

    /// Regression test for VideoModifier negotiation: GNOME 46 mutter on
    /// Intel iHD rejects EnumFormat with "no more input formats" if the
    /// consumer doesn't declare a Modifier Choice. We need to emit
    /// Choice<Long> Enum with MANDATORY|DONT_FIXATE, default LINEAR (0),
    /// alternatives [LINEAR, INVALID] per OBS / gnome-remote-desktop.
    #[test]
    fn choice_long_enum_round_trip_with_mandatory_dont_fixate() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_choice_long_enum(
                spa_sys::SPA_FORMAT_VIDEO_modifier,
                spa_sys::SPA_POD_PROP_FLAG_MANDATORY | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
                0i64,                  // DRM_FORMAT_MOD_LINEAR
                &[0i64, -1i64],        // LINEAR + INVALID
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
        assert_eq!(p.key, spa_sys::SPA_FORMAT_VIDEO_modifier);
        // MANDATORY|DONT_FIXATE = 8|16 = 24
        assert_eq!(p.flags.bits(), 8 | 16, "MANDATORY|DONT_FIXATE flags missing on Modifier Choice");
        match &p.value {
            Value::Choice(ChoiceValue::Long(c)) => match &c.1 {
                ChoiceEnum::Enum { default, alternatives } => {
                    assert_eq!(*default, 0i64, "default must be LINEAR (0)");
                    assert_eq!(alternatives.as_slice(), &[0i64, -1i64], "alternatives must be [LINEAR, INVALID]");
                }
                other => panic!("expected ChoiceEnum::Enum, got {other:?}"),
            },
            other => panic!("expected Choice<Long>, got {other:?}"),
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
                spa_sys::SPA_POD_PROP_FLAG_MANDATORY | spa_sys::SPA_POD_PROP_FLAG_DONT_FIXATE,
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

    /// Scalar Fraction property (not Choice). Used for VideoFramerate=0/1
    /// in the EnumFormat where mutter expects "no fixed rate" semantics.
    #[test]
    fn scalar_fraction_property_round_trip() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_fraction_property(spa_sys::SPA_FORMAT_VIDEO_framerate, 0, 1);
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
        assert_eq!(p.key, spa_sys::SPA_FORMAT_VIDEO_framerate);
        assert_eq!(p.flags.bits(), 0, "scalar property must have flags=0");
        match &p.value {
            Value::Fraction(f) => {
                assert_eq!(f.num, 0);
                assert_eq!(f.denom, 1);
            }
            other => panic!("expected scalar Fraction, got {other:?}"),
        }
    }

    /// Choice property with flags=0 (used for VideoSize / VideoFramerate
    /// in the OBS-pattern EnumFormat where the consumer is fine with
    /// whatever size/rate the producer offers, no DONT_FIXATE).
    #[test]
    fn choice_rectangle_range_with_zero_flags() {
        let mut b = PodBuilder::new();
        {
            let mut o = b.push_object(
                spa_sys::SPA_TYPE_OBJECT_Format,
                spa_sys::SPA_PARAM_EnumFormat,
            );
            o.add_choice_rectangle_range(
                spa_sys::SPA_FORMAT_VIDEO_size,
                0,  // no MANDATORY, no DONT_FIXATE
                (1920, 1080),
                (320, 240),
                (7680, 4320),
            );
        }
        let bytes = b.finish();
        let (_n, value) = PodDeserializer::deserialize_any_from(&bytes).expect("deserialise");
        let obj = match value {
            Value::Object(o) => o,
            _ => panic!(),
        };
        let p = &obj.properties[0];
        assert_eq!(p.flags.bits(), 0, "flags should be 0 when explicitly passed 0");
    }
}
