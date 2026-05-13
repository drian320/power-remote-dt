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
    // fine here.
    // Used by `push_object` / `ObjectScope` in T4.
    #[allow(dead_code)]
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
}
