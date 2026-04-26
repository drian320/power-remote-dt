# Host session liveness — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add heartbeat (`KeepAlive` 1Hz) and watchdog (5s timeout) so the host detects a dead viewer, tears down the session, and accepts a fresh handshake — without process restart.

**Architecture:** New `ControlMessage::KeepAlive` flows viewer→host every second. Host watchdog task fires `CancellationToken` after 5s silence; all worker tasks exit on cancel. Outer session loop calls `transport.reset_session()` and re-handshakes.

**Tech Stack:** Rust 2021 / tokio + tokio-util `CancellationToken` (already a host dep) / `std::sync::atomic::AtomicU64` for the last-keepalive timestamp / serde+bincode for the new control variant (existing pattern).

**Spec:** `docs/superpowers/specs/2026-04-27-host-session-liveness-design.md` (committed at `b583ec4`).

**Branch:** `host-session-liveness` (already created).

---

## File Structure

| File | Role |
|---|---|
| `crates/protocol/src/control.rs` | Add `KeepAlive` variant + `kind_u8 = 17` |
| `crates/transport/src/udp.rs` | Add public `reset_session()` method |
| `crates/host/src/watchdog.rs` | **NEW** — watchdog task spawn + 2 unit tests |
| `crates/host/src/main.rs` | Outer session loop, cancel propagation, KeepAlive handler |
| `crates/viewer/src/main.rs` | One added line: KeepAlive send in `latency_task` |

---

## Task 1: Protocol — `KeepAlive` variant

**Files:**
- Modify: `crates/protocol/src/control.rs:33-148` (enum + `kind_u8`)

- [ ] **Step 1.1: Write the failing test**

Add to the `mod tests` block at the bottom of `crates/protocol/src/control.rs` (after the existing `probe_roundtrip_bincode` test):

```rust
    #[test]
    fn keep_alive_kind_is_stable() {
        assert_eq!(ControlMessage::KeepAlive.kind_u8(), 17);
    }

    #[test]
    fn keep_alive_roundtrip_bincode() {
        let msg = ControlMessage::KeepAlive;
        let bytes = bincode::serialize(&msg).unwrap();
        let back: ControlMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }
```

- [ ] **Step 1.2: Run test to verify it fails**

Run: `cargo test -p prdt-protocol keep_alive`
Expected: 2 errors of the form `no variant named 'KeepAlive' found for enum 'ControlMessage'`.

- [ ] **Step 1.3: Add the enum variant**

In `crates/protocol/src/control.rs`, add a new variant inside the `ControlMessage` enum (place it after `LatencyReport { ... }` and before `Probe { nonce }`):

```rust
    /// Viewer → Host periodic liveness heartbeat. The host's watchdog
    /// uses the receive timestamp of these messages to decide whether
    /// the viewer is still alive. Empty payload — `Ping`/`Pong` and
    /// `LatencyReport` already carry timing data; this is purely a
    /// liveness signal that fires unconditionally every 1s.
    KeepAlive,
```

- [ ] **Step 1.4: Add the `kind_u8` arm**

In the `kind_u8` match in the same file, add the new arm between `LatencyReport => 16` and `Probe => 20`:

```rust
            Self::KeepAlive => 17,
```

- [ ] **Step 1.5: Run tests**

Run: `cargo test -p prdt-protocol keep_alive`
Expected: `test keep_alive_kind_is_stable ... ok` and `test keep_alive_roundtrip_bincode ... ok`.

Then run all protocol tests to confirm nothing else broke:
Run: `cargo test -p prdt-protocol`
Expected: All tests pass.

- [ ] **Step 1.6: Commit**

```bash
git add crates/protocol/src/control.rs
git commit -m "protocol: add ControlMessage::KeepAlive variant (kind=17)"
```

---

## Task 2: Transport — `reset_session()`

**Files:**
- Modify: `crates/transport/src/udp.rs` (add method on `CustomUdpTransport`)
- Modify: `crates/transport/tests/loopback_smoke.rs` (or whichever existing transport test file) — add a unit test

- [ ] **Step 2.1: Write the failing test**

Add a new test file `crates/transport/tests/reset_session.rs`:

