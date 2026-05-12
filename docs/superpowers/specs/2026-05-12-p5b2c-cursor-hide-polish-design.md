# P5B-2c: OS-Native Cursor Hide + P5B-2b Review Polish — Design

**Status:** Draft (2026-05-12)
**Predecessor:** `phase-p5b2b-cursor-metadata-matrix-complete` (commit `be7a4a5`)
**Branch:** `phase-p5b2c-cursor-hide-polish`

## 1. Goal

Two bundled concerns:

1. **OS-native cursor hide**: when the viewer window has focus AND the host is sending cursor metadata (`cursor_state.visible() == true`), suppress the viewer's OS-native cursor so the user sees only the composited host cursor. Without this, P5B-2b's composited cursor sits on top of the OS arrow → "two cursors visible" UX bug.

2. **Reviewer polish bundle**: address the MEDIUM/LOW items both reviewers (code-reviewer + Codex) flagged in P5B-2b but didn't gate merge on:
   - Cursor forwarder task not in `tokio::join!` / no `CancellationToken` integration
   - `LinuxSwFactory` slot stashing `cursor_rx` before `build_video_producer_with` succeeds (stale slot on failure)
   - `alpha_blend_bgra` missing `debug_assert!` on src buffer length
   - `protocol_version: 3` literals remaining in protocol crate tests (cosmetic)
   - `cursor.rs` module-header comment references obsolete fallback approach

## 2. Scope

| Item | Approach | Why |
|---|---|---|
| OS cursor hide trigger | `focused == true` AND `cursor_state.visible() == true` | Self-correcting; falls back to visible OS cursor when host is Embedded or hasn't sent first bitmap yet |
| Frame region detection | Frame fills window (verified via Explore agent); no letterbox logic exists today | "Mouse over frame" == "mouse in window" |
| Window focus event | Add `WindowEvent::Focused(bool)` arm to existing `window_event` dispatch | Catchall `_ => {}` at lib.rs:634 currently discards it |
| Cross-platform | winit 0.30 `Window::set_cursor_visible(bool)` works on both Win + Linux | Same `Arc<Window>` held in `PlatformRender` for both renderers |
| ShouldHide check | Run on every `WindowEvent::Focused` AND every `ControlMessage::CursorUpdate` apply | Both can toggle the predicate |
| MOD_INVALID auto-retry (P5B-2a follow-up) | **NOT** in this phase | Separate concern; bundle into P5B-2d if surfaces in real-machine smoke |
| Sway / Hyprland matrix | **NOT** in this phase | P5C |
| Windows D3D11 cursor overlay full impl | **NOT** in this phase | Separate Windows follow-up branch |

## 3. Architecture

### 3.1 `ViewerShared` extension

Add a `focused: Arc<std::sync::Mutex<bool>>` field next to the existing `cursor: Arc<std::sync::Mutex<CursorState>>`. Initialize to `true` (winit window starts focused on creation).

### 3.2 Event loop

In `crates/viewer/src/lib.rs::ViewerApp::window_event`, add:

```rust
WindowEvent::Focused(focused) => {
    if let Ok(mut g) = shared.focused.lock() {
        *g = focused;
    }
    update_os_cursor_visibility(&r, &shared);
}
```

(`r` is the existing `PlatformRender` reference; `shared` is the `Arc<ViewerShared>`.)

In the `CursorUpdate` arm added by P5B-2b T4, append a call to `update_os_cursor_visibility` after `cursor.apply(...)`. Position-only updates change `visible()`'s answer only when the cached bitmap changes from absent → present.

### 3.3 The predicate helper

New free function in `crates/viewer/src/cursor_state.rs`:

```rust
/// Compute whether to hide the OS-native cursor.
///
/// Hide when: viewer window has focus AND host is actively emitting a
/// visible cursor bitmap (`CursorState::visible()` true). This means an
/// Embedded-mode host (no CursorUpdate messages) leaves the OS cursor
/// visible — the user always has at least one cursor.
pub fn should_hide_os_cursor(focused: bool, state: &CursorState) -> bool {
    focused && state.visible()
}
```

Caller (in `lib.rs::update_os_cursor_visibility`):

```rust
fn update_os_cursor_visibility(r: &PlatformRender, shared: &ViewerShared) {
    let focused = shared.focused.lock().ok().map(|g| *g).unwrap_or(true);
    let hide = shared
        .cursor
        .lock()
        .ok()
        .is_some_and(|c| crate::cursor_state::should_hide_os_cursor(focused, &c));
    r.window().set_cursor_visible(!hide);
}
```

### 3.4 Polish — cursor forwarder cancellation integration

In `crates/host/src/lib.rs` (search for the cursor forwarder block around line 898-910 per P5B-2b T3):

