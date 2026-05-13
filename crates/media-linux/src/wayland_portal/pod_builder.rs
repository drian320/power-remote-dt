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
