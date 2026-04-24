# Phase 2 W2: STUN Integration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** host/viewer が起動時に STUN で自機の public IP:port(srflx)を学習し、W1 で作った signaling 経由で相手に流す。Selection/hole-punching は W3 で触るので、W2 の peer_addr は引き続き Host candidate を採用。

**Architecture:** 新クレート `nat-traversal`(stun_codec ベース、~80行)に `learn_public_addr(socket, server_addr, timeout)` を置く。`signaling-client::rendezvous_as_*` が `stun_url: Option<Url>` を受けて STUN 成功時に Host + Srflx の 2 candidate を signaling に流す。`signaling-server` の Srflx 拒否を撤廃(Relay は引き続き拒否)。

**Tech Stack:** Rust (Tokio 1.40, stun_codec 0.3, bytecodec 0.4, url 2, 既存の prdt-signaling-* + prdt-transport + prdt-crypto)

**Spec:** `docs/superpowers/specs/2026-04-24-phase2-w2-stun-integration-design.md`

---

## File Structure

新規:

```
crates/nat-traversal/
  Cargo.toml
  src/lib.rs             # re-export
  src/error.rs           # StunError
  src/stun.rs            # learn_public_addr + Attribute enum
  tests/mock_stun.rs     # in-process STUN server で roundtrip テスト
```

変更:

```
Cargo.toml                             # workspace members + stun_codec/bytecodec
crates/signaling-client/Cargo.toml     # prdt-nat-traversal dep 追加
crates/signaling-client/src/config.rs  # RendezvousConfig::stun_url + Outcome::peer_candidates
crates/signaling-client/src/rendezvous.rs  # STUN 呼出 + 複数 candidate 送受信
crates/signaling-server/src/ws.rs      # Srflx 受理、Relay は従来通り拒否
crates/signaling-server/tests/server_tests.rs  # 既存 non_host_* を split
crates/signaling-client/tests/*.rs     # W1 テストを新シグネチャに追従
crates/host/Cargo.toml                 # url は既に入っている、prdt-nat-traversal はいらない
crates/host/src/main.rs                # --stun-url フラグ + rendezvous 引数追加
crates/viewer/Cargo.toml               # 同上
crates/viewer/src/main.rs              # 同上
```

---

## Conventions

- TDD: failing test first → verify failure → implement → verify pass → commit
- コミット単位は 1 タスク 1 コミット(メッセージは `<scope>: <imperative>` 形式)
- 各タスクで `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` を通す
- `tokio-tungstenite` / `serde_json` / `futures-util` は既存 dev-dep なのでそのまま使用
- 日付は絶対日付で記載(`2026-04-24`)
- branch は `phase2-w2-stun`(既に作成・チェックアウト済)
- `cargo build --workspace` は media-win の NV_CODEC_SDK_PATH 要件のためローカル環境で失敗する場合あり — per-crate test で判定

---

## Task 1: Scaffold nat-traversal crate + workspace deps

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/nat-traversal/Cargo.toml`
- Create: `crates/nat-traversal/src/lib.rs`
- Create: `crates/nat-traversal/src/error.rs`
- Create: `crates/nat-traversal/src/stun.rs` (empty stub)

- [ ] **Step 1: Add nat-traversal to workspace and STUN crates to shared deps**

Modify `Cargo.toml`:

1. In `[workspace] members`, append `"crates/nat-traversal",`.

2. In `[workspace.dependencies]`, append:
```toml
# STUN
stun_codec = "0.3"
bytecodec = "0.4"
```

- [ ] **Step 2: Create nat-traversal crate**

`crates/nat-traversal/Cargo.toml`:
```toml
[package]
name = "prdt-nat-traversal"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio = { workspace = true, features = ["net", "time", "rt"] }
stun_codec = { workspace = true }
bytecodec = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread", "net", "time", "io-util"] }
```

`crates/nat-traversal/src/lib.rs`:
```rust
//! NAT traversal primitives for power-remote-dt.
//! Currently provides a STUN binding client. TURN client is W4.

pub mod error;
pub mod stun;

pub use error::StunError;
pub use stun::learn_public_addr;
```

`crates/nat-traversal/src/error.rs`:
```rust
#[derive(thiserror::Error, Debug)]
pub enum StunError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout waiting for STUN response")]
    Timeout,
    #[error("decode: {0}")]
    Decode(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("no XOR-MAPPED-ADDRESS attribute")]
    NoMappedAddress,
}
```

`crates/nat-traversal/src/stun.rs`:
```rust
//! STUN Binding Request client (RFC 5389).
//! Implementation in Task 2.
```

- [ ] **Step 3: Verify workspace builds**

Run: `cargo check -p prdt-nat-traversal`
Expected: clean check.

- [ ] **Step 4: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add Cargo.toml crates/nat-traversal
git commit -m "nat-traversal: scaffold crate + workspace STUN deps"
```

---

## Task 2: Implement STUN `learn_public_addr` with in-process mock test

**Files:**
- Modify: `crates/nat-traversal/src/stun.rs`
- Create: `crates/nat-traversal/tests/mock_stun.rs`

- [ ] **Step 1: Write failing test**

