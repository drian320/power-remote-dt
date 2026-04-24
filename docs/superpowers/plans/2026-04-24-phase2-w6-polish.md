# Phase 2 W6 Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** W6 実機 2台 LAN 検証で顕在化した 2 つの UX 摩擦(probe 単発 drop・host bind 手動指定)を解消し、viewer に生えた `discover_outbound_ip` を `signaling-client` へ共通化する。

**Architecture:** (1) `signaling-client` に `discover_outbound_ip` を追加して viewer/host から再利用、(2) host bin で `args.bind.ip().is_unspecified()` 時に auto-detect、(3) `CustomUdpTransport::probe_and_commit_peer` に `tokio::select!` ベースの 200ms × 5 回の Probe 再送ループを追加。

**Tech Stack:** Rust 2021, tokio 1.40 (`net`/`time`/`macros`/`sync`), url 2.x, prdt_protocol (ControlMessage/PacketHeader), rand_core (OsRng)。テストは `tokio::test(flavor = "multi_thread")`。

**Spec:** `docs/superpowers/specs/2026-04-24-phase2-w6-polish-design.md`

---

## File Structure

**Created files:**
- `crates/signaling-client/src/net.rs` — `discover_outbound_ip` + unit tests
- `crates/transport/tests/probe_retry.rs` — integration test for probe retry

**Modified files:**
- `crates/signaling-client/src/lib.rs` — add `pub mod net;` + `pub use net::discover_outbound_ip;`
- `crates/viewer/src/main.rs` — delete inline `discover_outbound_ip`, import from `prdt_signaling_client`
- `crates/host/src/main.rs` — auto-detect when `--bind` IP is unspecified
- `crates/transport/src/udp.rs` — `probe_and_commit_peer` gets retry loop + pub consts

**Public API surface added:**
- `prdt_signaling_client::discover_outbound_ip(&url::Url) -> io::Result<IpAddr>`
- `prdt_transport::PROBE_RETRY_INTERVAL: Duration`
- `prdt_transport::PROBE_RETRY_COUNT: u32`

---

## Task 1: Extract `discover_outbound_ip` to signaling-client

**Files:**
- Create: `crates/signaling-client/src/net.rs`
- Modify: `crates/signaling-client/src/lib.rs:1-10`

- [ ] **Step 1: Create net.rs with the function + two unit tests**

Write `crates/signaling-client/src/net.rs`:

```rust
//! Outbound-interface discovery helper.
//!
//! Opens a temp UDP socket, `connect`s it (no packets sent) to the signaling
//! server's resolved addr, and reads `local_addr`. The kernel picks the
//! outbound route, so the returned IP is the one the OS will actually route
//! from — useful for announcing a Host candidate on the correct LAN interface
//! instead of `0.0.0.0` or `127.0.0.1`.

use std::io;
use std::net::IpAddr;

/// Discover the local IP the OS would route outbound traffic over toward the
/// signaling server at `url`. The URL scheme is ignored; only `host_str()`
/// and `port()` are consulted (port defaults to 80 when absent).
pub async fn discover_outbound_ip(url: &url::Url) -> io::Result<IpAddr> {
    let host = url
        .host_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing host"))?;
    let port = url.port().unwrap_or(80);
    let resolved = tokio::net::lookup_host(format!("{host}:{port}"))
        .await?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no addr"))?;
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    probe.connect(resolved).await?;
    Ok(probe.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_loopback_target_to_loopback_local() {
        let url = url::Url::parse("ws://127.0.0.1:8080/signal").unwrap();
        let ip = discover_outbound_ip(&url).await.unwrap();
        assert!(ip.is_loopback(), "expected loopback, got {ip}");
    }

    #[tokio::test]
    async fn rejects_url_without_host() {
        let url = url::Url::parse("file:///tmp/x").unwrap();
        let err = discover_outbound_ip(&url).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p prdt-signaling-client --lib net::`
Expected: FAIL (module `net` not declared in `lib.rs`)

- [ ] **Step 3: Export the module from `lib.rs`**