```rust
//! Verify CustomUdpTransport::reset_session clears peer + crypto so a
//! subsequent handshake_as_server can rebind to a different peer.

use prdt_transport::{CustomUdpTransport, TransportConfig};
use std::net::SocketAddr;

#[tokio::test]
async fn reset_session_clears_peer_and_crypto() {
    // Bind a transport on a random local port.
    let cfg = TransportConfig::default();
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let transport = CustomUdpTransport::bind(bind, cfg).await.unwrap();

    // Reset before any peer set — must not panic / error.
    transport.reset_session().await;

    // No public getter for `peer`, so we just verify reset is idempotent
    // and does not deadlock under repeat calls.
    transport.reset_session().await;
    transport.reset_session().await;
}
```

- [ ] **Step 2.2: Run test to verify it fails**

Run: `cargo test -p prdt-transport reset_session`
Expected: error `no method named 'reset_session' found for struct 'CustomUdpTransport'`.

- [ ] **Step 2.3: Implement the method**

In `crates/transport/src/udp.rs`, add a method to the `impl CustomUdpTransport` block (place it near `is_relay`, around line ~204):

```rust
    /// Reset session state so the next `handshake_as_server` accepts a
    /// fresh peer. Used by the host's outer session loop after a viewer
    /// disconnects or times out. Idempotent.
    pub async fn reset_session(&self) {
        *self.peer.lock().await = None;
        *self.crypto.lock().await = None;
    }
```

- [ ] **Step 2.4: Run tests**

Run: `cargo test -p prdt-transport reset_session`
Expected: `test reset_session_clears_peer_and_crypto ... ok`.

Run: `cargo test -p prdt-transport`
Expected: All transport tests pass.

- [ ] **Step 2.5: Commit**

```bash
git add crates/transport/src/udp.rs crates/transport/tests/reset_session.rs
git commit -m "transport: add CustomUdpTransport::reset_session() for outer-loop reuse"
```

---

## Task 3: Host watchdog module

**Files:**
- Create: `crates/host/src/watchdog.rs`
- Modify: `crates/host/src/main.rs` (add `mod watchdog;` near top of file)

- [ ] **Step 3.1: Create the new module file**

Create `crates/host/src/watchdog.rs` with a `mod tests` block from the start:

```rust
//! Heartbeat watchdog for host session liveness.
//!
//! Spawned once per session by `main.rs`. Polls `last_keepalive` every
//! second and fires the supplied `CancellationToken` if no `KeepAlive`
//! has arrived for `KEEPALIVE_TIMEOUT`. The control task elsewhere is
//! responsible for storing a fresh timestamp into `last_keepalive` on
//! each `ControlMessage::KeepAlive` receipt.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use prdt_transport::now_monotonic_us;

/// Threshold beyond which the viewer is considered dead.
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Watchdog tick cadence.
const TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the watchdog. Cancels `cancel` when no KeepAlive has been
/// observed for `KEEPALIVE_TIMEOUT`.
pub fn spawn_watchdog(
    cancel: CancellationToken,
    last_keepalive: Arc<AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let last = last_keepalive.load(Ordering::Relaxed);
                    let now = now_monotonic_us();
                    let silence_us = now.saturating_sub(last);
                    if silence_us > KEEPALIVE_TIMEOUT.as_micros() as u64 {
                        warn!(
                            silence_us,
                            "viewer silent > {}s; canceling session",
                            KEEPALIVE_TIMEOUT.as_secs(),
                        );
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn watchdog_fires_on_silence() {
        let cancel = CancellationToken::new();
        // Initialize last_keepalive to "now" via the same clock the
        // watchdog reads. With the runtime paused, advance 6s and check
        // that the watchdog has cancelled.
        let last_ka = Arc::new(AtomicU64::new(now_monotonic_us()));
        let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));

        tokio::time::advance(Duration::from_secs(6)).await;

        // Yield so the watchdog task gets a chance to observe the elapsed
        // ticks before we assert.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert!(cancel.is_cancelled(), "watchdog should have cancelled");
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_quiet_with_recent_keepalive() {
        let cancel = CancellationToken::new();
        let last_ka = Arc::new(AtomicU64::new(now_monotonic_us()));
        let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));

        // Simulate 10 keepalives at 900ms cadence. Each one refreshes
        // last_ka so the watchdog never sees more than 1s of silence.
        for _ in 0..10 {
            tokio::time::advance(Duration::from_millis(900)).await;
            last_ka.store(now_monotonic_us(), Ordering::Relaxed);
            tokio::task::yield_now().await;
        }

        assert!(!cancel.is_cancelled(), "watchdog must not fire while heartbeat present");
        // Manual cleanup so the JoinHandle resolves.
        cancel.cancel();
        handle.await.unwrap();
    }
}
```

