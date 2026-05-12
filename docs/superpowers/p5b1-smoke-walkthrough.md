# P5B-1 Wayland Portal — Smoke Walkthrough

This document is the operator-facing smoke checklist for the
`phase-p5b1-t5-t6-pipewire-runtime-complete` tag (P5B-1 successor).

The successor branch lands T5 (PipeWire stream) and T6 (capturer glue)
on top of the Foundation milestone. `--capture-backend wayland` now opens
the portal consent dialog, persists the RestoreToken, and feeds real
PipeWire frames into the OpenH264 path. The WSLg X11 path is unaffected
and remains the primary regression guard.

P5B-2 will add KDE / Sway / Hyprland sections and DMABUF zero-copy; for
now we verify GNOME real-frames + WSLg X11 regression + the
probe-priority log line.

---

## Section A — GNOME smoke (real consent dialog + real frames)

**Pre-conditions:**
- Ubuntu 24.04+ / Fedora 39+ / Arch with GNOME (Wayland session) AND
  `libpipewire-0.3 >= 0.3.55`.
- No `~/.config/prdt/portal-session.toml` (first-run path).
- `prdt-host` binary from this branch:
  - **Ubuntu 24.04+ host**: `cargo build --release -p prdt-host` then
    use `./target/release/prdt-host`.
  - **Ubuntu 22.04 host** (libpipewire 0.3.48 < required 0.3.55): build
    inside the Docker container:
    ```bash
    ./scripts/dev-container.sh cargo build --release -p prdt-host \
        --target x86_64-unknown-linux-gnu
    ```
    The binary lands in `target-docker/x86_64-unknown-linux-gnu/release/prdt-host`.
    Copy it to the target Wayland machine before running.

**Steps:**

1. Start the host with verbose tracing:

   ```bash
   RUST_LOG=info ./prdt-host --bitrate-mbps 5 --silent-allow --headless \
       2>&1 | tee p5b1-gnome-run1.log
   ```

   (No `--capture-backend` override — auto-probe will select WaylandPortal
   when `WAYLAND_DISPLAY` is set and the portal is reachable.)

2. Expect the following log lines in order:

   ```
   P5B-1 capture backend resolved choice=Auto resolved=WaylandPortal
   xdg-desktop-portal reachable; selecting Wayland capture backend
   portal session: starting has_token=false
   portal session: started pipewire_node_id=…
   ```

3. The OS consent dialog fires (GNOME "Allow screen sharing?" prompt).
   Click **Allow**. Frames begin flowing immediately after.

4. Confirm `~/.config/prdt/portal-session.toml` exists with mode `0600`:

   ```bash
   ls -l ~/.config/prdt/portal-session.toml
   stat -c "%a" ~/.config/prdt/portal-session.toml   # expect: 600
   ```

5. Connect a viewer:

   ```bash
   ./prdt-client connect <host-id>
   ```

   Expect the viewer overlay HUD to show `linux-openh264` codec line and
   frames-per-second >= 20 after the first IDR settles (typically < 3s).
   Run for at least 30 seconds.

6. Stop the host with Ctrl-C. Check the log does NOT contain:

   ```
   WaylandPortalCapturer dropped without explicit shutdown
   ```

   If it does fire, this is a known follow-up (see Known issues below);
   it is safe to proceed for dev iteration.

7. **Second run (token path):** Re-run the same command. Expect no dialog.
   Log should show:

   ```
   portal session: starting has_token=true
   portal session: started pipewire_node_id=…
   ```

**What this proves:** The full T5/T6/T7-rewire pipeline — portal session
open, PipeWire mainloop thread, frame channel, stride stripping, token
persist and restore — is wired end-to-end.

---

## Section A' — RestoreTokenRejected path (for implementers)

T5/T6 implement token-rejection recovery: if the compositor rejects the
saved token (e.g. the user revoked the portal grant from Settings), the
capturer deletes the stale token file and retries without a token,
causing the consent dialog to re-fire.

**To exercise this path manually:**

1. Run the host once and confirm `portal-session.toml` exists (Section A
   steps 1–4).
2. In GNOME Settings → Privacy → Screen Sharing (or equivalent), revoke
   the `prdt-host` grant.
3. Re-run the host. Expect the log to show:

   ```
   portal restore token rejected; deleting stale token and retrying without token
   portal session: starting has_token=false
   ```

   Followed by the consent dialog firing again.

This exercises the `RestoreTokenRejected` branch in
`WaylandPortalCapturer::new()` (`crates/media-linux/src/wayland_portal/capturer.rs`).

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
that the routing logic is clearly reported.

### C1 — Forced Wayland on a non-Wayland host (expect ashpd error, not stub error)