Replace `crates/signaling-client/src/lib.rs` contents with:

```rust
//! WebSocket client for the power-remote-dt signaling rendezvous.

mod config;
mod error;
mod net;
mod rendezvous;

pub use config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
pub use error::SignalingError;
pub use net::discover_outbound_ip;
pub use rendezvous::{rendezvous_as_host, rendezvous_as_viewer};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p prdt-signaling-client --lib net::`
Expected: `test net::tests::resolves_loopback_target_to_loopback_local ... ok` and `test net::tests::rejects_url_without_host ... ok`

- [ ] **Step 5: Commit**

```bash
cd E:/project/rust-desktop/power-remote-dt
git add crates/signaling-client/src/net.rs crates/signaling-client/src/lib.rs
git commit -m "signaling-client: add discover_outbound_ip helper (shared by host+viewer)"
```

---

## Task 2: Migrate viewer to use shared `discover_outbound_ip`

**Files:**
- Modify: `crates/viewer/src/main.rs:127-146` (delete function), wire in import

- [ ] **Step 1: Confirm baseline build + tests pass before changes**

Run: `cargo build -p prdt-viewer`
Expected: `Compiling ... Finished` (no errors)

Run: `cargo test -p prdt-signaling-client --test w1_smoke`
Expected: smoke test passes (should complete in well under 5 seconds)

- [ ] **Step 2: Delete the inline function and import from signaling-client**

Apply two edits to `crates/viewer/src/main.rs`.

Edit A — remove the inline function (lines 127-146). Delete:

```rust
/// Discover the local IP the OS would route outbound traffic over toward the
/// signaling server. Opens a temp UDP socket, `connect`s it (no packets sent)
/// to force the kernel to pick the outbound interface, then reads `local_addr`.
///
/// This avoids hard-coding `127.0.0.1` or relying on a user-supplied `--bind`
/// for cross-LAN viewer deployments where the viewer has no reason to know
/// its own LAN IP ahead of time.
async fn discover_outbound_ip(signaling_url: &url::Url) -> std::io::Result<std::net::IpAddr> {
    let host = signaling_url
        .host_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing host"))?;
    let port = signaling_url.port().unwrap_or(80);
    let resolved = tokio::net::lookup_host(format!("{host}:{port}"))
        .await?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addr"))?;
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    probe.connect(resolved).await?;
    Ok(probe.local_addr()?.ip())
}
```

Edit B — change the call site at line ~720. Find:

```rust
                match discover_outbound_ip(url).await {
```

and replace with:

```rust
                match prdt_signaling_client::discover_outbound_ip(url).await {
```

- [ ] **Step 3: Build viewer to confirm the edit compiles**

Run: `cargo build -p prdt-viewer`
Expected: `Finished` (no warnings about unused `discover_outbound_ip`, no unresolved imports)

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test --workspace --exclude prdt-transport -- --skip wgpu`
Expected: all tests pass. (`--exclude prdt-transport` is temporary; transport retry test is added in Task 3. The `--skip wgpu` avoids GPU-dependent tests that may fail in headless setups.)

If the headless host has no wgpu, this is acceptable — re-run `cargo test -p prdt-signaling-client` and `cargo test -p prdt-signaling-server` at minimum.

- [ ] **Step 5: Commit**

```bash
git add crates/viewer/src/main.rs
git commit -m "viewer: reuse prdt_signaling_client::discover_outbound_ip"
```

---

## Task 3: Probe retry — integration test first, then implementation

**Files:**
- Create: `crates/transport/tests/probe_retry.rs`
- Modify: `crates/transport/src/udp.rs:244-313` (rewrite `probe_and_commit_peer` body)
- Modify: `crates/transport/src/udp.rs:66` area (add two pub consts)
- Modify: `crates/transport/src/lib.rs:22-24` (re-export consts)

- [ ] **Step 1: Write the failing integration test**

Write `crates/transport/tests/probe_retry.rs`:

```rust
//! Verifies `CustomUdpTransport::probe_and_commit_peer` resends Probe packets
//! so a transient drop of the first N packets (typical of stateful firewalls
//! admitting the first outbound but dropping inbound until state is tracked)
//! is masked within the configured retry budget.