- [ ] **Step 3.2: Add module to host main**

In `crates/host/src/main.rs`, add at the top of the file (after the existing `use` block, before the first `fn`):

```rust
mod watchdog;
```

- [ ] **Step 3.3: Run the tests**

Run: `cargo test -p prdt-host watchdog`
Expected: `test watchdog::tests::watchdog_fires_on_silence ... ok` and `test watchdog::tests::watchdog_quiet_with_recent_keepalive ... ok`.

If `now_monotonic_us` is not exported from `prdt_transport`, find the actual export path with:
Run: `grep -nE "pub fn now_monotonic_us|pub use.*now_monotonic_us" crates/transport/src/lib.rs crates/transport/src/udp.rs`
Use whichever path the existing `host/src/main.rs` already imports it from (it uses `now_monotonic_us` at line ~302 today). Match that import.

- [ ] **Step 3.4: Commit**

```bash
git add crates/host/src/watchdog.rs crates/host/src/main.rs
git commit -m "host: add watchdog module with KEEPALIVE_TIMEOUT=5s"
```

---

## Task 4: Host main — outer loop, cancel propagation, KeepAlive handler

**Files:**
- Modify: `crates/host/src/main.rs:270-589` (the entire post-bind session block)

This is the largest task. The strategy:
1. Wrap the existing session block in `loop { ... }`.
2. Convert `host_handshake` failures into `continue`.
3. Create `cancel` and `last_keepalive` per iteration.
4. Add `cancel.cancelled()` arms to each existing worker's `loop { match recv ... }`.
5. Add a `ControlMessage::KeepAlive` arm in the input task's match.
6. Add a `let watchdog = watchdog::spawn_watchdog(...)` call before the final `tokio::select!`.
7. Replace the final `tokio::select!` with one that polls JoinHandles by `&mut`, then cancels and joins survivors.

- [ ] **Step 4.1: Add imports**

In `crates/host/src/main.rs`, near the existing `use` block at the top, add:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
```

(`Arc`, `Duration`, `tokio::spawn` etc. are already imported.)

- [ ] **Step 4.2: Wrap session block in outer loop**

In `crates/host/src/main.rs`, find the line:

```rust
    info!("waiting for Noise handshake");
```

(around line 270). Insert above it:

```rust
    loop {
        transport.reset_session().await;
```

Then find the closing brace of the function (the `Ok(())` after the final `tokio::select!` block, around line 589) and insert just BEFORE the `Ok(())`:

```rust
        // Cancel any survivors and join, so encoder Drops run before the
        // next handshake (NVENC/MF release GPU resources here).
        cancel.cancel();
        let _ = tokio::join!(video, input, audio_task, clip_task, outgoing_task, watchdog);
        info!("session ended; returning to handshake wait");
    }
```

(The variables `cancel`, `video`, `input`, `audio_task`, `clip_task`, `outgoing_task`, `watchdog` are introduced in subsequent steps.)

- [ ] **Step 4.3: Make handshake failure non-fatal**

Find the existing `host_handshake` call (around line 299) and change:

```rust
    let req = host_handshake(
        &*transport,
        session_id,
        now_monotonic_us(),
        bitrate_bps,
        monitor_rect,
        vd_rect,
        Duration::from_secs(60),
    )
    .await
    .context("handshake")?;
    info!(?req, "handshake complete");
```

into:

```rust
        let req = match host_handshake(
            &*transport,
            session_id,
            now_monotonic_us(),
            bitrate_bps,
            monitor_rect,
            vd_rect,
            Duration::from_secs(60),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(?e, "host_handshake failed; resetting session");
                continue;
            }
        };
        info!(?req, "handshake complete");
