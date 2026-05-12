# P5B-2c Implementation Plan — OS Cursor Hide + Polish

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`.

**Goal:** Hide viewer's OS-native cursor when host is emitting visible cursor metadata + bundle reviewer MEDIUM/LOW polish from P5B-2b.

**Architecture:** New `should_hide_os_cursor(focused, &CursorState) -> bool` predicate; `ViewerShared` gains `focused: Arc<Mutex<bool>>`; `WindowEvent::Focused` arm + `CursorUpdate` arm both call `update_os_cursor_visibility(window, shared)`. Polish items: cursor forwarder joins via `CancellationToken` + `tokio::join!`; `LinuxSwFactory::create` stashes `cursor_rx` after success; `alpha_blend_bgra` gains `debug_assert_eq!`; protocol test literals bumped 3→4; `cursor.rs` header comment refreshed.

**Tech Stack:** Rust 1.85, winit 0.30 (already in viewer), tokio mpsc/CancellationToken.

**Constraints:** Container-only build (`./scripts/dev-container.sh`). No new workspace deps.

**Spec:** `docs/superpowers/specs/2026-05-12-p5b2c-cursor-hide-polish-design.md`.

---

## Task 1: `should_hide_os_cursor` predicate + ViewerShared focused field

**Files:**
- Modify: `crates/viewer/src/cursor_state.rs`
- Modify: `crates/viewer/src/lib.rs`

- [ ] **Step 1: Write the failing predicate test**

Append to `crates/viewer/src/cursor_state.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn should_hide_os_cursor_only_when_focused_and_visible() {
        let mut empty = CursorState::new();
        assert!(!super::should_hide_os_cursor(false, &empty));
        assert!(!super::should_hide_os_cursor(true, &empty));

        empty.apply(1, 0, 0, 0, 0, Some(prdt_protocol::CursorBitmap {
            width: 2, height: 1, bgra: vec![0u8; 8]
        }));
        assert!(empty.visible());
        assert!(!super::should_hide_os_cursor(false, &empty));
        assert!(super::should_hide_os_cursor(true, &empty));

        let mut invisible = CursorState::new();
        invisible.apply(2, 0, 0, 0, 0, Some(prdt_protocol::CursorBitmap {
            width: 0, height: 0, bgra: vec![]
        }));
        assert!(!invisible.visible());
        assert!(!super::should_hide_os_cursor(true, &invisible));
    }
