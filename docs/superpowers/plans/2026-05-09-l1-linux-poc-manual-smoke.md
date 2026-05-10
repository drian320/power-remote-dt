# L1 Linux PoC — Manual Smoke Checklist (Platform Crates Scope)

> **Scope note (post-execution reality check):** L1 was originally planned to land
> end-to-end host↔viewer smoke on WSL2. During execution we discovered that
> `crates/host/src/lib.rs` is unconditionally Windows-coupled (every key
> import — `D3d11Device`, `DxgiNvencProducer`, `HwHevcEncoder`, `MfH265Encoder`,
> the `prdt_input_win` free functions — is unguarded), not the `#[cfg(windows)]`
> sibling-friendly structure the plan assumed. Doing the host wiring in L1 would
> have required a large refactor outside the planned scope. We therefore stopped
> L1 at T10 (platform crates complete) and split host/viewer wiring + end-to-end
> smoke into a separate **L1.5** plan (TBD).
>
> What L1 delivers: `prdt-media-linux` and `prdt-input-linux` are fully
> implemented, unit-test green, and resolve cleanly under `cargo check
> --target x86_64-unknown-linux-gnu`. The trait surface is exercised, real
> X11/uinput/RandR/clipboard paths are coded, and per-crate integration
> `#[ignore]` tests prove they work against a live X server (verified
> manually during T3 / T9 on WSLg).

Target environment: WSL2 Ubuntu 22.04+ on the same machine that ran the L1 build.

## Environment prerequisites

- [ ] Rust toolchain ≥ 1.85 (`rustc --version`). L1 used `rustc 1.95.0` after
  `rustup default stable`. The original `nightly-1.88.0` from April 2025
  fails on the `zmij 1.0.21` transitive dep that uses an unstable intrinsic.
- [ ] X11 display server reachable (`xdpyinfo | head -3` returns version info)
- [ ] User in `input` group OR `/dev/uinput` writable: `groups | grep input` or
  `ls -l /dev/uinput`. If not: `sudo usermod -aG input $USER` and re-login,
  OR temporarily `sudo chmod 666 /dev/uinput`.
- [ ] System dev packages (one-time install):
  ```bash
  sudo apt-get install -y \
      pkg-config \
      libglib2.0-dev libdbus-1-dev \
      libxcb-shm0-dev libxcb-randr0-dev libxcb-xfixes0-dev \
      libxkbcommon-dev libwayland-dev libxdo-dev libssl-dev \
      libgtk-3-dev libpango1.0-dev libcairo2-dev \
      libgdk-pixbuf-2.0-dev libatk1.0-dev libayatana-appindicator3-dev
  ```
  (The GTK + ayatana-appindicator stack is pulled in by `prdt-gui-host`'s
  unconditional `tray-icon` dep; it doesn't gate on platform.)
- [ ] (optional) `xclip` or `xsel` for clipboard cross-checks from another process.

## Build (workspace, both targets)

- [ ] `cargo check --workspace`
  Expect: `Finished` clean. Pre-existing warnings: 1 in `prdt-gui-host`
  (`unreachable expression`) and 1 future-incompat note for `ashpd v0.8.1`.
  Do NOT count these against L1; they exist on master.

- [ ] `cargo check --workspace --target x86_64-unknown-linux-gnu`
  Expect: same green + same pre-existing warnings.

## Per-crate test sweep (the core L1 deliverable)

- [ ] `cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu --lib -- --skip xshm_capture_one_frame`
  Expect: **16 passed; 0 failed; 0 ignored; 1 filtered out**.

- [ ] `cargo test -p prdt-input-linux --target x86_64-unknown-linux-gnu --lib -- --skip x11_clipboard_set_then_get --skip open_uinput_succeeds_with_permission --skip live_virtual_desktop_rect_returns_sensible_value`
  Expect: **13 passed; 0 failed; 0 ignored; 3 filtered out**.

## Per-crate clippy

- [ ] `cargo clippy --target x86_64-unknown-linux-gnu -p prdt-media-linux -p prdt-input-linux --all-targets -- -D warnings`
  Expect: `Finished` (no errors, no warnings).

## Live X11 / uinput integration tests (best-effort)

These are `#[ignore]`-gated; they require a real X display and `/dev/uinput`
access. WSLg satisfies both with the steps above.

- [ ] `cargo test -p prdt-media-linux --target x86_64-unknown-linux-gnu -- --ignored xshm_capture_one_frame`
  Expect: 1 passed. Captures one frame from the WSLg root window via XShm
  (or warn-falls-back to plain XGetImage if MIT-SHM is unavailable). Frame
  buffer length matches `width * height * 4`.

- [ ] `cargo test -p prdt-input-linux --target x86_64-unknown-linux-gnu -- --ignored open_uinput_succeeds_with_permission`
  Expect: 1 passed. Just verifies `/dev/uinput` opens without `EACCES`.
  Doesn't actually create a virtual device (that's T11.5 territory).