`crates/nat-traversal/tests/mock_stun.rs`:
```rust
//! In-process STUN server for roundtrip tests. Implements just enough of
//! RFC 5389 to answer a Binding Request with an XOR-MAPPED-ADDRESS.

use bytecodec::{DecodeExt, EncodeExt};
use prdt_nat_traversal::{learn_public_addr, StunError};
use std::net::SocketAddr;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId,
};
use tokio::net::UdpSocket;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_mock_stun_echoing_source_addr() -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut decoder = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = decoder.decode_from_bytes(&buf[..n]) else { continue };
            if req.class() != MessageClass::Request || req.method() != BINDING {
                continue;
            }
            let mut resp = Message::new(MessageClass::SuccessResponse, BINDING, req.transaction_id());
            resp.add_attribute(XorMappedAddress::new(src).into());
            let mut encoder = MessageEncoder::<Attribute>::new();
            let bytes = encoder.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    addr
}

#[tokio::test]
async fn happy_path_learns_own_addr() {
    let server_addr = spawn_mock_stun_echoing_source_addr().await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let expected = client.local_addr().unwrap();

    let learned = learn_public_addr(&client, server_addr, Duration::from_secs(2))
        .await
        .unwrap();

    assert_eq!(learned.ip(), expected.ip());
    assert_eq!(learned.port(), expected.port());
}

#[tokio::test]
async fn timeout_when_server_silent() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    // Do NOT spawn a reader; packets sit in the kernel queue.
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let err = learn_public_addr(&client, silent_addr, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(matches!(err, StunError::Timeout), "got: {err:?}");
}

#[tokio::test]
async fn ignores_wrong_transaction_id() {
    // Server always replies with transaction id = [0xFF; 12], never matching.
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut decoder = MessageDecoder::<Attribute>::new();
            let Ok(Ok(_req)) = decoder.decode_from_bytes(&buf[..n]) else { continue };
            let mut resp = Message::new(
                MessageClass::SuccessResponse,
                BINDING,
                TransactionId::new([0xFF; 12]),
            );
            resp.add_attribute(XorMappedAddress::new(src).into());
            let mut encoder = MessageEncoder::<Attribute>::new();
            let bytes = encoder.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let err = learn_public_addr(&client, server_addr, Duration::from_millis(400))
        .await
        .unwrap_err();
    assert!(matches!(err, StunError::Timeout), "got: {err:?}");
}
```

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-nat-traversal --test mock_stun`
Expected: FAIL (`learn_public_addr` not yet implemented).

- [ ] **Step 3: Implement `learn_public_addr`**

REPLACE `crates/nat-traversal/src/stun.rs`:
```rust
//! STUN Binding Request client (RFC 5389).
//!
//! `learn_public_addr` sends a single Binding Request on the provided UDP
//! socket, waits up to `timeout`, and returns the XOR-MAPPED-ADDRESS attribute
//! from the success response.

use crate::error::StunError;
use bytecodec::{DecodeExt, EncodeExt};
use std::net::SocketAddr;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId,
};
use tokio::net::UdpSocket;
use tokio::time::timeout;
use tracing::{debug, instrument};

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

fn random_transaction_id() -> TransactionId {
    use rand_core::{OsRng, RngCore};
    let mut id = [0u8; 12];
    OsRng.fill_bytes(&mut id);
    TransactionId::new(id)
}

#[instrument(skip(socket), fields(%server_addr))]
pub async fn learn_public_addr(
    socket: &UdpSocket,
    server_addr: SocketAddr,
    timeout_duration: Duration,
) -> Result<SocketAddr, StunError> {
    let txn_id = random_transaction_id();
    let request = Message::<Attribute>::new(MessageClass::Request, BINDING, txn_id);

    let mut encoder = MessageEncoder::<Attribute>::new();
    let req_bytes = encoder
        .encode_into_bytes(request)
        .map_err(|e| StunError::Encode(e.to_string()))?;

    socket.send_to(&req_bytes, server_addr).await?;
    debug!(len = req_bytes.len(), "stun binding request sent");

    let mut buf = [0u8; 1500];
    let deadline = tokio::time::Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(StunError::Timeout);
        }
        let (n, _from) = match timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(StunError::Io(e)),
            Err(_) => return Err(StunError::Timeout),
        };

        let mut decoder = MessageDecoder::<Attribute>::new();
        let msg = match decoder.decode_from_bytes(&buf[..n]) {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                debug!(error = ?e, "stun decode error; ignoring packet");
                continue;
            }
            Err(e) => {
                debug!(error = %e, "bytecodec error; ignoring packet");
                continue;
            }
        };
        if msg.transaction_id() != txn_id {
            debug!("transaction id mismatch; ignoring packet");
            continue;
        }
        if msg.class() != MessageClass::SuccessResponse {
            return Err(StunError::Decode(format!(
                "unexpected message class: {:?}",
                msg.class()
            )));
        }
        if let Some(xma) = msg.get_attribute::<XorMappedAddress>() {
            return Ok(xma.address());
        }
        return Err(StunError::NoMappedAddress);
    }
}
```

Add `rand_core` to `crates/nat-traversal/Cargo.toml` `[dependencies]`:
```toml
rand_core = { version = "0.6", features = ["getrandom"] }
```

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-nat-traversal`
Expected: 3 tests PASS (happy_path, timeout, wrong_txn).

- [ ] **Step 5: clippy**

Run: `cargo clippy -p prdt-nat-traversal --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/nat-traversal
git commit -m "nat-traversal: implement STUN learn_public_addr with mock-server tests"
```

---

## Task 3: Extend signaling-client types — stun_url + peer_candidates

**Files:**
- Modify: `crates/signaling-client/src/config.rs`
- Modify: `crates/signaling-client/Cargo.toml` (nat-traversal dep)
- Modify: callers of RendezvousOutcome in tests (compile-fix only, no logic change)

- [ ] **Step 1: Add nat-traversal dep**

Append to `crates/signaling-client/Cargo.toml` `[dependencies]`:
```toml
prdt-nat-traversal = { path = "../nat-traversal" }
```

- [ ] **Step 2: Add `stun_url` to RendezvousConfig, `peer_candidates` to RendezvousOutcome**