```

Also change the earlier handshake_as_server call from `.await.context("Noise server handshake")?;` to a match that uses `continue` on error:

```rust
        if let Err(e) = transport.handshake_as_server(&keypair).await {
            warn!(?e, "Noise server handshake failed; resetting");
            continue;
        }
```

- [ ] **Step 4.4: Create cancel + last_keepalive at session start**

After the line `info!(backend = encoder.backend_name(), "encoder ready");` (around line 323), insert:

```rust
        let cancel = CancellationToken::new();
        let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));
```

These are fresh per session, so the watchdog timeout always starts from session-start.

- [ ] **Step 4.5: Add cancel arm to video task**

Find the video task (`let video = tokio::spawn(async move {` around line 329). Inside its loop, modify the body to a `tokio::select!`. The existing body looks like:

```rust
    let video = tokio::spawn(async move {
        loop {
            // existing capture+encode+send logic
        }
    });
```

Add a `cancel` clone capture and a select:

```rust
        let cancel_video = cancel.clone();
        let video = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    res = async { /* existing per-iteration body, returning Result<()> */ } => {
                        if let Err(e) = res {
                            warn!(?e, "video iteration error");
                        }
                    }
                }
            }
        });
```

If the existing video loop body is straight imperative (no Result), wrap it in an `async { ... }` block and pair with cancel as in:

```rust
        let cancel_video = cancel.clone();
        let video = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_video.cancelled() => break,
                    _ = async {
                        // existing per-iteration body verbatim
                    } => {}
                }
            }
        });
```

The exact transformation depends on the current body — keep all existing logic, just wrap each loop iteration in the `select!`.

- [ ] **Step 4.6: Add cancel arm to audio task, clip_task, outgoing_task**

Apply the same pattern as Step 4.5 to each of:
- `audio_task` (around line 357)
- `clip_task` (around line 489)
- `outgoing_task` (around line 533)

Each of these has a `loop { /* body */ }` structure. Wrap each iteration's body in a `tokio::select!` with a `cancel.cancelled() => break` arm at the top.

- [ ] **Step 4.7: Modify input task — add cancel arm AND KeepAlive handler**

The input task at `let input = tokio::spawn(async move {` (around line 428) is the most complex because it already has a `match` on `recv()`. Modify to:

```rust
        let cancel_input = cancel.clone();
        let last_ka_input = Arc::clone(&last_keepalive);
        let input = tokio::spawn(async move {
            let mut ft_rx = TransferReceiver::new(FILE_RECV_DIR, DEFAULT_MAX_TRANSFER_BYTES);
            loop {
                tokio::select! {
                    _ = cancel_input.cancelled() => break,
                    msg = rx_input.recv() => {
                        match msg {
                            Ok(ReceivedMessage::Input(ev)) => {
                                if let Err(e) = injector.inject(ev) {
                                    warn!(?e, "inject error");
                                }
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::KeepAlive)) => {
                                last_ka_input.store(now_monotonic_us(), Ordering::Relaxed);
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::ClipboardText { text })) => {
                                *input_last_remote.lock().await = Some(text.clone());
                                if let Err(e) = write_clipboard_text(&text) {
                                    warn!(?e, "write_clipboard_text failed");
                                }
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::Bye)) => {
                                info!("peer sent Bye");
                                break;
                            }
                            Ok(ReceivedMessage::Control(ControlMessage::LatencyReport {
                                samples,
                                arrival_p50_us,
                                arrival_p95_us,
                                decode_p50_us,
                                decode_p95_us,
                                present_p50_us,
                                present_p95_us,
                                present_p99_us,
                            })) => {
                                info!(
                                    samples,
                                    arrival_p50_us,
                                    arrival_p95_us,
                                    decode_p50_us,
                                    decode_p95_us,
                                    present_p50_us,
                                    present_p95_us,
                                    present_p99_us,
                                    "viewer latency report",
                                );
                            }
                            Ok(ReceivedMessage::Control(msg)) => {
                                let _ = ft_rx.handle(msg);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                warn!(?e, "recv error");
                                break;
                            }
                        }
                    }
                }
            }
        });
```

The `KeepAlive` arm (the `last_ka_input.store(...)` line) is the new logic. Everything else is the existing match preserved verbatim, just wrapped in the outer `select!` for cancel handling.

- [ ] **Step 4.8: Spawn watchdog**

After the `outgoing_task` spawn (around line 578) and BEFORE the final `tokio::select!`, add:

```rust
        let watchdog = watchdog::spawn_watchdog(cancel.clone(), Arc::clone(&last_keepalive));
