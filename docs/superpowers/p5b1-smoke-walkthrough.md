# P5B-1 Wayland Portal Foundation — Smoke Walkthrough

This document is the operator-facing smoke checklist for the
`phase-p5b1-wayland-portal-foundation-complete` tag.

**Foundation scope note**: T5 (PipeWire stream) and T6 (capturer glue)
are deferred to a successor branch on a host with pipewire >= 0.3.55.
The smoke sections below are honest about what the Foundation milestone
actually exercises: the probe, the CLI flag, the token persistence, and
the factory error path. No frames flow through the Wayland portal path in
this milestone. The WSLg X11 path is unaffected and is the primary
regression guard.

P5B-2 will add KDE / Sway / Hyprland sections and DMABUF zero-copy; for
now we verify GNOME dialog reachability + WSLg X11 regression + the
probe-priority log line.

---

## Section A — GNOME smoke (Foundation: dialog-fire reachability only, no frames)

**Pre-conditions:**
- Ubuntu 22.04+ GNOME (Wayland session).
- No `~/.config/prdt/portal-session.toml`.
- `prdt-host` built from this tag (`cargo build --release -p prdt-host`).

**Steps:**

1. Start the host with verbose tracing:

   ```bash
   RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow --headless \
       --capture-backend wayland 2>&1 | tee p5b1-gnome-run1.log
   ```

2. Expect the following log lines (probe + factory path):

   ```
   P5B-1 capture backend resolved choice=Wayland resolved=WaylandPortal
   ```

3. Expect a `FactoryError::Unavailable` error immediately after, because
   `WaylandPortalCapturer::new()` returns `NotImplemented` in the Foundation
   milestone. The host will exit with a clear diagnostic:

   ```
   failed to build video producer: Unavailable: Foundation-only milestone; T5/T6 deferred
   ```

   The host exits fast — no frames are produced and no consent dialog fires.
   This is intentional and correct for this milestone.

**What this proves:** T1–T4's plumbing (probe, CLI flag, factory routing,
capturer constructor stub) is reachable up to the point of factory
construction. The actual portal consent dialog and the PipeWire stream do
not fire because the capturer constructor short-circuits at `NotImplemented`.

**To exercise the full dialog path:** Run the successor branch that wires
T5/T6 on a host with pipewire >= 0.3.55. The session lifecycle code
(`PortalSession::start_with_token_opt` → `create_session` →
`select_sources` → `start` → `open_pipewire_remote`) is already present in
`crates/media-linux/src/wayland_portal/session.rs`; it simply is not called
through to completion without T5/T6's stream wiring.

---

## Section A' — Manual ashpd dialog test (optional, for future implementers)

Once T5/T6 land on a successor branch, the portal dialog can be exercised
independently of the full host binary by driving
`PortalSession::start_with_token_opt` directly. Two approaches:

1. **`cargo test -- --ignored`**: add a `#[test] #[ignore]` integration test
   in `crates/media-linux/tests/` that calls `start_with_token_opt(None)`
   on a real GNOME session, asserts the returned token is non-empty, and
   calls `close().await`. Run with:

   ```bash
   cargo test -p prdt-media-linux -- --ignored portal_dialog_fires
   ```

2. **`examples/` binary**: add
   `crates/media-linux/examples/portal_dialog_smoke.rs` that does the same
   in a small `tokio::main` binary:

   ```bash
   cargo run --example portal_dialog_smoke -p prdt-media-linux
   ```

Neither the test nor the example binary needs to be written now — this note
is a breadcrumb for the T5/T6 implementer so the dialog path can be verified
without running the full host stack.

---

## Section B — WSLg X11 regression (DoD #3)

This is the most important section in the Foundation milestone. It verifies
that T1–T4 did not regress the X11 path.

**Pre-conditions:**
- WSL2 Ubuntu with WSLg.
- `DISPLAY` is set (typically `:0`), `WAYLAND_DISPLAY` is unset inside WSL.
- `prdt-host` built from this tag.

**Steps:**

1. Confirm the environment inside WSL:

   ```bash
   echo "WAYLAND_DISPLAY=[$WAYLAND_DISPLAY]"
   echo "DISPLAY=[$DISPLAY]"
   # Expected: WAYLAND_DISPLAY=[]  DISPLAY=[:0]
   ```

2. Run the host in auto-probe mode (no `--capture-backend` override):

   ```bash
   RUST_LOG=info ./target/release/prdt-host --bitrate-mbps 5 --silent-allow --headless \
       2>&1 | tee p5b1-wslg-run.log
   ```

3. Expect the log lines:

   ```
   WAYLAND_DISPLAY unset; selecting X11 capture backend
   P5B-1 capture backend resolved choice=Auto resolved=X11Shm
   ```

4. Connect a viewer (or run a 30-second loopback bench). Confirm frames
   arrive with no behavioural change from the pre-P5B-1 baseline.
   Expected viewer-overlay HUD: `linux-openh264` codec line, frames per
   second >= 20 (WSLg sparse-desktop baseline).