use prdt_protocol::control::ControlMessage;
use prdt_protocol::wire::{PacketHeader, PacketType, HEADER_LEN};
use prdt_transport::{
    CustomUdpTransport, UdpTransportConfig, PROBE_RETRY_COUNT, PROBE_RETRY_INTERVAL,
};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_retry_survives_first_packet_drops() {
    // Simulate a firewall that drops the first `DROP_COUNT` Probes, accepts
    // the (DROP_COUNT+1)th, and only then emits the ProbeAck. The client
    // should retry enough times to get past this.
    const DROP_COUNT: u32 = 2;
    assert!(
        DROP_COUNT < PROBE_RETRY_COUNT,
        "DROP_COUNT must be strictly less than PROBE_RETRY_COUNT"
    );

    let client = Arc::new(
        CustomUdpTransport::bind(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            UdpTransportConfig::default(),
        )
        .await
        .unwrap(),
    );

    let server = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server.local_addr().unwrap();

    let probe_count = Arc::new(AtomicU32::new(0));
    let probe_count_bg = Arc::clone(&probe_count);
    let server_bg = Arc::clone(&server);

    let server_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = match server_bg.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            let hdr = match PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(_) => continue,
            };
            if hdr.packet_type != PacketType::Control {
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                continue;
            }
            let msg = match prdt_protocol::decode_control(&buf[HEADER_LEN..body_end]) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let ControlMessage::Probe { nonce } = msg {
                let count = probe_count_bg.fetch_add(1, Ordering::SeqCst) + 1;
                if count > DROP_COUNT {
                    let ack = ControlMessage::ProbeAck { nonce };
                    let body = prdt_protocol::encode_control(&ack).unwrap();
                    let ack_hdr = PacketHeader {
                        packet_type: PacketType::Control,
                        flags: 0,
                        session_id: 0,
                        payload_len: body.len() as u32,
                    };
                    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
                    out.extend_from_slice(&ack_hdr.encode());
                    out.extend_from_slice(&body);
                    let _ = server_bg.send_to(&out, from).await;
                }
            }
        }
    });

    let start = std::time::Instant::now();
    let winner = client
        .probe_and_commit_peer(&[server_addr], Duration::from_secs(10))
        .await
        .expect("probe_and_commit_peer should succeed once retries land");
    let elapsed = start.elapsed();

    assert_eq!(winner, server_addr);
    let observed = probe_count.load(Ordering::SeqCst);
    assert!(
        observed > DROP_COUNT,
        "server should have observed at least {} probes, saw {observed}",
        DROP_COUNT + 1
    );
    // (DROP_COUNT+1) successful probe = initial + DROP_COUNT retries.
    // Allow 2× slack over the nominal DROP_COUNT × interval for CI timing.
    let generous = PROBE_RETRY_INTERVAL * (DROP_COUNT * 2 + 2);
    assert!(
        elapsed < generous,
        "probe_and_commit_peer took {elapsed:?}, expected < {generous:?} with retry",
    );
    server_task.abort();
}
```

- [ ] **Step 2: Run the test — it must fail**

Run: `cargo test -p prdt-transport --test probe_retry probe_retry_survives_first_packet_drops`

Expected: FAIL with either a compile error (`PROBE_RETRY_COUNT not found in prdt_transport`) — this is the intended first failure. If it compiles unexpectedly, it should still fail at runtime because the current `probe_and_commit_peer` only sends a single Probe and the 2 drops would push the call to the 10-second timeout. Either failure mode is acceptable; we fix both by implementing.

- [ ] **Step 3: Add pub consts + re-export**

Apply two edits:

Edit A — `crates/transport/src/udp.rs`, immediately after the existing `DEFAULT_HANDSHAKE_TIMEOUT` constant (around line 66), add:

```rust
/// Interval between Probe retransmissions inside `probe_and_commit_peer`.
/// 200ms is short enough for LAN RTT (typically <5ms) plus firewall
/// connection-tracking install, yet long enough to avoid flooding. Exposed
/// for integration tests.
pub const PROBE_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Total number of Probe transmissions per candidate (initial + retries).
/// With `PROBE_RETRY_INTERVAL = 200ms`, `PROBE_RETRY_COUNT = 5` means probes
/// are sent over the first 800ms of the overall timeout; the remaining
/// budget stays passive (only receiving). Exposed for integration tests.
pub const PROBE_RETRY_COUNT: u32 = 5;
```

Edit B — `crates/transport/src/lib.rs`, extend the `pub use udp::` line to include the new consts:

```rust
pub use udp::{
    now_monotonic_us, CustomUdpTransport, UdpTransportConfig, DEFAULT_HANDSHAKE_TIMEOUT,
    PROBE_RETRY_COUNT, PROBE_RETRY_INTERVAL,
};
```

- [ ] **Step 4: Rewrite `probe_and_commit_peer` body with retry loop**

In `crates/transport/src/udp.rs`, replace the existing function body (lines 244-313) with the retry-aware version.

Find:

```rust
    pub async fn probe_and_commit_peer(
        &self,
        candidates: &[SocketAddr],
        timeout_duration: std::time::Duration,
    ) -> Result<SocketAddr, TransportError> {
        use rand_core::{OsRng, RngCore};
        use std::collections::HashSet;

        let mut pending: HashSet<[u8; 16]> = HashSet::new();
        for &addr in candidates {
            let mut nonce = [0u8; 16];
            OsRng.fill_bytes(&mut nonce);
            pending.insert(nonce);
            if let Err(e) = self.send_control_to(ControlMessage::Probe { nonce }, addr).await {
                tracing::warn!(?addr, error = ?e, "probe send failed; skipping candidate");
            }
        }

        let deadline = tokio::time::Instant::now() + timeout_duration;
        let mut buf = vec![0u8; 4096];
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::HandshakeTimeout);
            }
            let (n, from) =
                match tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => return Err(TransportError::Io(e)),
                    Err(_) => return Err(TransportError::HandshakeTimeout),
                };

            let hdr = match PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(_) => continue,
            };
            if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                continue;
            }
            if hdr.packet_type != PacketType::Control {
                continue;
            }
            if hdr.flags & prdt_protocol::packet_flags::ENCRYPTED != 0 {
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                continue;
            }
            let msg = match prdt_protocol::decode_control(&buf[HEADER_LEN..body_end]) {
                Ok(m) => m,
                Err(_) => continue,
            };
            match msg {
                ControlMessage::Probe { nonce } => {
                    let _ = self
                        .send_control_to(ControlMessage::ProbeAck { nonce }, from)
                        .await;
                }
                ControlMessage::ProbeAck { nonce } => {
                    if pending.contains(&nonce) {
                        self.configure_peer(from).await;
                        tracing::info!(peer = ?from, "probe winner");
                        return Ok(from);
                    }
                }
                _ => continue,
            }
        }
    }