- Clone `cancel.clone()` into a `cancel_cursor`.
- Wrap the `cursor_rx.recv().await` in a `tokio::select!` with `cancel_cursor.cancelled()` arm.
- Capture the `JoinHandle` and include it in the `tokio::join!` macro at line ~1330 alongside `video`, `input`, etc.

This matches the surrounding pattern (video / input / audio / clipboard / outgoing / watchdog tasks all use this shape).

### 3.5 Polish — `LinuxSwFactory` slot ordering

In `crates/media-linux/src/policy.rs::LinuxSwFactory::create` (WaylandPortal arm), move `*slot = Some(cursor_rx);` to AFTER `build_video_producer_with(...)` succeeds. If the build fails, the slot stays empty and the next session retries cleanly.

### 3.6 Polish — `alpha_blend_bgra` debug_assert

In `crates/viewer/src/platform/linux.rs::alpha_blend_bgra`, add at the top:

```rust
debug_assert_eq!(
    src.len(),
    (src_w as usize) * (src_h as usize) * 4,
    "alpha_blend_bgra: src buffer size mismatch"
);
debug_assert_eq!(
    dst.len(),
    (dst_w as usize) * (dst_h as usize) * 4,
    "alpha_blend_bgra: dst buffer size mismatch"
);
```

Surface mismatches in dev builds without runtime cost in release.

### 3.7 Polish — `protocol_version: 3` literal cleanup

In `crates/protocol/src/control.rs` and `crates/protocol/src/wire.rs`, find `protocol_version: 3,` literals in test cases and bulk-replace with `protocol_version: 4,`. Tests don't exercise the version gate; this is purely documentation hygiene.

### 3.8 Polish — `cursor.rs` module-header comment

Update the file-header comment in `crates/media-linux/src/wayland_portal/cursor.rs` to point at the actual P5B-2b production path (stream.rs uses `dequeue_raw_buffer` + `*pw_buf.buffer`), replacing the obsolete fallback description.

## 4. Tests

- **New**: `cursor_state::should_hide_os_cursor_only_when_focused_and_visible` — table test:
  - `(focused=true,  visible=true)`  → hide
  - `(focused=true,  visible=false)` → show
  - `(focused=false, visible=true)`  → show
  - `(focused=false, visible=false)` → show
- **No new test for the winit event arm** — winit can't be driven in unit tests without a windowing context; the logic is covered by the predicate test.
- **Existing P5B-2b 15 tests + this 1 = 16 tests on top of master**.

## 5. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | `set_cursor_visible(false)` race with modal dialogs that restore the cursor | `update_os_cursor_visibility` re-runs on every `Focused` event; modal close fires `Focused(true)` and re-asserts |
| 2 | User drags cursor outside the viewer window | `Focused(false)` fires on focus loss; cursor restored. winit handles this consistently across X11/Wayland/Win32 |
| 3 | Forwarder cancellation integration changes shutdown timing | Existing tasks already use this pattern; the cursor forwarder joins symmetrically with no new race surface |
| 4 | Slot ordering change leaves old `cursor_rx` orphaned on retry | Slot was idempotently overwritten anyway — no behaviour change in the happy path; just removes a fragile "what if?" |

## 6. Out of scope

- **MOD_INVALID renegotiation auto-retry** — separate concern; gather smoke data first
- **Sway / Hyprland / wlroots matrix** — P5C
- **Windows D3D11 cursor overlay full implementation** — Windows follow-up branch with cross-platform CI
- **HiDPI cursor scaling refinement** — logical-pixel passthrough remains; revisit if smoke reveals issues
- **Letterboxed frame region** — frame fills window today; "mouse over frame" check defers until letterboxing exists

## 7. DoD

- `prdt-viewer` clippy clean (`-D warnings`)
- `prdt-host`, `prdt-media-linux`, `prdt-media-policy`, `prdt-protocol` clippy clean
- Affected-crate slice lib tests green
- X11 contract test regression guard: 3 pass / 1 ignored
- 1 new test (`should_hide_os_cursor` table test)
- Real-machine smoke deferred to walkthrough §J (TODO in T4)

## 8. References

- winit 0.30 `Window::set_cursor_visible` — [docs.rs](https://docs.rs/winit/0.30/winit/window/struct.Window.html#method.set_cursor_visible)
- P5B-2b reviewer findings: `code-reviewer` agent transcript + Codex artifact at `.omc/artifacts/ask/codex-p5b-2b-branch-commits-master-head-on-home-ubuntu-project-pow-2026-05-12T09-25-53-089Z.md`
- Predecessor `cursor_state.rs` + `wayland_portal/cursor.rs` introduced in P5B-2b commits `f80c9fc` + `8651da0`