REPLACE `crates/signaling-client/src/config.rs`:
```rust
use prdt_signaling_proto::Candidate;
use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
    /// STUN server URL, e.g. `stun://stun.l.google.com:19302`. None = STUN disabled.
    pub stun_url: Option<Url>,
}

impl RendezvousConfig {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
}

#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub pubkey_b64: String,
}

#[derive(Debug, Clone)]
pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_addr: SocketAddr,
    pub peer_pubkey_b64: Option<String>,
    /// All PeerCandidates received from the other side (order preserved).
    /// W2 still picks peer_addr from the first Host-typ candidate; W3 will
    /// use this list for selection/hole-punching.
    pub peer_candidates: Vec<Candidate>,
}
```

- [ ] **Step 3: Temporarily keep rendezvous.rs returning empty peer_candidates + ignoring stun_url**

In `crates/signaling-client/src/rendezvous.rs`, find the two `Ok(RendezvousOutcome { ... })` returns. Append `peer_candidates: vec![]` to each struct literal so the crate still compiles.

(Example for `rendezvous_as_host`'s return:)
```rust
Ok(RendezvousOutcome {
    session_id,
    peer_addr: peer,
    peer_pubkey_b64: None,
    peer_candidates: vec![],
})
```

Do the same for `rendezvous_as_viewer` (set `peer_pubkey_b64` to its existing value, add `peer_candidates: vec![]`).

Do NOT implement stun_url handling yet — that's Task 5. Just keep the type-compatible field.

- [ ] **Step 4: Update tests that construct RendezvousConfig**

In EACH of these test files, add `stun_url: None` to every `RendezvousConfig { ... }` literal:
- `crates/signaling-client/tests/mock_host_flow.rs`
- `crates/signaling-client/tests/mock_viewer_flow.rs`
- `crates/signaling-client/tests/timeout_stages.rs`
- `crates/signaling-client/tests/error_mapping.rs`
- `crates/signaling-client/tests/w1_smoke.rs`

(Use grep to find occurrences: `grep -rn 'RendezvousConfig {' crates/signaling-client/tests/`)

- [ ] **Step 5: Run all client tests and verify still pass**

Run: `cargo test -p prdt-signaling-client`
Expected: all 7 W1 tests still PASS (peer_candidates is empty but type-compatible).

- [ ] **Step 6: clippy**

Run: `cargo clippy -p prdt-signaling-client --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/signaling-client
git commit -m "signaling-client: add stun_url config + peer_candidates outcome (types only)"
```

---

## Task 4: signaling-server — accept Srflx, still reject Relay

**Files:**
- Modify: `crates/signaling-server/src/ws.rs`
- Modify: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Replace `non_host_candidate_type_rejected` with split tests**

Find the existing test `non_host_candidate_type_rejected` in `crates/signaling-server/tests/server_tests.rs` and REPLACE it with:

```rust
#[tokio::test]
async fn srflx_candidate_forwarded() {
    let (addr, _) = start_test_server().await;

    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;

    let h_start = ws_recv(&mut host_ws).await;
    let _ = ws_recv(&mut viewer_ws).await;
    let sid = match h_start {
        ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };

    ws_send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate {
            typ: CandidateType::Srflx,
            ip: "198.51.100.42".into(),
            port: 55_000,
            priority: prdt_signaling_proto::PRIORITY_SRFLX,
        },
    }).await;

    // Host should receive the Srflx candidate as-is.
    let m = ws_recv(&mut host_ws).await;
    match m {
        ServerMessage::PeerCandidate { session_id, candidate } => {
            assert_eq!(session_id, sid);
            assert_eq!(candidate.typ, CandidateType::Srflx);
            assert_eq!(candidate.ip, "198.51.100.42");
            assert_eq!(candidate.port, 55_000);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn relay_candidate_still_rejected() {
    let (addr, _) = start_test_server().await;

    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;
    let h_start = ws_recv(&mut host_ws).await;
    let _ = ws_recv(&mut viewer_ws).await;
    let sid = match h_start {
        ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };

    ws_send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid,
        candidate: Candidate {
            typ: CandidateType::Relay,
            ip: "1.2.3.4".into(),
            port: 1,
            priority: prdt_signaling_proto::PRIORITY_RELAY,
        },
    }).await;

    let err = ws_recv(&mut viewer_ws).await;
    match err {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::UnsupportedCandidateType);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 2: Run and verify — srflx_candidate_forwarded FAILS, relay_candidate_still_rejected PASSES**

Run: `cargo test -p prdt-signaling-server --test server_tests -- srflx_ relay_`
Expected:
- `srflx_candidate_forwarded` FAILS (server rejects Srflx today)
- `relay_candidate_still_rejected` PASSES (existing reject path)

- [ ] **Step 3: Lift Srflx rejection in ws.rs**

In `crates/signaling-server/src/ws.rs`, find BOTH loops (`host_loop` and `viewer_loop`). Locate this block in each:

```rust
if candidate.typ != prdt_signaling_proto::CandidateType::Host {
    send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "only host candidates supported in W1").await;
    continue;
}
```

REPLACE with:
```rust
if candidate.typ == prdt_signaling_proto::CandidateType::Relay {
    send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "relay candidates require W4 TURN").await;
    continue;
}
```

- [ ] **Step 4: Run full server suite**

Run: `cargo test -p prdt-signaling-server`
Expected: 9 tests PASS (8 from W1 minus 1 replaced + 2 new = 9).

- [ ] **Step 5: clippy**

Run: `cargo clippy -p prdt-signaling-server --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/signaling-server
git commit -m "signaling-server: accept Srflx candidates, keep Relay rejected"
```

---

## Task 5: signaling-client — send Srflx when stun_url provided

**Files:**
- Modify: `crates/signaling-client/src/rendezvous.rs`
- Create: `crates/signaling-client/tests/w2_stun_mock_host.rs`
- Modify: `crates/signaling-client/Cargo.toml` (dev-deps)

- [ ] **Step 1: Add nat-traversal to dev-deps and ensure tokio UdpSocket works**

Ensure `crates/signaling-client/Cargo.toml` `[dependencies]` has `prdt-nat-traversal = { path = "../nat-traversal" }` (added in Task 3).

Also ensure `[dev-dependencies]` has `tokio` with `net` feature — if the existing tokio feature list doesn't include `net`, add it.

- [ ] **Step 2: Write failing test**

`crates/signaling-client/tests/w2_stun_mock_host.rs`:
```rust
//! Verify rendezvous_as_host sends BOTH Host and Srflx candidates when stun_url is given.

use bytecodec::{DecodeExt, EncodeExt};
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_client::{rendezvous_as_host, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder,
};
use tokio::net::UdpSocket;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_stun_mock(report_addr: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut dec = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = dec.decode_from_bytes(&buf[..n]) else { continue };
            if req.class() != MessageClass::Request || req.method() != BINDING {
                continue;
            }
            // Always report the caller-supplied public addr, not the real src.
            let mut resp = Message::new(MessageClass::SuccessResponse, BINDING, req.transaction_id());
            resp.add_attribute(XorMappedAddress::new(report_addr).into());
            let mut enc = MessageEncoder::<Attribute>::new();
            let bytes = enc.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    addr
}

async fn spawn_signaling() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn host_sends_both_host_and_srflx_when_stun_url_given() {
    let sig_addr = spawn_signaling().await;
    // Mock STUN reports a fixed "public" addr of 198.51.100.7:55555
    let fake_public: SocketAddr = "198.51.100.7:55555".parse().unwrap();
    let stun_addr = spawn_stun_mock(fake_public).await;

    let sig_url: Url = format!("ws://{sig_addr}/signal").parse().unwrap();
    let stun_url: Url = format!("stun://{stun_addr}").parse().unwrap();

    let host_task = tokio::spawn(async move {
        rendezvous_as_host(
            RendezvousConfig {
                url: sig_url,
                host_id: "h1".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(stun_url),
            },
            HostIdentity { pubkey_b64: "HPK".into() },
            "127.0.0.1:40100".parse().unwrap(),
        ).await
    });

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Connect as viewer, collect candidates
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(format!("ws://{sig_addr}/signal")).await.unwrap();
    viewer_ws.send(WsMessage::Text(serde_json::to_string(
        &ClientMessage::Connect { host_id: "h1".into() }
    ).unwrap())).await.unwrap();

    // expect SessionStart
    let _ = viewer_ws.next().await.unwrap().unwrap();

    // collect 2 PeerCandidates
    let mut got_host = None::<Candidate>;
    let mut got_srflx = None::<Candidate>;
    for _ in 0..2 {
        let frame = viewer_ws.next().await.unwrap().unwrap();
        let t = match frame { WsMessage::Text(s) => s, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&t).unwrap();
        match m {
            prdt_signaling_proto::ServerMessage::PeerCandidate { candidate, .. } => match candidate.typ {
                CandidateType::Host => got_host = Some(candidate),
                CandidateType::Srflx => got_srflx = Some(candidate),
                _ => panic!("unexpected typ: {:?}", candidate.typ),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    // We have what we wanted (2 candidates observed). The host task would
    // keep waiting for us to send a Candidate back; abort it since we don't
    // care about its return value for this test.
    host_task.abort();
    let _ = host_task.await;

    let host_cand = got_host.expect("Host candidate missing");
    assert_eq!(host_cand.ip, "127.0.0.1");
    assert_eq!(host_cand.port, 40100);
    assert_eq!(host_cand.priority, PRIORITY_HOST);

    let srflx_cand = got_srflx.expect("Srflx candidate missing");
    assert_eq!(srflx_cand.ip, "198.51.100.7");
    assert_eq!(srflx_cand.port, 55555);
}
```

Note: the test aborts the host future because it intentionally never completes rendezvous — we only need to observe the 2 outgoing candidates.

- [ ] **Step 3: Run and verify fails**

Run: `cargo test -p prdt-signaling-client --test w2_stun_mock_host`
Expected: FAIL — currently only Host candidate is sent.

- [ ] **Step 4: Implement STUN integration in rendezvous_as_host**

In `crates/signaling-client/src/rendezvous.rs`, locate the host flow:

```rust
send_msg(&mut ws, &ClientMessage::Candidate {
    session_id: session_id.clone(),
    candidate: candidate_for(local_udp_addr),
}).await?;
```

REPLACE with a helper-call block:
```rust
send_candidates(&mut ws, &session_id, local_udp_addr, cfg.stun_url.as_ref()).await?;
```

Add the helper at the bottom of the same file (after the existing helpers):
```rust
async fn send_candidates(
    ws: &mut Ws,
    session_id: &str,
    local_udp_addr: SocketAddr,
    stun_url: Option<&url::Url>,
) -> Result<(), SignalingError> {
    // Always send Host candidate first.
    send_msg(ws, &ClientMessage::Candidate {
        session_id: session_id.to_string(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    // Try STUN if configured. Failures here are non-fatal — they just
    // mean no srflx candidate will be sent (caller may still get a working
    // Host-candidate path on LAN).
    if let Some(url) = stun_url {
        match resolve_and_learn_srflx(url, local_udp_addr).await {
            Ok(srflx) => {
                send_msg(ws, &ClientMessage::Candidate {
                    session_id: session_id.to_string(),
                    candidate: Candidate {
                        typ: CandidateType::Srflx,
                        ip: srflx.ip().to_string(),
                        port: srflx.port(),
                        priority: PRIORITY_SRFLX,
                    },
                }).await?;
                tracing::info!(%srflx, "srflx candidate sent");
            }
            Err(e) => {
                tracing::warn!(error = %e, "STUN failed; proceeding without srflx candidate");
            }
        }
    }
    Ok(())
}

async fn resolve_and_learn_srflx(
    stun_url: &url::Url,
    _local_udp_addr: SocketAddr,
) -> Result<SocketAddr, SignalingError> {
    if stun_url.scheme() != "stun" {
        return Err(SignalingError::Protocol(format!(
            "unsupported stun URL scheme: {}",
            stun_url.scheme()
        )));
    }
    let host = stun_url
        .host_str()
        .ok_or_else(|| SignalingError::Protocol("stun URL missing host".into()))?;
    let port = stun_url.port().unwrap_or(3478);
    let stun_addr = tokio::net::lookup_host(format!("{host}:{port}"))
        .await
        .map_err(|e| SignalingError::Protocol(format!("resolve stun: {e}")))?
        .next()
        .ok_or_else(|| SignalingError::Protocol("no addrs for stun host".into()))?;

    // Bind a *separate* UdpSocket for STUN to avoid mixing the STUN reply
    // frames into our main transport queue. The NAT mapping we learn via
    // this socket is NOT the same port the main transport will use, which
    // is a known W2 limitation; srflx accuracy requires sharing the socket.
    // Fix: use the existing bound transport socket. See below.
    //
    // Workaround: we share the bound port by opening a second socket bound
    // to the SAME local addr. Tokio/Windows does not permit this, so for
    // W2 we use a different ephemeral socket — srflx port will therefore
    // NOT match the UDP transport port. W3 will re-learn on the shared
    // socket. This is documented as an Open Question in the spec.
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let addr = prdt_nat_traversal::learn_public_addr(
        &probe,
        stun_addr,
        std::time::Duration::from_secs(3),
    )
    .await
    .map_err(|e| SignalingError::Protocol(format!("stun: {e}")))?;
    Ok(addr)
}
```

Imports to add at the top of `rendezvous.rs`:
```rust
use prdt_signaling_proto::PRIORITY_SRFLX;
use std::io;
```

And convert the existing `io::Error` source into SignalingError via `From<io::Error>`. Add to `crates/signaling-client/src/error.rs`:
```rust
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
```

Re-order / remove `std::io` import as needed; the From trait auto-wires through thiserror.

**IMPORTANT NOTE on the "separate socket" workaround**: The plan intentionally uses a fresh UDP socket for STUN probe rather than the main transport socket. This is because `CustomUdpTransport` owns its socket and doesn't currently expose it. Learning a srflx port on a DIFFERENT socket means the port in the srflx candidate does NOT map to where media traffic will actually arrive — this is a W2 limitation documented in the spec's Open Questions. W3 will fix it by either sharing the transport socket or doing STUN *inside* CustomUdpTransport. For W2, the test verifies the Srflx candidate is SENT with the STUN-reported address, which is what we need to prove the signaling path works.

Apply the same helper call in `rendezvous_as_viewer`: replace its existing single `send_msg(&mut ws, &ClientMessage::Candidate { ... })` with `send_candidates(&mut ws, &session_id, local_udp_addr, cfg.stun_url.as_ref()).await?;`.

- [ ] **Step 5: Run and verify pass**

Run: `cargo test -p prdt-signaling-client --test w2_stun_mock_host`
Expected: PASS.

- [ ] **Step 6: Run full client suite for regression**

Run: `cargo test -p prdt-signaling-client`
Expected: 8 tests PASS (7 W1 + 1 new).

- [ ] **Step 7: clippy**

Run: `cargo clippy -p prdt-signaling-client --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/signaling-client
git commit -m "signaling-client: send Srflx candidate via STUN when stun_url provided"
```

---

## Task 6: signaling-client — collect multiple PeerCandidates into peer_candidates

**Files:**
- Modify: `crates/signaling-client/src/rendezvous.rs`
- Create: `crates/signaling-client/tests/w2_peer_candidates.rs`

- [ ] **Step 1: Write failing test — rendezvous_as_viewer collects Host + Srflx**

`crates/signaling-client/tests/w2_peer_candidates.rs`:
```rust
//! Verify rendezvous_as_viewer collects ALL incoming PeerCandidates into
//! `peer_candidates` but still returns the Host-typ one as peer_addr.

use futures_util::{SinkExt, StreamExt};
use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST, PRIORITY_SRFLX};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::test]
async fn viewer_collects_host_and_srflx_peer_candidates() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Mock "host" that registers, waits for SessionStart, sends BOTH
    // a Host and a Srflx candidate, then stays alive briefly.
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(), pubkey_b64: "HPK".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await.unwrap();

        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };

        // Host candidate first
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40200, priority: PRIORITY_HOST },
        }).unwrap())).await.unwrap();
        // Then Srflx
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid,
            candidate: Candidate { typ: CandidateType::Srflx, ip: "198.51.100.9".into(), port: 44444, priority: PRIORITY_SRFLX },
        }).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local_udp: SocketAddr = "127.0.0.1:40201".parse().unwrap();
    let outcome = rendezvous_as_viewer(
        RendezvousConfig {
            url,
            host_id: "h1".into(),
            timeout: Duration::from_secs(5),
            stun_url: None,
        },
        local_udp,
    ).await.unwrap();

    // peer_addr = first Host-typ candidate
    assert_eq!(outcome.peer_addr.port(), 40200);
    // peer_candidates should contain both (order not asserted)
    let types: Vec<CandidateType> = outcome.peer_candidates.iter().map(|c| c.typ).collect();
    assert!(types.contains(&CandidateType::Host), "missing Host in {types:?}");
    assert!(types.contains(&CandidateType::Srflx), "missing Srflx in {types:?}");
}
```

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-signaling-client --test w2_peer_candidates`
Expected: FAIL — current viewer flow returns after the FIRST PeerCandidate.

- [ ] **Step 3: Update receive loop in both rendezvous functions**

In `crates/signaling-client/src/rendezvous.rs`, find the PeerCandidate receive block inside `rendezvous_as_host`:

```rust
let peer = match recv_msg(&mut ws, "peer_candidate", PEER_CANDIDATE_TIMEOUT).await? {
    ServerMessage::PeerCandidate { candidate, .. } => {
        if candidate.typ != CandidateType::Host {
            return Err(SignalingError::BadCandidate(format!("unsupported typ {:?}", candidate.typ)));
        }
        parse_peer_addr(&candidate)?
    }
    ...
};
```

REPLACE with a helper call + accumulator:
```rust
let (peer, peer_candidates) = recv_peer_candidates(&mut ws, PEER_CANDIDATE_TIMEOUT).await?;
```

And in the final `return Ok(RendezvousOutcome { ... })`, replace `peer_candidates: vec![]` with `peer_candidates`.

Do the same in `rendezvous_as_viewer`.

Add the helper at the bottom of `rendezvous.rs`:
```rust
async fn recv_peer_candidates(
    ws: &mut Ws,
    total_timeout: Duration,
) -> Result<(SocketAddr, Vec<Candidate>), SignalingError> {
    let deadline = tokio::time::Instant::now() + total_timeout;
    let mut collected: Vec<Candidate> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            // Timed out — did we ever get a Host?
            if let Some(host) = collected.iter().find(|c| c.typ == CandidateType::Host) {
                return Ok((parse_peer_addr(host)?, collected));
            }
            return Err(SignalingError::Timeout { stage: "peer_candidate" });
        }
        match recv_msg(ws, "peer_candidate", remaining).await? {
            ServerMessage::PeerCandidate { candidate, .. } => {
                let is_host = candidate.typ == CandidateType::Host;
                let parsed_if_host = if is_host { Some(parse_peer_addr(&candidate)?) } else { None };
                collected.push(candidate);
                if let Some(addr) = parsed_if_host {
                    // Got a Host — commit to it immediately. Srflx left in
                    // peer_candidates for W3 to consult.
                    return Ok((addr, collected));
                }
                // Non-Host (Srflx in W2) — keep waiting briefly for a Host.
            }
            ServerMessage::Error { code, message } => {
                return Err(SignalingError::Server { code, message });
            }
            other => {
                return Err(SignalingError::Protocol(format!(
                    "expected PeerCandidate, got {other:?}"
                )))
            }
        }
    }
}