- [ ] `cargo test -p prdt-input-linux --target x86_64-unknown-linux-gnu -- --ignored x11_clipboard_set_then_get_round_trips`
  Expect: 1 passed. `write_clipboard_text("hello-l1")` then `read_clipboard_text()` == `"hello-l1"`. Owner thread spawns silently.

- [ ] `cargo test -p prdt-input-linux --target x86_64-unknown-linux-gnu -- --ignored live_virtual_desktop_rect_returns_sensible_value`
  Expect: 1 passed. RandR returns a non-degenerate `MonitorRect`.

## End-to-end smoke (deferred to L1.5)

Not testable in L1 because `prdt host` and `prdt connect` don't have Linux
wiring yet. Once L1.5 lands, the original 9-step end-to-end checklist
(launch host → connect viewer → mouse / keyboard / clipboard) becomes
runnable. Recorded here for reference:

1. Start `prdt host`, observe `X11 connected, MIT-SHM available, host
   listening on UDP <port>` log lines.
2. From a second terminal, `prdt connect <host_id>`. Handshake completes
   in <5s, winit window opens, frames appear.
3. Mouse movement in the viewer window propagates to the host's WSLg
   pointer.
4. Keypress in viewer arrives at host (`xev | head -20` confirms).
5. Clipboard text copied in viewer is readable on host with `xclip -sel c -o`.
6. Disconnect (Esc) and reconnect succeeds within ~5s.
7. Clean shutdown on Ctrl+C, no panics.
8. (best-effort) `prdt gui` opens a GUI window on Linux. Failure is
   acceptable; goal is panic-free.
9. (informational) Record `e2e_p99` from `RUST_LOG=prdt_host=info` for ~30s.

## Failure triage

- **`/dev/uinput` open denied** → `sudo chmod 666 /dev/uinput` for the
  smoke (or fix groups + re-login).
- **`MIT-SHM extension unavailable`** → expected on some WSLg setups;
  XGetImage fallback should still work, just slower.
- **`zmij 1.0.21` build failure** → `rustup update stable` (toolchain too
  old for the unstable intrinsic).
- **System lib missing (glib / dbus / xcb / pango / etc.)** → install via
  the apt block above.

## Pre-existing fixes landed alongside L1

Two pre-existing Windows-only bugs blocked Linux baseline `cargo check`
and were fixed in commit `2830a3e`:

1. `crates/media-win/build.rs` referenced `bindgen::Builder` unconditionally,
   but `bindgen` is only declared as a Windows build-dep. Added
   `#[cfg(target_os = "windows")]` on both generator functions and the
   `main()` call sites.
2. `crates/latency-bench/src/bin/bench-matrix.rs` had `#![cfg(windows)]`
   inner attribute → empty file on Linux → cargo errored
   "main function not found in crate prdt_bench_matrix". Replaced with
   per-item `#[cfg(windows)]` gating + a non-Windows `fn main()` stub
   that prints "Windows-only" and exits 1.

These are independent of L1 functionality and do not change Windows behavior.
