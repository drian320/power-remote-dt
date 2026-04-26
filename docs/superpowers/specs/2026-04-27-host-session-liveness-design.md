# Host session liveness — design spec

**Date**: 2026-04-27
**Branch**: `host-session-liveness` (to be created)
**Status**: Design approved, plan to follow
**Related work**: builds on `mf-encoder-fallback-complete` (b7b8ca4) on master

## Problem

The host binary commits to a single viewer session for its lifetime once
the first packet arrives:

- `crates/transport/src/udp.rs:514-516`: `recv_raw_unencrypted` records
  the first sender's address into `peer: Mutex<Option<SocketAddr>>` and
  never resets it.
- `crates/transport/src/udp.rs:244`: `handshake_as_server` installs a
  `crypto: Mutex<Option<TransportSession>>` once and never tears it down.
- `crates/host/src/main.rs:270-329`: linear flow — `handshake_as_server`
  → `host_handshake` → `tokio::spawn` workers, each running an infinite
  inner loop. No liveness signal, no cancel path, no outer loop.

Symptom: when the viewer process dies (crash, manual kill, app close),
the host keeps encoding + sending video to the dead peer's `SocketAddr`.
A new viewer attempting to connect gets `HandshakeTimeout` because:

1. The new viewer's `NoiseE1` arrives but `peer` is already set, so
   `recv_raw_unencrypted` accepts the new packet but `handshake_as_server`
   has already returned. The encrypted main loop is running, which
   discards plaintext packets.
2. The host has no path back to the handshake state.

Recovery currently requires manual host restart. Observed multiple times
during 2-machine LAN testing on 2026-04-26.

## Goal

The host should detect a silent viewer within ~5 seconds, tear down the
session cleanly, and return to accepting a fresh handshake — without
needing process restart.

## Approach: heartbeat + watchdog + outer session loop

A new control message `KeepAlive` flows viewer→host every 1 second. The
host tracks the last receive time. A dedicated watchdog task fires a
`CancellationToken` if no `KeepAlive` arrives for 5 seconds. All worker
tasks observe the token via `tokio::select!` and exit. The outer loop
resets transport session state and waits for the next handshake.

### Cadence and timeout

- **Cadence**: 1 second (viewer-side `tokio::time::interval`).
- **Timeout**: 5 seconds. Up to 5 consecutive missed `KeepAlive`s tolerated
  — sufficient margin for transient UDP loss bursts.
- **Bandwidth**: `KeepAlive` is a 1-byte payload + 16-byte common header
  + AEAD tag = ~50 bytes/sec. Negligible.

## Architecture

### Current flow (linear, no recovery)

```
bind → handshake_as_server → host_handshake → spawn workers → workers loop forever
```

### New flow (outer loop with cancellation)

```
┌─ outer session loop ────────────────────────────────────────┐
│  reset_session()                                            │
│   ↓                                                         │
│  handshake_as_server  ← waits for viewer NoiseE1            │
│   ↓                                                         │
│  host_handshake (HelloRequest/HelloAck)                     │
│   ↓                                                         │
│  CancellationToken::new()                                   │
│  Arc<AtomicU64> last_keepalive = now_monotonic_us()         │
│   ↓                                                         │
│  spawn:                                                     │
│   - video / audio / input (each holds cancel_token)         │
│   - control_recv: updates last_keepalive on KeepAlive       │
│   - watchdog: 1s tick; cancel if now - last > 5s            │
│   ↓                                                         │
│  tokio::join!(workers...)  ← blocks until cancel propagates │
└─ loop back to top ──────────────────────────────────────────┘
```

## Wire protocol changes

### `ControlMessage::KeepAlive` — new variant

`crates/protocol/src/control.rs`:

```rust
pub enum ControlMessage {
    // ...existing variants
    KeepAlive,                    // empty variant, no fields
}
```

- Discriminator: next available 1-byte tag after `LatencyReport`.
- Encode: 1 byte (tag only).
- Decode: matching tag → `Ok((ControlMessage::KeepAlive, 1))`.
- `wire_size_max()` does not change (`KeepAlive` is the smallest variant).