fn parse_peer_addr(c: &Candidate) -> Result<SocketAddr, SignalingError> {
    format!("{}:{}", c.ip, c.port)
        .parse()
        .map_err(|e| SignalingError::BadCandidate(format!("{e}: {}:{}", c.ip, c.port)))
}
```

(If `parse_peer_addr` already exists inline in earlier code, keep only one copy — move it up so both rendezvous functions and `recv_peer_candidates` use the same helper.)

Remove any now-unused code from the old `let peer = match recv_msg(...)` blocks — the helper replaces them.

Keep behavior for BadCandidate: when the Host candidate's ip/port is unparseable, propagate the error.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-client --test w2_peer_candidates`
Expected: PASS.

- [ ] **Step 5: Run full client regression**

Run: `cargo test -p prdt-signaling-client`
Expected: all 9 tests PASS (7 W1 + 2 new W2).

- [ ] **Step 6: clippy**

Run: `cargo clippy -p prdt-signaling-client --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/signaling-client
git commit -m "signaling-client: collect multiple PeerCandidates into peer_candidates"
```

---

## Task 7: host bin — `--stun-url` CLI flag + rendezvous wire-up

**Files:**
- Modify: `crates/host/src/main.rs`

- [ ] **Step 1: Add CLI flag**

In `crates/host/src/main.rs`, locate the `struct Args` declaration and APPEND this field AFTER `signaling_timeout`:
```rust
    /// STUN server URL (e.g. stun://stun.l.google.com:19302). Optional.
    /// When set together with --signaling-url, the host learns its public
    /// addr and sends it alongside the LAN Host candidate.
    #[arg(long)]
    stun_url: Option<url::Url>,
```