```

Replace the entire function with:

```rust
    pub async fn probe_and_commit_peer(
        &self,
        candidates: &[SocketAddr],
        timeout_duration: std::time::Duration,
    ) -> Result<SocketAddr, TransportError> {
        use rand_core::{OsRng, RngCore};
        use std::collections::HashMap;

        // nonce → addr map. Nonces stay in the map until success (we return
        // on the first matching ProbeAck) so periodic retries know where to
        // resend. `candidates` being empty is tolerated: the loop below just
        // waits for the overall timeout.
        let mut pending: HashMap<[u8; 16], SocketAddr> = HashMap::new();
        for &addr in candidates {
            let mut nonce = [0u8; 16];
            OsRng.fill_bytes(&mut nonce);
            pending.insert(nonce, addr);
            if let Err(e) = self.send_control_to(ControlMessage::Probe { nonce }, addr).await {
                tracing::warn!(?addr, error = ?e, "probe send failed; skipping candidate");
            }
        }

        let deadline = tokio::time::Instant::now() + timeout_duration;
        // Start the retry ticker one interval out so its first tick fires at
        // t=PROBE_RETRY_INTERVAL (not immediately, which would double-send).
        let mut retry_ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + PROBE_RETRY_INTERVAL,
            PROBE_RETRY_INTERVAL,
        );
        let mut sends_done: u32 = 1;
        let mut buf = vec![0u8; 4096];

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::HandshakeTimeout);
            }

            tokio::select! {
                biased;
                recv = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)) => {
                    let (n, from) = match recv {
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) => return Err(TransportError::Io(e)),
                        Err(_) => return Err(TransportError::HandshakeTimeout),
                    };
                    let hdr = match PacketHeader::decode(&buf[..n]) {
                        Ok(h) => h,
                        Err(_) => continue,
                    };
                    if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                        continue;
                    }
                    if hdr.packet_type != PacketType::Control {
                        continue;
                    }
                    if hdr.flags & prdt_protocol::packet_flags::ENCRYPTED != 0 {
                        continue;
                    }
                    let body_end = HEADER_LEN + hdr.payload_len as usize;
                    if body_end > n {
                        continue;
                    }
                    let msg = match prdt_protocol::decode_control(&buf[HEADER_LEN..body_end]) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    match msg {
                        ControlMessage::Probe { nonce } => {
                            let _ = self
                                .send_control_to(ControlMessage::ProbeAck { nonce }, from)
                                .await;
                        }
                        ControlMessage::ProbeAck { nonce } => {
                            if pending.contains_key(&nonce) {
                                self.configure_peer(from).await;
                                tracing::info!(peer = ?from, "probe winner");
                                return Ok(from);
                            }
                        }
                        _ => continue,
                    }
                }
                _ = retry_ticker.tick(), if sends_done < PROBE_RETRY_COUNT && !pending.is_empty() => {
                    sends_done += 1;
                    tracing::trace!(attempt = sends_done, pending = pending.len(), "probe retry");
                    // Copy to avoid holding borrow across await.
                    let snapshot: Vec<(_, _)> = pending
                        .iter()
                        .map(|(nonce, addr)| (*nonce, *addr))
                        .collect();
                    for (nonce, addr) in snapshot {
                        if let Err(e) = self.send_control_to(ControlMessage::Probe { nonce }, addr).await {
                            tracing::warn!(?addr, error = ?e, "probe retry send failed");
                        }
                    }
                }
            }
        }
    }