`viewer_ts_us` is intentionally omitted: the host only needs *that something
arrived*, not when it was sent. Keeping the variant empty avoids over-loading
the message with latency-measurement semantics that the existing
`Ping`/`Pong`/`LatencyReport` already cover.

## Component changes

### Transport (`crates/transport/src/udp.rs`)

Add one method:

```rust
impl CustomUdpTransport {
    /// Reset session state so the next `handshake_as_server` accepts a
    /// fresh peer. Called by host outer loop after a viewer disconnects
    /// or times out.
    pub async fn reset_session(&self) {
        *self.peer.lock().await = None;
        *self.crypto.lock().await = None;
    }
}
```

The existing `recv_raw_unencrypted` line 514 (`if p.is_none() { *p =
Some(from); }`) keeps working: after `reset_session`, `p.is_none()` so
the next sender's address is recorded.

No other transport changes.

### Viewer (`crates/viewer/src/main.rs`)

In the existing `latency_task` (line ~1437), add `KeepAlive` send at the
top of each tick, before the snapshot/log/LatencyReport logic:

```rust
loop {
    ticker.tick().await;

    // Liveness heartbeat — host's watchdog needs this regardless of
    // whether decode is healthy (crucial for slow-init viewers).
    if let Err(e) = latency_transport
        .send_control(ControlMessage::KeepAlive)
        .await
    {
        warn!(?e, "send KeepAlive failed");
    }

    // ...existing snapshot + log + LatencyReport logic unchanged
}
```

The existing `LatencyReport`-every-5-ticks-with-present-data path is
preserved. `KeepAlive` is unconditional.

### Host (`crates/host/src/main.rs`)

Wrap the post-bind logic in an outer loop. Convert the existing direct
`spawn` calls into helper functions that accept a `CancellationToken`,
and add a watchdog spawn helper.

```rust
// (existing bind/transport setup unchanged)
loop {
    transport.reset_session().await;

    info!("waiting for Noise handshake");
    if let Err(e) = transport.handshake_as_server(&keypair).await {
        warn!(?e, "Noise server handshake failed; resetting");
        continue;
    }
    info!("Noise handshake complete");

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

    let cancel = CancellationToken::new();
    let last_keepalive = Arc::new(AtomicU64::new(now_monotonic_us()));

    let video    = spawn_video    (..., cancel.clone());
    let audio    = spawn_audio    (..., cancel.clone());
    let input    = spawn_input    (..., cancel.clone());
    let control  = spawn_control  (..., cancel.clone(), Arc::clone(&last_keepalive));
    let watchdog = spawn_watchdog (cancel.clone(), Arc::clone(&last_keepalive));

    let _ = tokio::join!(video, audio, input, control, watchdog);

    info!("session ended; returning to handshake wait");
}
```

Each existing worker (`video`, `audio`, `input`, `control`) gets a
`select!` arm:

```rust
loop {
    tokio::select! {
        _ = cancel.cancelled() => break,
        // existing select arms (recv, capture tick, etc.)
    }
}
```

Watchdog (new):

```rust
fn spawn_watchdog(
    cancel: CancellationToken,
    last_keepalive: Arc<AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let last = last_keepalive.load(Ordering::Relaxed);
                    let now = now_monotonic_us();
                    if now.saturating_sub(last) > 5_000_000 {
                        warn!(silence_us = now - last, "viewer silent > 5s; canceling session");
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    })
}
```

Control task (modified): on `ControlMessage::KeepAlive`, update
`last_keepalive`:

```rust
ReceivedMessage::Control(ControlMessage::KeepAlive) => {
    last_keepalive.store(now_monotonic_us(), Ordering::Relaxed);
}
```

`Cargo.toml` (host crate): ensure `tokio-util` is a dependency with the
`rt` feature for `CancellationToken`. If not already present in the
workspace, add it.

## Failure modes