```

Run:
```bash
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu cursor_state::tests::should_hide 2>&1 | head -20
```
Expected: compile failure (`should_hide_os_cursor` doesn't exist).

- [ ] **Step 2: Implement the predicate**

Add to `crates/viewer/src/cursor_state.rs` (alongside the `CursorState` impl block):

```rust
/// Compute whether to suppress the viewer's OS-native cursor.
///
/// Hide when the viewer window has focus AND the host is actively
/// rendering a visible cursor bitmap. Self-correcting: an Embedded-mode
/// host (no CursorUpdate messages) leaves `state.visible() == false`, so
/// the OS cursor stays visible and the user always has at least one
/// pointer.
pub fn should_hide_os_cursor(focused: bool, state: &CursorState) -> bool {
    focused && state.visible()
}
```

- [ ] **Step 3: Run the predicate test**

```bash
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu cursor_state
```
Expected: 4 cursor_state tests pass (3 existing + 1 new).

- [ ] **Step 4: Extend `ViewerShared` with `focused` field**

Open `crates/viewer/src/lib.rs`. Find the `ViewerShared` struct (around line 428). Add:

```rust
pub focused: Arc<std::sync::Mutex<bool>>,
```

Initialize wherever `ViewerShared` is constructed: `focused: Arc::new(std::sync::Mutex::new(true))`.

- [ ] **Step 5: Add `update_os_cursor_visibility` helper + wire into event arm**

In `crates/viewer/src/lib.rs`, near the `ViewerApp::window_event` body, add:

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

(`PlatformRender` is the type held in `ViewerApp`; check the actual name and adjust if needed. `r.window()` may instead be `r.window` direct field access — adapt.)

Add the focus arm in `window_event`'s match:

```rust
WindowEvent::Focused(focused) => {
    if let Ok(mut g) = shared.focused.lock() {
        *g = focused;
    }
    update_os_cursor_visibility(r, &shared);
}
```

In the existing `CursorUpdate { id, position_x, ... }` arm (added by P5B-2b T4), after the `apply(...)` call, add:

```rust
update_os_cursor_visibility(r, &shared);
```

(Verify `r` and `shared` are in scope at that point; if the dispatch happens in a different scope, restructure or capture-clone as needed. The CursorUpdate arm runs inside the recv task; `shared` is `recv_shared` there. `r` (the renderer) is NOT in the recv task's scope — only in the main event loop. **Resolution**: the recv task only updates `shared.cursor`; the OS cursor visibility recomputation has to happen on the main thread. Schedule it via `event_loop.create_proxy()` sending a custom `UserEvent::CursorRecomputeVisibility` that the main loop handles. OR: just call `update_os_cursor_visibility` from the `WindowEvent::Focused` arm + check on every `CursorMoved` event (cheap; mutex lock + set_cursor_visible). The simpler path is the CursorMoved trigger.)

**Implementer decision**: choose the simpler path. If `CursorMoved` is the trigger:

```rust
WindowEvent::CursorMoved { position, .. } => {
    // [existing emit_mouse_move call …]
    update_os_cursor_visibility(r, &shared);
}
```

Cursor moves are at most ~250Hz; the mutex lock + winit call is negligible.

- [ ] **Step 6: Run viewer tests + clippy**

```bash
./scripts/dev-container.sh cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu
./scripts/dev-container.sh cargo clippy -p prdt-viewer --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
```

Expected: green. `cursor_state` tests = 5 (4 existing + 1 new = should_hide).

- [ ] **Step 7: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/viewer/src/cursor_state.rs crates/viewer/src/lib.rs
git commit -m "$(cat <<'EOF'
P5B-2c T1: OS-native cursor hide on focus + visible host cursor

Adds should_hide_os_cursor(focused, &CursorState) predicate to the
viewer's cursor_state module: returns true ONLY when the viewer window
has focus AND the host is rendering a visible cursor bitmap. Embedded
mode (no CursorUpdate messages) leaves state.visible() == false so the
OS cursor stays visible — the user always has at least one pointer.

ViewerShared gains focused: Arc<Mutex<bool>> tracked via the existing
WindowEvent dispatch. update_os_cursor_visibility() reads both fields
and calls winit's Window::set_cursor_visible(!hide). Trigger sites:

- WindowEvent::Focused — explicit focus state change
- WindowEvent::CursorMoved — handles the "cursor entered/left a region"
  case implicitly (winit fires CursorMoved on entry/exit) and naturally
  re-asserts visibility after modal-dialog interactions

1 new test: should_hide_os_cursor_only_when_focused_and_visible (table
over the four (focused, visible) corner cases).
EOF
)"
```

---

## Task 2: Cursor forwarder `CancellationToken` + `tokio::join!` integration

**Files:**
- Modify: `crates/host/src/lib.rs`

- [ ] **Step 1: Locate the cursor forwarder block**

```bash
grep -n "cursor_rx\|cursor_to_control" crates/host/src/lib.rs | head -10
```

The block is around line 898-910 per P5B-2b T3's commit. Re-verify.

- [ ] **Step 2: Restructure to use `tokio::select!` + capture JoinHandle**

Inside the `#[cfg(target_os = "linux")]` cursor-forwarder block, change from `tokio::spawn(async move { while let Some(c) = cursor_rx.recv().await { ... } })` to:

```rust
let cursor_transport = Arc::clone(&transport);
let cancel_cursor = cancel.clone();
let cursor_task = tokio::spawn(async move {
    loop {
        tokio::select! {
            _ = cancel_cursor.cancelled() => break,
            msg = cursor_rx.recv() => {
                match msg {
                    Some(c) => {
                        if let Err(e) = cursor_transport
                            .send_control(prdt_media_linux::policy::cursor_to_control(c))
                            .await
                        {
                            tracing::debug!(?e, "cursor send failed");
                            break;
                        }
                    }
                    None => break, // sender dropped
                }
            }
        }
    }
});
```

(Verify the exact path of `cursor_to_control` — it lives in `media-linux/src/policy.rs` per T3. Adapt the `use` if needed.)

For non-Linux targets, the forwarder is gated behind `#[cfg(target_os = "linux")]`. The `tokio::join!` block at line ~1330 needs a `cfg`'d join arm OR you can wrap the entire `cursor_task` in a `#[cfg(target_os = "linux")] let cursor_task = ...; #[cfg(not(target_os = "linux"))] let cursor_task: Option<JoinHandle<()>> = None;` shape — the cleanest depends on the surrounding code. Pick whichever has minimal diff.

If the `tokio::join!` macro is rigid, the simplest fix is to push `cursor_task.unwrap_or_else(|| spawn_noop())` or replicate the cfg gate. Look at how `audio_task` (or similar conditionally-spawned tasks) is handled in this file and mirror.

- [ ] **Step 3: Include in `tokio::join!`**

At the `tokio::join!(video, input, audio_task, clip_task, outgoing_task, watchdog)` line (~1330), add `cursor_task` (cfg-gated if needed).

- [ ] **Step 4: Verify the lib compiles + clippy clean**

```bash
./scripts/dev-container.sh cargo check -p prdt-host --target x86_64-unknown-linux-gnu 2>&1 | tail -10
./scripts/dev-container.sh cargo clippy -p prdt-host --target x86_64-unknown-linux-gnu --no-deps 2>&1 | tail -10
```

`prdt-host` lib tests pull GUI deps that can't compile in the container (pre-existing). Use `cargo check` to verify Rust-level correctness without linking the GUI deps.

If `cargo check` succeeds, proceed.

- [ ] **Step 5: Commit**

```bash
./scripts/dev-container.sh cargo fmt --all
git add crates/host/src/lib.rs
git commit -m "$(cat <<'EOF'
P5B-2c T2: cursor forwarder joins CancellationToken + tokio::join!

P5B-2b T3 shipped the cursor forwarder as a detached tokio::spawn with
no cancellation integration — it exited only when cursor_rx returned
None (sender dropped at session-end). The session shutdown sequence
hits cancel.cancel() and then awaits tokio::join!(...) on the other
session tasks, but cursor_task wasn't in that join, leading to a
race where the forwarder might still be running at the start of the
next session iteration.

Restructured to mirror the pattern used by video / input / audio /
clip / outgoing / watchdog:
  - tokio::select! on cancel_cursor.cancelled() AND cursor_rx.recv()
  - JoinHandle captured and included in the tokio::join! at session
    teardown
  - send_control error returns break (transport closed)
  - sender-dropped (None) returns break (stream ended)

No new tests — the change is structural and exercised by every
existing cancel-driven session smoke.
EOF
)"
```

---

## Task 3: LinuxSwFactory slot ordering + alpha_blend debug_assert + literal cleanup + cursor.rs comment

**Files:**
- Modify: `crates/media-linux/src/policy.rs`
- Modify: `crates/viewer/src/platform/linux.rs`
- Modify: `crates/protocol/src/control.rs`
- Modify: `crates/protocol/src/wire.rs`
- Modify: `crates/media-linux/src/wayland_portal/cursor.rs`

These four small edits bundle into one commit.

- [ ] **Step 1: Fix `LinuxSwFactory::create` slot ordering**

Open `crates/media-linux/src/policy.rs`. Find the WaylandPortal arm of `LinuxSwFactory::create` (around line 263-280 per T3 commit). Move the line `*slot = Some(cursor_rx);` (or similar — verify actual variable name) to AFTER `build_video_producer_with(...)` succeeds. The current order is:

```rust
*slot = Some(cursor_rx);
// build_video_producer_with returns Ok / Err
```

Change to:

```rust
let producer = build_video_producer_with(...)?;  // ? propagates Err
*slot = Some(cursor_rx);
Ok(producer)
```

If `?` doesn't work because of return-type wrapping (`Box<dyn VideoProducer>`), use explicit `match`:

```rust
match build_video_producer_with(...) {
    Ok(p) => {
        *slot = Some(cursor_rx);
        Ok(p)
    }
    Err(e) => Err(e),
}
```

- [ ] **Step 2: Add `debug_assert_eq!` to `alpha_blend_bgra`**

In `crates/viewer/src/platform/linux.rs`'s `alpha_blend_bgra` function, at the top of the body:

```rust
debug_assert_eq!(
    src.len(),
    (src_w as usize).saturating_mul(src_h as usize).saturating_mul(4),
    "alpha_blend_bgra: src buffer size mismatch"
);
debug_assert_eq!(
    dst.len(),
    (dst_w as usize).saturating_mul(dst_h as usize).saturating_mul(4),
    "alpha_blend_bgra: dst buffer size mismatch"
);
```

`saturating_mul` defends against negative `dst_w`/`dst_h` casts wrapping.

- [ ] **Step 3: Bulk-bump `protocol_version: 3` test literals**

```bash
grep -n "protocol_version: 3" crates/protocol/src/control.rs crates/protocol/src/wire.rs
```

For each match, replace `protocol_version: 3,` with `protocol_version: 4,`. These are in test fixtures only — no functional impact.

```bash
sed -i 's/protocol_version: 3,/protocol_version: 4,/g' \
    crates/protocol/src/control.rs \
    crates/protocol/src/wire.rs
```

Verify with grep that no `protocol_version: 3` remains in any non-comment context.

- [ ] **Step 4: Refresh `cursor.rs` module-header comment**

Open `crates/media-linux/src/wayland_portal/cursor.rs`. The file header (around lines 38-49) describes the SpaBufferLike trait's "production impl deferred to call site" pattern. After P5B-2b's CRITICAL fix, `stream.rs` uses `dequeue_raw_buffer` + `*pw_buf.buffer` to obtain a `*const spa_buffer`, wrapped in a small `SpaRawPtr(*const spa_buffer)` adapter struct.

Replace the obsolete paragraph with:

```rust
//! # Production adapter (stream.rs)
//!
//! `crates/media-linux/src/wayland_portal/stream.rs::.process` obtains the
//! raw `*mut pw_buffer` via `Stream::dequeue_raw_buffer` (pipewire-rs 0.9.2
//! `stream/mod.rs:154`), follows the `.buffer` field to a
//! `*const spa_sys::spa_buffer`, and wraps it in a local `SpaRawPtr` struct
//! that implements `SpaBufferLike` by returning the stored pointer. The
//! raw API is unsafe (caller must `queue_raw_buffer` the pointer back) but
//! it avoids the `transmute_copy` UB of the earlier `Buffer` private-field
//! extraction approach. See P5B-2b critical fix commit `63b0802`.
```

- [ ] **Step 5: Run gates**

```bash
./scripts/dev-container.sh cargo fmt --all
./scripts/dev-container.sh cargo clippy -p prdt-protocol -p prdt-transport \
    -p prdt-media-core -p prdt-media-sw -p prdt-media-policy -p prdt-media-linux \
    -p prdt-viewer -p prdt-viewer-overlay \
    --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --target x86_64-unknown-linux-gnu --lib \
    -p prdt-protocol -p prdt-media-core -p prdt-media-sw -p prdt-media-policy \
    -p prdt-media-linux -p prdt-transport -p prdt-viewer
./scripts/dev-container.sh cargo test -p prdt-media-linux \
    --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: green. No new tests in this task — the changes are defensive or cosmetic and existing tests cover the affected paths.

- [ ] **Step 6: Commit**

```bash
git add crates/media-linux/src/policy.rs \
        crates/viewer/src/platform/linux.rs \
        crates/protocol/src/control.rs \
        crates/protocol/src/wire.rs \
        crates/media-linux/src/wayland_portal/cursor.rs