```

Key behavioral differences vs. the old body:
- `pending` is now `HashMap<nonce, addr>` so retries can resend to the correct destination.
- `tokio::select!` drives recv and the retry ticker concurrently.
- `biased;` prefers handling an already-arrived ProbeAck before firing a retry (avoids needless extra send when an ack is queued).
- Retry branch is guarded by `sends_done < PROBE_RETRY_COUNT && !pending.is_empty()`, so retries stop after 5 sends total or when there's nothing left to probe.
- The `interval_at(now + interval, interval)` start offset avoids the classic "first tick fires immediately" pitfall.

- [ ] **Step 5: Run the retry test — it must pass**

Run: `cargo test -p prdt-transport --test probe_retry probe_retry_survives_first_packet_drops -- --nocapture`

Expected: PASS. Log (from `--nocapture`) should show `probe retry attempt=2 pending=1` and `probe retry attempt=3 pending=1` trace lines if you enable trace logs; with default log level just the PASS line appears.

- [ ] **Step 6: Run the full transport test suite — existing tests still pass**

Run: `cargo test -p prdt-transport`

Expected: all tests pass (probe_test.rs, loopback_test.rs, udp_test.rs, encrypted_test.rs, probe_retry.rs). No regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/transport/src/udp.rs crates/transport/src/lib.rs crates/transport/tests/probe_retry.rs
git commit -m "transport: retry Probe 5x@200ms in probe_and_commit_peer"
```

---

## Task 4: Host bin auto-detect