5. Stop the host with Ctrl-C. Confirm no unexpected panics or error log
   lines related to capture or the Wayland portal code paths.

**This section should pass cleanly.** The X11 path (`X11ShmCapturer`) is
structurally identical to the pre-P5B-1 code — T1 refactored it behind the
`CaptureSource` trait but did not change its logic.

---

## Section C — Probe priority verification (DoD #4)

These steps verify that `--capture-backend` overrides the auto-probe and
that the Foundation error path is clearly reported.

### C1 — Forced Wayland (expect Foundation error, not ashpd error)

With `WAYLAND_DISPLAY` unset (WSLg or any non-Wayland host):

```bash
WAYLAND_DISPLAY= RUST_LOG=info ./target/release/prdt-host \
    --capture-backend wayland --bitrate-mbps 5 --headless --silent-allow \
    2>&1 | head -30
```

Expect:
- Log: `P5B-1 capture backend resolved choice=Wayland resolved=WaylandPortal`
- Then a hard failure from the factory with the Foundation-milestone marker:
  ```
  failed to build video producer: Unavailable: Foundation-only milestone; T5/T6 deferred
  ```
- The host exits. No ashpd D-Bus call is made (the error occurs in
  `WaylandPortalCapturer::new()` before any session is created).

**Important distinction:** In the Foundation milestone, the error comes from
the capturer stub, not from ashpd or D-Bus. On a live GNOME session you
would still see the same `Unavailable` error — the capturer is a
`NotImplemented` stub regardless of what the portal reports.

### C2 — Forced X11 (even with WAYLAND_DISPLAY set)

```bash
WAYLAND_DISPLAY=wayland-fake RUST_LOG=info ./target/release/prdt-host \
    --capture-backend x11 --bitrate-mbps 5 --headless --silent-allow \
    2>&1 | head -30
```

Expect:
- Log: `P5B-1 capture backend resolved choice=X11 resolved=X11Shm`
- X11 path proceeds normally (or fails with an X11 connection error if the X
  server is unavailable, but that is not a P5B-1 regression).

### C3 — Auto-probe with WAYLAND_DISPLAY set but portal unreachable

```bash
WAYLAND_DISPLAY=wayland-1 RUST_LOG=info ./target/release/prdt-host \
    --bitrate-mbps 5 --headless --silent-allow \
    2>&1 | head -30
```

Expect:
- Probe attempts `NameHasOwner("org.freedesktop.portal.Desktop")` on the
  session bus (1s timeout).
- If the portal is unreachable (typical on WSLg), auto-probe falls back:
  ```
  portal probe failed or timed out; falling back to X11
  P5B-1 capture backend resolved choice=Auto resolved=X11Shm
  ```
- X11 path proceeds.

---

## Out of scope (deferred)

- **PipeWire runtime (T5/T6)**: deferred to a successor branch on a host
  with pipewire >= 0.3.55. Ubuntu 22.04 ships pipewire 0.3.48; the current
  libspa Rust crate (>= 0.7) targets the post-0.3.55 C ABI. No version on
  crates.io builds on this dev box regardless of pin. See commit `684f43d`
  for the full rationale and the removed `[dependencies]` block.
- **DMABUF zero-copy**: deferred to P5B-2. All frames still go through CPU
  `bgra_to_i420`.
- **Multi-compositor smoke matrix (KDE / Sway / Hyprland)**: deferred to
  P5B-2.
- **Wayland-native input dispatch**: XTest under XWayland keeps working.
  Native Wayland input (libei) is deferred to P5B-2 / future.
- **HW encoder on Linux**: OpenH264 SW only. VAAPI / NVENC-Linux deferred
  to P5C.

---

## Known issues / follow-ups

- **PipeWire runtime deferred**: the successor branch implementing T5/T6
  should target a host with pipewire >= 0.3.55 (Ubuntu 24.04+, Fedora 39+,
  Arch current). The dep removal and the comment block pointing to the ABI
  mismatch are in commit `684f43d`. Do not attempt to unblock on Ubuntu 22.04
  by pinning an older libspa version — the ABI break (`spa_video_info_raw`
  struct field changes) means no published crate version works.
- **Probe timeout is 1s**: spec §11 noted a cold GNOME login might exceed
  this. If smoke on the successor branch shows false negatives (portal
  reachable but probe returns X11 due to slow dbus startup), bump the
  timeout to 3s in a follow-up commit. Do NOT bump pre-emptively.
- **`WaylandPortalCapturer::new()` is a `NotImplemented` stub**: the capturer
  constructor in `crates/media-linux/src/wayland_portal/capturer.rs` returns
  `CaptureError::NotImplemented` immediately. The factory (`policy.rs`)
  wraps this as `FactoryError::Unavailable` with the Foundation-milestone
  message. This is the correct behavior for this milestone — operators get
  a clear error instead of a silent X11 substitution.