git commit -m "$(cat <<'EOF'
P5B-2c T3: P5B-2b reviewer polish bundle

Five small reviewer follow-ups from P5B-2b in one commit:

1. LinuxSwFactory::create stashes cursor_rx AFTER build_video_producer
   succeeds (was: before, leaving a stale slot on factory error).

2. alpha_blend_bgra gains debug_assert_eq! on src.len + dst.len. Surfaces
   buffer-size mismatches in dev builds without runtime cost in release.
   saturating_mul defends against negative-cast wrap.

3. Test fixtures in protocol/src/{control,wire}.rs bulk-bumped
   protocol_version: 3 -> 4 to match the runtime constant. Cosmetic
   (these tests don't exercise the version gate) but keeps reviewer
   sweep clean.

4. wayland_portal/cursor.rs module-header comment refreshed to point
   at the actual production adapter (dequeue_raw_buffer + SpaRawPtr in
   stream.rs from CRITICAL-fix commit 63b0802) instead of the
   pre-CRITICAL-fix fallback paragraph.

No new tests — changes are defensive (#1, #2), cosmetic (#3), or
documentation (#4). Existing tests cover the affected paths.
EOF
)"
```

---

## Task 4: STATUS + walkthrough §J + final gate

**Files:**
- Modify: `docs/superpowers/STATUS.md`
- Modify: `docs/superpowers/p5b1-smoke-walkthrough.md`

- [ ] **Step 1: Bump STATUS header + append P5B-2c entry**

In `docs/superpowers/STATUS.md`:

```markdown
**Latest tag:** `phase-p5b2c-cursor-hide-polish-complete`
```

Insert after the P5B-2b entry (before `### **C.` section):

```markdown
- **P5B-2c (`phase-p5b2c-cursor-hide-polish-complete`, 2026-05-12)**:
  OS-native cursor hide + P5B-2b reviewer polish bundle.
  - Viewer hides OS-native cursor when window has focus AND host is
    emitting a visible cursor bitmap (`cursor_state::should_hide_os_cursor`).
    Self-correcting: Embedded-mode host leaves OS cursor visible, so user
    always has at least one pointer. winit `Window::set_cursor_visible`
    is cross-platform (works on both Linux softbuffer + Windows D3D11).
    Trigger sites: `WindowEvent::Focused` + `WindowEvent::CursorMoved`.
    `ViewerShared` gains `focused: Arc<Mutex<bool>>` field.
  - **Reviewer polish bundle** (P5B-2b MEDIUM/LOW items from code-reviewer +
    Codex): cursor forwarder task now integrates `CancellationToken` +
    joins via `tokio::join!`; `LinuxSwFactory::create` stashes `cursor_rx`
    AFTER `build_video_producer_with` succeeds (no stale slot on error);
    `alpha_blend_bgra` gains `debug_assert_eq!` on src/dst buffer length;
    `protocol_version: 3` literals in protocol crate test fixtures bumped
    to 4; `wayland_portal/cursor.rs` module-header comment refreshed to
    point at the production `dequeue_raw_buffer` adapter.
  - **Tests**: 1 new (`should_hide_os_cursor_only_when_focused_and_visible`
    table over (focused, visible) corner cases). Total P5B-2b+P5B-2c new
    tests since master = 16.
  - **Out of scope (deferred)**: Sway / Hyprland / wlroots matrix (P5C);
    Windows D3D11 cursor overlay full implementation (follow-up branch);
    MOD_INVALID renegotiation auto-retry (P5B-2a follow-up; smoke data
    needed first); HiDPI cursor scaling refinement.
  - **Smoke walkthrough**: `docs/superpowers/p5b1-smoke-walkthrough.md`
    §P5B-2c Section J (OS cursor hide verification).
```

- [ ] **Step 2: Append walkthrough §J**

Edit `docs/superpowers/p5b1-smoke-walkthrough.md`. After the P5B-2b §I block, append:

```markdown
---

## P5B-2c — OS-native cursor hide

### Section J — Two-cursor regression check

**Pre-conditions:**
- v4 host + v4 viewer.
- A compositor that advertises `CursorMode::Metadata` (GNOME mutter ≥ 42, KDE kwin ≥ 5.27).
- Viewer window initially focused.

**Steps:**

1. Start the host + viewer per §G or §H.
2. Move the host's cursor. Confirm the composited host cursor tracks in the viewer.
3. **Verify ONE cursor visible**:
   - Hover the OS cursor over the viewer window. The OS-native cursor should DISAPPEAR.
   - Only the composited host cursor remains visible.
4. **Verify focus-loss restores OS cursor**:
   - Alt-Tab to another window. The viewer loses focus.
   - Hover back over the viewer window. The OS-native cursor reappears (focus is on the other window).
5. **Verify focus-regain hides OS cursor again**:
   - Click on the viewer window to focus it.
   - The OS-native cursor disappears once the cursor is over the window.
6. **Verify Embedded-mode fallback keeps OS cursor visible**:
   - Connect to a host running with `--capture-backend wayland` on a compositor that does NOT advertise Metadata mode (e.g. xdg-desktop-portal-wlr without cursor metadata patches).
   - The cursor is baked into the frame; `cursor_state.visible() == false`.
   - The OS-native cursor stays visible (user always has at least one pointer).

### Known issues / follow-ups (P5B-2c)

- **Modal dialog cursor restore race**: opening a modal dialog (e.g. a permissions prompt) within the viewer may briefly re-show the OS cursor. The visibility helper re-asserts on the next `CursorMoved` event, so the flash is bounded to one frame. Tracked but not pre-emptively gated.
```

- [ ] **Step 3: Final gate**

```bash
./scripts/dev-container.sh cargo fmt --all
./scripts/dev-container.sh cargo clippy -p prdt-protocol -p prdt-transport \
    -p prdt-media-core -p prdt-media-sw -p prdt-media-policy -p prdt-media-linux \
    -p prdt-viewer -p prdt-viewer-overlay \
    --all-targets --target x86_64-unknown-linux-gnu -- -D warnings
./scripts/dev-container.sh cargo test --target x86_64-unknown-linux-gnu --lib \
    -p prdt-protocol -p prdt-media-core -p prdt-media-sw -p prdt-media-policy \
    -p prdt-media-linux -p prdt-transport -p prdt-viewer
./scripts/dev-container.sh cargo test -p prdt-media-linux \
    --test capture_source_contract --target x86_64-unknown-linux-gnu
```

Expected: green. 1 new test (should_hide) passing.

- [ ] **Step 4: Commit STATUS + walkthrough**

```bash
git add docs/superpowers/STATUS.md docs/superpowers/p5b1-smoke-walkthrough.md
git commit -m "$(cat <<'EOF'
docs(STATUS): record P5B-2c — OS cursor hide + reviewer polish

Adds the phase-p5b2c-cursor-hide-polish-complete entry under §1.
Header bumped from phase-p5b2b-cursor-metadata-matrix-complete.

Walkthrough §J added: two-cursor regression check (verify OS cursor
hides on focus + visible host cursor, restores on focus loss,
Embedded fallback keeps OS cursor).
EOF
)"
```

- [ ] **Step 5: Stop — controller handles PR**

---

## Cross-task notes

- All cargo via `./scripts/dev-container.sh`.
- `prdt-host` lib tests block in the container on gdk-sys (pre-existing); use `cargo check` to verify host-side Rust correctness.
- No new workspace deps.
- Pre-existing flaky `transport::probe_test::two_transports_find_each_other` excluded as before.