**Files:**
- Modify: `crates/host/src/main.rs:152-169` (bind preparation block)

- [ ] **Step 1: Insert auto-detect block before the transport bind**

In `crates/host/src/main.rs`, find the block:

```rust
    // Bind UDP first; wait for viewer to say Hello.
    let cfg = UdpTransportConfig {
        session_id: 0, // client picks
        ..Default::default()
    };
    let transport = Arc::new(if let Some(url) = args.turn_url.clone() {
        let turn_cfg = prdt_nat_traversal::TurnConfig::from_url(&url)
            .await
            .context("parse turn URL")?;
        CustomUdpTransport::bind_with_relay(args.bind, cfg, turn_cfg)
            .await
            .context("UDP bind with TURN relay")?
    } else {
        CustomUdpTransport::bind(args.bind, cfg)
            .await
            .context("UDP bind")?
    });
```

Replace it with:

```rust
    // Bind UDP first; wait for viewer to say Hello.
    let cfg = UdpTransportConfig {
        session_id: 0, // client picks
        ..Default::default()
    };

    // If --bind's IP is wildcard (0.0.0.0 or ::) and we're in signaling mode,
    // auto-detect the outbound interface the kernel would use to reach the
    // signaling server. This avoids the operator having to hand the host its
    // LAN IP explicitly. Direct mode has no URL to probe, so we keep the
    // user-supplied wildcard (the transport binds to all interfaces, which
    // is fine for server-side listen, but the Host candidate we emit won't
    // be used in direct mode anyway).
    let effective_bind = if args.bind.ip().is_unspecified() {
        if let Some(url) = args.signaling_url.as_ref() {
            match prdt_signaling_client::discover_outbound_ip(url).await {
                Ok(ip) => {
                    let new_bind = SocketAddr::new(ip, args.bind.port());
                    info!(%args.bind, %new_bind, "host auto-detected LAN bind IP via signaling URL");
                    new_bind
                }
                Err(e) => {
                    tracing::warn!(error = %e, "outbound IP discovery failed; keeping wildcard bind (Host candidate may be unroutable)");
                    args.bind
                }
            }
        } else {
            args.bind
        }
    } else {
        args.bind
    };

    let transport = Arc::new(if let Some(url) = args.turn_url.clone() {
        let turn_cfg = prdt_nat_traversal::TurnConfig::from_url(&url)
            .await
            .context("parse turn URL")?;
        CustomUdpTransport::bind_with_relay(effective_bind, cfg, turn_cfg)
            .await
            .context("UDP bind with TURN relay")?
    } else {
        CustomUdpTransport::bind(effective_bind, cfg)
            .await
            .context("UDP bind")?
    });
```

Notes:
- `info!(%args.bind, %new_bind, ...)` uses tracing's field syntax; `info!` is already imported via `use tracing::info;` (check top of file). If not imported, add `use tracing::info;` or prefix with `tracing::info!(...)`.
- The wildcard→direct-mode behavior is unchanged from current (just keeps `args.bind`), so direct-mode smoke tests won't regress.

Verify `info!` import with:

Run: `grep -n "use tracing" crates/host/src/main.rs`

If the output doesn't include `info`, change the `info!(...)` call to `tracing::info!(...)` in the snippet above.

- [ ] **Step 2: Verify the host binary still builds**

Run: `cargo build -p prdt-host`
Expected: `Finished` (no warnings about unused imports, no type errors)

- [ ] **Step 3: Run workspace tests — no regressions**

Run: `cargo test -p prdt-signaling-client`
Expected: all smoke tests (`w1_smoke`, `w5_smoke`, etc.) still pass — the host bin lives in a separate crate, but if they share code paths we want to catch any breakage early.

Run: `cargo test -p prdt-transport`
Expected: all tests including `probe_retry` still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/host/src/main.rs
git commit -m "host: auto-detect LAN bind IP when --bind is wildcard (signaling mode)"
```

---

## Task 5: Final validation + tag

**Files:** no source changes, verification only.

- [ ] **Step 1: Clippy clean across the workspace**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tee /tmp/clippy.log`