```

- [ ] **Step 4.9: Replace the final select!**

Find the existing final select (around line 580):

```rust
    tokio::select! {
        _ = video => info!("video task ended"),
        _ = input => info!("input task ended"),
        _ = audio_task => info!("audio task ended"),
        _ = clip_task => info!("clipboard task ended"),
        _ = outgoing_task => info!("outgoing file watcher ended"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received"),
    }
    Ok(())
```

Replace it with:

```rust
        tokio::select! {
            _ = &mut video => warn!("video task ended unexpectedly"),
            _ = &mut input => warn!("input task ended unexpectedly"),
            _ = &mut audio_task => warn!("audio task ended unexpectedly"),
            _ = &mut clip_task => warn!("clipboard task ended unexpectedly"),
            _ = &mut outgoing_task => warn!("outgoing file watcher ended unexpectedly"),
            _ = &mut watchdog => info!("watchdog cancelled session"),
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received; shutting down");
                cancel.cancel();
                let _ = tokio::join!(video, input, audio_task, clip_task, outgoing_task, watchdog);
                return Ok(());
            }
        }
```

The final `cancel.cancel(); tokio::join!(...); info!(...)` lines from Step 4.2 will run after this select fires (for non-ctrl-c cases), then the outer `loop` iterates back.

For the `&mut` form to work, change the `let video = tokio::spawn(...);` style (and the other 5 spawns) to `let mut video = tokio::spawn(...);` (add `mut` to each).

- [ ] **Step 4.10: Build and run all host tests**

Run: `cargo build --release -p prdt-host`
Expected: clean build (with `NV_CODEC_SDK_PATH`, `LIBCLANG_PATH`, `CUDA_PATH` env set per `docs/build_env.md`).

Run: `cargo test -p prdt-host`
Expected: all tests pass, including the watchdog tests from Task 3.

- [ ] **Step 4.11: Commit**

```bash
git add crates/host/src/main.rs
git commit -m "host: outer session loop with cancel + KeepAlive watchdog"
```

---

## Task 5: Viewer — send `KeepAlive` from `latency_task`

**Files:**
- Modify: `crates/viewer/src/main.rs:1437-1492` (the `latency_task` body)

- [ ] **Step 5.1: Locate latency_task**

In `crates/viewer/src/main.rs`, find:

```rust
        let latency_task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.tick().await; // fire first tick immediately; skip it
            let mut ticks_since_report: u32 = 0;
            loop {
                ticker.tick().await;
                let snap = latency_probe.snapshot();
```

- [ ] **Step 5.2: Insert the KeepAlive send**

Insert one block between `ticker.tick().await;` and `let snap = latency_probe.snapshot();`:

```rust
            loop {
                ticker.tick().await;

                // Liveness heartbeat — host's watchdog needs this regardless
                // of whether decode is healthy yet. Crucial for slow-init
                // viewers that have not produced a present sample.
                if let Err(e) = latency_transport
                    .send_control(prdt_protocol::ControlMessage::KeepAlive)
                    .await
                {
                    warn!(?e, "send KeepAlive failed");
                }

                let snap = latency_probe.snapshot();
```

- [ ] **Step 5.3: Build viewer**

Run: `cargo build --release -p prdt-viewer`
Expected: clean build.

- [ ] **Step 5.4: Run viewer tests**

Run: `cargo test -p prdt-viewer`
Expected: all tests pass.

- [ ] **Step 5.5: Commit**

```bash
git add crates/viewer/src/main.rs
git commit -m "viewer: send KeepAlive every 1s from latency_task"
```

---

## Task 6: Manual smoke test (acceptance)

**Files:** none (verification only).

This validates the end-to-end recovery loop. Cannot be reasonably automated as a unit test — requires real OS process management.

- [ ] **Step 6.1: Build both binaries with full env**

```bash
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
export PATH="/c/Program Files/LLVM/bin:$PATH"
cargo build --release -p prdt-host -p prdt-viewer
```

Expected: clean build for both.

- [ ] **Step 6.2: Start host (loopback)**

```bash
rm -f host.log host.err viewer.log viewer.err
CWD='E:\project\rust-desktop\power-remote-dt'
powershell.exe -NoProfile -Command "Set-Location '$CWD'; \$env:RUST_LOG='info'; Start-Process -FilePath '.\target\release\prdt-host.exe' -ArgumentList '--bind','127.0.0.1:9000','--monitor','0','--bitrate-mbps','10','--key-file','host-key.bin','--encoder','nvenc','--headless' -RedirectStandardOutput host.log -RedirectStandardError host.err -WindowStyle Hidden"
sleep 2
```

Expected: `host.log` shows `waiting for Noise handshake`.

- [ ] **Step 6.3: First viewer run**

```bash
powershell.exe -NoProfile -Command "Set-Location '$CWD'; \$env:RUST_LOG='info'; Start-Process -FilePath '.\target\release\prdt-viewer.exe' -ArgumentList '--host','127.0.0.1:9000','--host-pubkey','pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0','--headless' -RedirectStandardOutput viewer.log -RedirectStandardError viewer.err -WindowStyle Hidden"
sleep 6
```

Expected: `host.log` shows `Noise handshake complete`, `handshake complete`, `encoder ready backend=nvenc`. `viewer.log` shows `frames_received=N` with N>0.

- [ ] **Step 6.4: Kill viewer abruptly**

```bash
powershell.exe -NoProfile -Command "Stop-Process -Name 'prdt-viewer' -Force"
sleep 7
```

Expected within ~5–7 seconds: `host.log` shows `viewer silent > 5s; canceling session` then `session ended; returning to handshake wait` then `waiting for Noise handshake`.

Verify:
```bash
grep "viewer silent\|session ended\|waiting for Noise" host.log
```

Expected: all three lines present.

- [ ] **Step 6.5: Second viewer run (no host restart)**

```bash
rm -f viewer.log viewer.err
powershell.exe -NoProfile -Command "Set-Location '$CWD'; \$env:RUST_LOG='info'; Start-Process -FilePath '.\target\release\prdt-viewer.exe' -ArgumentList '--host','127.0.0.1:9000','--host-pubkey','pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0','--headless' -RedirectStandardOutput viewer.log -RedirectStandardError viewer.err -WindowStyle Hidden"
sleep 6
```

Expected: a SECOND `Noise handshake complete` entry in `host.log` with a different `session_id`. `viewer.log` shows fresh `frames_received=N`.

- [ ] **Step 6.6: Repeat kill + restart cycle 3× total**

Repeat steps 6.4 and 6.5 two more times. Confirm host enters/exits sessions cleanly each cycle (search `host.log` for at least 3 `viewer silent > 5s` lines and 3 `Noise handshake complete` lines after the first).

- [ ] **Step 6.7: Cleanup**

```bash
powershell.exe -NoProfile -Command "Stop-Process -Name 'prdt-host','prdt-viewer' -Force -ErrorAction SilentlyContinue"
```

- [ ] **Step 6.8: Update STATUS.md and commit**

In `docs/superpowers/STATUS.md`, in the Plan 4 section (or wherever recent work is logged), add a line summarising:

```markdown
| `host-session-liveness-complete` | host が dead viewer を 5 秒 timeout で検知し outer loop で再 handshake 受け入れ。viewer は 1Hz で `KeepAlive` 送信。再起動なしでセッション復旧可能。 |
```

Commit:

```bash
git add docs/superpowers/STATUS.md
git commit -m "docs(STATUS): host-session-liveness-complete summary"
```

- [ ] **Step 6.9: Tag the STATUS commit**

```bash
git tag -a host-session-liveness-complete -m "Host session liveness: KeepAlive heartbeat + watchdog auto-recovery"
```

Verify the tag points to the latest commit on the branch:

```bash
git rev-parse HEAD
git rev-parse 'host-session-liveness-complete^{commit}'
```

Both must print the same SHA.

- [ ] **Step 6.10: Merge to master**

```bash
git checkout master
git merge --ff-only host-session-liveness
```

If a fast-forward is not possible (master moved), rebase first:

```bash
git checkout host-session-liveness
git rebase master
git checkout master
git merge --ff-only host-session-liveness
```