- [ ] **Step 2: Pass stun_url into RendezvousConfig**

Find the existing `RendezvousConfig { url, host_id, timeout, ... }` construction. Add `stun_url: args.stun_url.clone(),` to the struct literal.

- [ ] **Step 3: Verify compile + --help**

Run: `cargo build -p prdt-host` (may fail on media-win pre-existing env; that's OK)
Alternatively: `cargo check -p prdt-host`

Run: `./target/debug/prdt-host --help 2>&1 | grep stun-url` (only if build succeeded)
Expected: flag appears in help.

- [ ] **Step 4: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/host
git commit -m "host: add --stun-url CLI flag; pass to rendezvous_as_host"
```

---

## Task 8: viewer bin — `--stun-url` CLI flag + rendezvous wire-up

**Files:**
- Modify: `crates/viewer/src/main.rs`

- [ ] **Step 1: Add CLI flag**

In `crates/viewer/src/main.rs`, locate `struct Args` and APPEND AFTER `signaling_timeout` (or the last signaling-related field):
```rust
    /// STUN server URL (e.g. stun://stun.l.google.com:19302). Optional.
    /// When set together with --signaling-url, the viewer learns its public
    /// addr and sends it alongside the LAN Host candidate.
    #[arg(long)]
    stun_url: Option<url::Url>,
```

- [ ] **Step 2: Pass stun_url into RendezvousConfig inside spawn_worker_tasks**

`spawn_worker_tasks` currently takes `signaling_url: Option<Url>`, `host_id: Option<String>`, `signaling_timeout_s: u64` etc. Extend its signature to include `stun_url: Option<url::Url>`.

In the function body, locate the `RendezvousConfig { url, host_id, timeout, ... }` construction and add `stun_url: stun_url.clone(),`.

Then in `main()` update the `spawn_worker_tasks(...)` call site to pass `args.stun_url.clone(),` in the matching position.

- [ ] **Step 3: Verify build + --help**

Run: `cargo check -p prdt-viewer`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/viewer
git commit -m "viewer: add --stun-url CLI flag; pass to rendezvous_as_viewer"
```

---

## Task 9: W2 E2E smoke integration test

**Files:**
- Create: `crates/signaling-client/tests/w2_smoke.rs`

- [ ] **Step 1: Write the smoke test**

`crates/signaling-client/tests/w2_smoke.rs`:
```rust
//! W2 end-to-end: mock STUN + in-process signaling-server + both rendezvous
//! functions with stun_url set + Noise handshake + Hello/HelloAck.
//! Asserts both peers see 2 PeerCandidates (Host + Srflx) and still establish
//! a working encrypted channel (peer_addr still = Host candidate in W2).

use bytecodec::{DecodeExt, EncodeExt};
use prdt_crypto::KeyPair;
use prdt_protocol::{frame::Codec, MonitorRect};
use prdt_signaling_client::{rendezvous_as_host, rendezvous_as_viewer, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::CandidateType;
use prdt_signaling_server::{router, ServerConfig, ServerState};
use prdt_transport::{
    host_handshake, viewer_handshake, CustomUdpTransport, HelloRequest, UdpTransportConfig,
    DEFAULT_HANDSHAKE_TIMEOUT,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::{
    define_attribute_enums, Message, MessageClass, MessageDecoder, MessageEncoder,
};
use tokio::net::UdpSocket;
use url::Url;

define_attribute_enums!(
    Attribute,
    AttributeDecoder,
    AttributeEncoder,
    [XorMappedAddress]
);

async fn spawn_stun_mock(report_addr: SocketAddr) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, src)) = socket.recv_from(&mut buf).await else { break };
            let mut dec = MessageDecoder::<Attribute>::new();
            let Ok(Ok(req)) = dec.decode_from_bytes(&buf[..n]) else { continue };
            if req.class() != MessageClass::Request || req.method() != BINDING { continue; }
            let mut resp = Message::new(MessageClass::SuccessResponse, BINDING, req.transaction_id());
            resp.add_attribute(XorMappedAddress::new(report_addr).into());
            let mut enc = MessageEncoder::<Attribute>::new();
            let bytes = enc.encode_into_bytes(resp).unwrap();
            let _ = socket.send_to(&bytes, src).await;
        }
    });
    addr
}

async fn spawn_signaling() -> Url {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("ws://{addr}/signal").parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w2_smoke_stun_plus_signaling_plus_noise() {
    let signaling_url = spawn_signaling().await;

    let fake_host_public: SocketAddr = "198.51.100.10:10000".parse().unwrap();
    let fake_viewer_public: SocketAddr = "198.51.100.20:20000".parse().unwrap();
    let host_stun = spawn_stun_mock(fake_host_public).await;
    let viewer_stun = spawn_stun_mock(fake_viewer_public).await;

    let host_kp = KeyPair::generate();
    let host_pub_b64 = host_kp.public.to_base64();
    let host_pub_copy = host_kp.public;

    let sig_url_a = signaling_url.clone();
    let host_stun_url: Url = format!("stun://{host_stun}").parse().unwrap();
    let host_fut = async move {
        let transport = Arc::new(
            CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
                .await.unwrap(),
        );
        let local = transport.local_addr().unwrap();
        let outcome = rendezvous_as_host(
            RendezvousConfig {
                url: sig_url_a,
                host_id: "w2-smoke".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(host_stun_url),
            },
            HostIdentity { pubkey_b64: host_pub_b64 },
            local,
        ).await.expect("host rendezvous");

        assert!(outcome.peer_candidates.iter().any(|c| c.typ == CandidateType::Host));
        assert!(outcome.peer_candidates.iter().any(|c| c.typ == CandidateType::Srflx));

        transport.configure_peer(outcome.peer_addr).await;
        transport.handshake_as_server(&host_kp).await.expect("host Noise");
        let _req = host_handshake(
            &*transport,
            0xDEAD_BEEF,
            0,
            10_000_000,
            MonitorRect::new(0, 0, 1920, 1080),
            MonitorRect::new(0, 0, 1920, 1080),
            Duration::from_secs(5),
        ).await.expect("host Hello");
    };

    let sig_url_b = signaling_url.clone();
    let viewer_stun_url: Url = format!("stun://{viewer_stun}").parse().unwrap();
    let viewer_fut = async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let transport = Arc::new(
            CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default())
                .await.unwrap(),
        );
        let local = transport.local_addr().unwrap();
        let outcome = rendezvous_as_viewer(
            RendezvousConfig {
                url: sig_url_b,
                host_id: "w2-smoke".into(),
                timeout: Duration::from_secs(5),
                stun_url: Some(viewer_stun_url),
            },
            local,
        ).await.expect("viewer rendezvous");
        assert!(outcome.peer_pubkey_b64.is_some());
        assert!(outcome.peer_candidates.iter().any(|c| c.typ == CandidateType::Host));
        assert!(outcome.peer_candidates.iter().any(|c| c.typ == CandidateType::Srflx));

        transport.configure_peer(outcome.peer_addr).await;
        transport.handshake_as_client(&host_pub_copy, DEFAULT_HANDSHAKE_TIMEOUT).await.expect("viewer Noise");
        let ack = viewer_handshake(
            &*transport,
            &HelloRequest { req_width: 1920, req_height: 1080, req_fps: 60, codec: Codec::H265 },
            Duration::from_millis(500),
            5,
        ).await.expect("viewer Hello");
        assert_eq!(ack.session_id, 0xDEAD_BEEF);
    };

    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(host_fut, viewer_fut)
    }).await.expect("W2 smoke must complete within 15s");
}
```

- [ ] **Step 2: Add stun_codec + bytecodec to signaling-client dev-deps**

Append to `crates/signaling-client/Cargo.toml` `[dev-dependencies]`:
```toml
stun_codec = { workspace = true }
bytecodec = { workspace = true }
```

- [ ] **Step 3: Run**

Run: `cargo test -p prdt-signaling-client --test w2_smoke`
Expected: PASS within 15s.

- [ ] **Step 4: Run full client regression**

Run: `cargo test -p prdt-signaling-client`
Expected: all W1 + W2 tests pass (7 + 3 = 10 total).

- [ ] **Step 5: Commit**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add crates/signaling-client
git commit -m "signaling-client: W2 end-to-end smoke (STUN + signaling + Noise)"
```

---

## Task 10: regression + clippy + manual smoke doc + tag

- [ ] **Step 1: Per-crate regression**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
cargo test -p prdt-signaling-proto
cargo test -p prdt-signaling-server
cargo test -p prdt-signaling-client
cargo test -p prdt-nat-traversal
cargo test -p prdt-crypto
cargo test -p prdt-protocol
cargo test -p prdt-transport
cargo test -p prdt-filetransfer
```

Expected: all green. Collect test-count totals for the report.

- [ ] **Step 2: clippy on signaling-touched crates**

```bash
cargo clippy -p prdt-signaling-proto --all-targets -- -D warnings
cargo clippy -p prdt-signaling-server --all-targets -- -D warnings
cargo clippy -p prdt-signaling-client --all-targets -- -D warnings
cargo clippy -p prdt-nat-traversal --all-targets -- -D warnings
cargo clippy -p prdt-crypto --all-targets -- -D warnings
cargo clippy -p prdt-protocol --all-targets -- -D warnings
cargo clippy -p prdt-transport --all-targets -- -D warnings
```

Expected: clean for all.

- [ ] **Step 3: Manual smoke TODO doc**

Create `docs/superpowers/plans/2026-04-24-phase2-w2-manual-smoke-TODO.md`:

```markdown
# Phase 2 W2 — manual smoke (user action, real Internet)

Automated tests + mock STUN fully green on this branch. Manual confirmation
with a real public STUN server verifies the srflx learning against a live
RFC 5389 implementation.

## Prerequisites

- Same as W1 (NV_CODEC_SDK_PATH etc.)
- Outbound UDP to `stun.l.google.com:19302` permitted

## Terminal 1 — signaling server
```
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

## Terminal 2 — host (LAN-bind for same-machine test)
```
cargo run -p prdt-host --release -- \
    --bind 127.0.0.1:9000 \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w2-manual \
    --signaling-timeout 60 \
    --stun-url stun://stun.l.google.com:19302
```

## Terminal 3 — viewer
```
cargo run -p prdt-viewer --release -- \
    --signaling-url ws://127.0.0.1:8080/signal \
    --host-id w2-manual \
    --stun-url stun://stun.l.google.com:19302
```

## Expected

- Both host and viewer log `srflx candidate sent` with a real public IP:port
- Signaling server log shows TWO `candidate_forwarded` events per side
  (one Host, one Srflx)
- Noise + Hello/HelloAck complete as in W1
- Video flows

## Known W2 caveat

The srflx port in the candidate is NOT the port the main UDP transport
uses (STUN probe is on a separate socket). Therefore a real remote peer
would receive the srflx IP correctly but the port would not map to host's
actual media socket. This is intentional for W2; W3 fixes it by sharing
the transport socket with STUN.

Once confirmed, tag `phase2-w2-complete` is fully validated.
```

- [ ] **Step 4: Update project memory**

Edit `C:\Users\nakan\.claude\projects\E--project-rust-desktop-power-remote-dt\memory\project_overview.md`:

In the "Completed phases (git tags)" list, APPEND (after the existing phase2-w1-complete entry):
```markdown
- `phase2-w2-complete` — STUN client + srflx candidate propagation via signaling; peer_candidates collected in RendezvousOutcome; selection still Host-first (W3 changes it)
```

In "Remaining (not done)", change the Phase 2 line to:
```markdown
- **Phase 2 W3〜W6** — hole punching + TURN + ID system + E2E on real public Internet
```

- [ ] **Step 5: Commit + tag**

```bash
cd "E:/project/rust-desktop/power-remote-dt"
git add docs/superpowers/plans/2026-04-24-phase2-w2-manual-smoke-TODO.md
git commit -m "phase2-w2: manual smoke instructions for real stun.l.google.com"
git tag phase2-w2-complete
git tag --list phase2-w2-complete
git log --oneline -10
```

(Memory files live outside the repo — edit but do not commit.)

---

## Self-Review checklist (run after all tasks implemented)

- [ ] spec Exit Criteria all covered by implementing tasks
- [ ] W1 regression holds (existing 7 signaling-client tests still pass)
- [ ] clippy clean across all signaling-touched crates
- [ ] `phase2-w2-complete` tag is created locally and points to the last commit
- [ ] Manual smoke TODO doc committed