Expected: `Finished` with no warning-promoted-to-error. If any clippy lint fires on the new code, fix it inline (most common offenders: `&Vec<T>` → `&[T]`, unused `async`, redundant clones).

If you need to re-run after a fix, commit the fix with:

```bash
git add -u
git commit -m "w6-polish: clippy fix — <specific lint>"
```

- [ ] **Step 2: Format clean**

Run: `cargo fmt --all --check`

Expected: exit code 0 (no output). If there's formatting drift, run `cargo fmt --all` and commit with `w6-polish: cargo fmt`.

- [ ] **Step 3: Full workspace test one more time**

Run: `cargo test --workspace`

Expected: all tests pass. Take particular note of:
- `prdt-signaling-client w1_smoke, w3_smoke, w4_smoke, w5_smoke` (these prove rendezvous still works)
- `prdt-transport probe_test, probe_retry` (these prove both happy-path and retry behavior)

If any pre-existing test fails here AND it wasn't failing before the branch started (check `git stash; cargo test --workspace; git stash pop`), investigate before tagging.

- [ ] **Step 4: Tag the completion state**

```bash
git tag -a phase2-w6-polish-complete -m "Phase 2 W6 polish complete — probe retry 5x@200ms + host auto-detect + signaling-client::discover_outbound_ip"
git tag | grep phase2
```

Expected: output includes `phase2-w6-polish-complete` alongside w1..w6.

- [ ] **Step 5: Summarize completion**

Report back to the user:

> W6 polish 完了:
> - signaling-client に `discover_outbound_ip` 共通化
> - viewer は共通関数を使用
> - host bin は `--bind 0.0.0.0:9000` で LAN IP を自動検出
> - `probe_and_commit_peer` が 200ms × 5 回 Probe 再送
> - タグ `phase2-w6-polish-complete` 打刻済み
>
> 実機 2台検証は手動で別途。初回接続で「1秒以内に成功」を観測できればファイアウォール drop shadow が消えたと判断可能。

---

## Risks & Notes for Implementer

- **`tokio::select!` + `biased;`** — The `biased;` directive makes the macro poll branches in source order rather than randomly. This ensures recv is checked before the retry ticker; without it, a retry can fire even when a ProbeAck is already queued. Keep the recv branch first.
- **`interval_at` start offset** — `tokio::time::interval(d)` emits the first tick immediately on first poll; using `interval_at(now + d, d)` delays it by one period. Without this, the retry ticker would fire at t=0 in addition to the explicit initial send loop, double-sending every initial probe.
- **HashMap vs HashSet for pending** — The old impl used `HashSet<nonce>`, sufficient for ack matching. Retries need `nonce → addr`, hence the upgrade to `HashMap`.
- **Snapshotting pending before resend** — `pending.iter()` borrows the map; `send_control_to().await` may yield; keeping the borrow across yield points is an anti-pattern and in some cases a compile error under the borrow checker. The `snapshot: Vec<_>` clone is cheap (≤ a few tuples) and avoids the issue.
- **Existing probe_test.rs tests** — These use `Duration::from_secs(3)` overall timeout. With retries firing every 200ms, they stay well under 3s (initial probes succeed immediately on loopback). No test changes expected.
- **Viewer signature churn** — The call site in `viewer/src/main.rs` already uses a local `discover_outbound_ip`. The replacement is a single qualified path; there are no other call sites to update.

---

## Self-Review Notes

- Spec `In-scope` items are all covered by Tasks 1–5.
- No `TBD`/`TODO` placeholders in the plan; every code block is complete and copy-pasteable.
- Type consistency: `discover_outbound_ip` returns `io::Result<IpAddr>` (spec + Task 1 + Task 2 + Task 4); `PROBE_RETRY_INTERVAL: Duration` / `PROBE_RETRY_COUNT: u32` consistent (spec + Task 3). No naming drift.
- The exit-criteria list in the spec maps 1:1 onto the tasks + commits above.