| Scenario | Behavior |
|---|---|
| Viewer process dies | KeepAlive stops arriving → 5s later watchdog cancels → workers exit → outer loop resets and waits for next NoiseE1 |
| Network blip 1-3s | Within timeout window — watchdog does not fire, KeepAlive resumes |
| Slow-init viewer (`--encoder mf`, etc.) | `latency_task` starts at handshake completion, sends first KeepAlive within ~1s, well under 5s timeout |
| `host_handshake` Hello timeout | `Err` → log + `continue` → outer loop returns to handshake wait |
| `handshake_as_server` waits forever | Intended — host is idle, waiting for any new viewer |
| Old viewer's packet arrives after reset | `peer = None`, packet's source becomes new peer. If `session_id` mismatches, `recv_raw_unencrypted` warns and drops. New NoiseE1 starts fresh handshake |
| Encoder `Drop` not running | `CancellationToken` causes `select!` to break gracefully → `JoinHandle` drop → struct `Drop` runs → NVENC/MF resources released |
| Multiple viewers race on NoiseE1 | First-to-arrive wins (existing behavior) |

### Out of scope

- **Viewer-side host liveness**: viewer already shows recv-timeout warnings
  every 1s when host stops sending. No new mechanism here.
- **Multiple concurrent viewers**: still single-session.
- **Graceful `Bye` message**: would let viewer signal disconnect for instant
  recovery instead of waiting 5s. Future work.

## Testing

### Unit tests

1. **`crates/protocol/src/control.rs`** — `keep_alive_wire_roundtrip`:
   ```rust
   #[test]
   fn keep_alive_wire_roundtrip() {
       let msg = ControlMessage::KeepAlive;
       let mut buf = Vec::new();
       msg.encode(&mut buf);
       let (decoded, len) = ControlMessage::decode(&buf).unwrap();
       assert_eq!(decoded, ControlMessage::KeepAlive);
       assert_eq!(len, buf.len());
   }
   ```

2. **`crates/host/src/watchdog.rs`** (new module) — fires on silence:
   ```rust
   #[tokio::test(start_paused = true)]
   async fn watchdog_fires_on_silence() {
       let cancel = CancellationToken::new();
       let last_ka = Arc::new(AtomicU64::new(now_monotonic_us()));
       let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));
       tokio::time::advance(Duration::from_secs(6)).await;
       assert!(cancel.is_cancelled());
       handle.await.unwrap();
   }
   ```

3. **`crates/host/src/watchdog.rs`** — does not fire while heartbeat present:
   ```rust
   #[tokio::test(start_paused = true)]
   async fn watchdog_quiet_with_recent_keepalive() {
       let cancel = CancellationToken::new();
       let last_ka = Arc::new(AtomicU64::new(now_monotonic_us()));
       let handle = spawn_watchdog(cancel.clone(), Arc::clone(&last_ka));
       for _ in 0..10 {
           tokio::time::advance(Duration::from_millis(900)).await;
           last_ka.store(now_monotonic_us(), Ordering::Relaxed);
       }
       assert!(!cancel.is_cancelled());
       cancel.cancel();
       handle.await.unwrap();
   }
   ```

### Manual smoke (acceptance criteria)

Loopback host + viewer setup:

1. Start host, start viewer, confirm video flows.
2. Kill viewer process (Stop-Process or window close).
3. Within ~5s, host log shows `viewer silent > 5s; canceling session`,
   then `session ended; returning to handshake wait`, then `waiting for
   Noise handshake`.
4. Restart viewer with the same host pubkey; new handshake completes
   within ~1s, frames flow.
5. Repeat steps 2–4 **at least 3 times** without restarting host.

## Acceptance summary

- 0 protocol breaking changes for existing message types.
- 1 new `ControlMessage::KeepAlive` variant.
- 1 new method on `CustomUdpTransport` (`reset_session`).
- 1 line added to viewer's existing `latency_task`.
- Host main.rs wrapped in outer loop, workers gain cancel arm.
- 1 new watchdog task (~30 lines).
- 3 new unit tests.
- 1 manual smoke test scenario.

Estimated change size: ~120-150 lines net additions across 4 crates.

## Tag (after implementation)

`host-session-liveness-complete` after merge to master.