With `WAYLAND_DISPLAY` unset (WSLg or any non-Wayland host):

```bash
WAYLAND_DISPLAY= RUST_LOG=info ./target/release/prdt-host \
    --capture-backend wayland --bitrate-mbps 5 --headless --silent-allow \
    2>&1 | head -30
```

Expect:
- Log: `P5B-1 capture backend resolved choice=Wayland resolved=WaylandPortal`
- Then a hard failure from the factory because the portal is unreachable
  (ashpd D-Bus error or session bus unavailable):
  ```
  failed to build video producer: …
  ```
- The host exits. On the successor branch the capturer constructor now
  attempts a real ashpd session; on a non-Wayland host it fails at the
  D-Bus layer (not at a stub).

**Note:** The Foundation-milestone `Unavailable: Foundation-only milestone;
T5/T6 deferred` error is no longer emitted on this branch — the capturer
is fully wired.

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

- **Probe timeout is 1s**: spec §11 noted a cold GNOME login might exceed
  this. If smoke shows false negatives (portal reachable but probe returns
  X11 due to slow dbus startup), bump the timeout to 3s in a follow-up
  commit. Do NOT bump pre-emptively.
- **`parse_video_format` / `build_format_params` are staged stubs**:
  compositor default negotiation typically lands on BGRA on GNOME and KDE.
  If a compositor refuses to default, the log will show:
  ```
  negotiated format not BGRA/BGRx; aborting
  ```
  and frames will stop arriving. Tracked as a P5B-2 follow-up.
- **Drop-without-shutdown leak warn**: `WaylandPortalCapturer dropped
  without explicit shutdown()` may fire on Ctrl-C if the host's tokio
  runtime exits before calling `shutdown()` on the producer side cleanly.
  This is acceptable for dev iteration; revisit if it appears in clean
  shutdown paths (the `Drop` impl logs `warn!` intentionally so leaks are
  visible).

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

---

## P5B-2b — Cursor metadata + 2-compositor smoke matrix

### Section G — GNOME (mutter) cursor metadata

**Pre-conditions:**
- Ubuntu 24.04 GNOME (Wayland session); mutter ≥ 42.
- v4 `prdt-host` + v4 `prdt-viewer` from this branch.
- `xdg-desktop-portal-gnome` ≥ 42 (Metadata cursor mode landed in 41).

**Steps:**

1. Start the host with cursor-mode tracing:

   ```bash
   RUST_LOG=info,prdt_media_linux::wayland_portal=debug \
       ./prdt-host --bitrate-mbps 5 --silent-allow --headless \
       2>&1 | tee p5b2b-gnome-cursor-run.log
   ```

2. Click **Allow**. Expect:

   ```
   portal advertises Metadata cursor mode — using it
   ```

3. Connect a v4 viewer (`./prdt-viewer`).

4. Move the host's cursor. Expect the viewer's window cursor to track
   the host's pointer at near-zero latency (independent of video FPS).

5. Change cursor shape on the host (hover over a resize handle / text
   field). The viewer's cursor should update with the new shape within
   one frame.

### Section H — KDE (kwin) cursor metadata

**Pre-conditions:**
- Kubuntu 24.04 KDE (Wayland session); kwin ≥ 5.27.
- v4 `prdt-host` + v4 `prdt-viewer`.
- `xdg-desktop-portal-kde` ≥ 5.27.

**Steps:**

1. Start the host as in §G.
2. Click **Share** in the KDE dialog. Same expected log line ("portal
   advertises Metadata cursor mode").
3. Connect viewer. Same shape + position tracking verification.

### Section I — Embedded fallback regression

**Pre-conditions:** A compositor that does NOT advertise Metadata
(e.g. old GNOME 40 / `xdg-desktop-portal-wlr`).

**Expected log:**

```
portal does not advertise Metadata cursor mode — falling back to Embedded
```

Viewer shows the cursor baked into the frame (existing P5B-1 successor
behaviour); no `CursorUpdate` messages on the wire.

### Known issues / follow-ups (P5B-2b specific)

- **Windows D3D11 overlay**: stubbed; full pixel-shader draw lands in
  a Windows follow-up branch.
- **Sway / Hyprland / wlroots**: not in this matrix; revisit in P5C.
- **HiDPI cursor scaling**: cursor coordinates pass through as logical
  pixels; if the viewer has a different DPI than the host, the cursor
  position may be off-by-scale. Logged but not auto-corrected.
- **`SetCursor(NULL)` on Windows viewer**: hides OS-native cursor when
  the viewer window has focus + cursor is within the render rect.
  Restores on focus loss / cursor-leave. Race with modal dialogs may
  cause brief double-cursor flashes.

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
