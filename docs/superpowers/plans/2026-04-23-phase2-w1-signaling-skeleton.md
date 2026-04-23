# Phase 2 W1: Signaling Protocol Skeleton — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新クレート `signaling-proto` / `signaling-client` / `signaling-server` を作り、LAN 同一マシンの host + viewer が signaling 経由でお互いの UDP アドレスを学習し、既存の Noise_NK ハンドシェイクから映像表示まで通すところを完成させる。STUN/TURN は W2 以降。

**Architecture:** JSON over WebSocket(axum 0.7 + tokio-tungstenite)+ `rendezvous_as_{host,viewer}` 関数型 client + host_id ベース TOFU(既存 `KnownHosts` に `insert`/`save` を足し、`known-host-ids` という別ファイルを使う)。`transport` crate は signaling を知らず、呼び出し側が `configure_peer(peer_addr)` で橋渡しする。

**Tech Stack:** Rust (Tokio 1.40, axum 0.7, tokio-tungstenite 0.24, serde_json 1, uuid 1, dashmap 6, url 2, base64 0.22, existing `prdt-*` crates)

**Spec:** `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md`

---

## File Structure

新規ファイル:

```
crates/signaling-proto/
  Cargo.toml
  src/lib.rs                       # 型すべて(1 ファイルで完結、約 200 行)
  tests/roundtrip.rs               # proptest serde 往復
  tests/wire_format.rs             # JSON literal fixture

crates/signaling-server/
  Cargo.toml
  src/lib.rs                       # Router 構築・state・WS handler(テスト用に公開)
  src/state.rs                     # ServerState / HostEntry / SessionEntry
  src/ws.rs                        # WS handler + connection state machine
  src/main.rs                      # CLI + tracing + shutdown signal
  tests/server_tests.rs            # 同プロセス server 起動 + WS client で結合テスト

crates/signaling-client/
  Cargo.toml
  src/lib.rs                       # re-exports
  src/config.rs                    # RendezvousConfig / HostIdentity / RendezvousOutcome
  src/error.rs                     # SignalingError
  src/rendezvous.rs                # rendezvous_as_host / rendezvous_as_viewer + 共通 helpers
  tests/mock_host_flow.rs          # duplex モック server で host 状態機械
  tests/mock_viewer_flow.rs        # 同上 viewer 側
  tests/w1_smoke.rs                # 実 server + 2 rendezvous + Noise handshake 結合
```

変更ファイル:

```
Cargo.toml                         # workspace members + dependencies 追加
crates/crypto/src/known_hosts.rs   # insert / save + VerifyVerdict + verify_or_record
crates/crypto/src/lib.rs           # 上記 re-export
crates/host/src/main.rs            # --signaling-url / --host-id フラグ + rendezvous 分岐
crates/host/Cargo.toml             # signaling-{proto,client} + url + base64 依存追加
crates/viewer/src/main.rs          # 同上 + TOFU 判定
crates/viewer/Cargo.toml           # 同上
```

---

## Conventions

- テストはすべて Rust 標準の `#[test]` / `#[tokio::test]`(proptest は proto のみ)
- `cargo test -p <crate>` / `cargo clippy -p <crate> --all-targets -- -D warnings` を毎タスクで実行
- コミットメッセージは `<scope>: <imperative>` 形式(例: `signaling-proto: add Candidate types`)
- 各タスクは **完了時に必ず build + test + commit** まで進める
- log/tracing は既存の `tracing` + `tracing-subscriber` スタイルに合わせる

---

## Task 1: Workspace scaffold — add 3 empty crates

**Files:**
- Modify: `Cargo.toml` (workspace members + deps)
- Create: `crates/signaling-proto/Cargo.toml`
- Create: `crates/signaling-proto/src/lib.rs`
- Create: `crates/signaling-client/Cargo.toml`
- Create: `crates/signaling-client/src/lib.rs`
- Create: `crates/signaling-server/Cargo.toml`
- Create: `crates/signaling-server/src/lib.rs`
- Create: `crates/signaling-server/src/main.rs`

- [ ] **Step 1: Add crates to workspace members and add shared deps**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = [
    "crates/protocol",
    "crates/transport",
    "crates/media-win",
    "crates/input-win",
    "crates/host",
    "crates/viewer",
    "crates/latency-bench",
    "crates/crypto",
    "crates/audio",
    "crates/filetransfer",
    "crates/signaling-proto",
    "crates/signaling-client",
    "crates/signaling-server",
]
```

`[workspace.dependencies]` セクションに追加:
```toml
# Signaling
tokio-tungstenite = "0.24"
axum = { version = "0.7", features = ["ws"] }
serde_json = "1"
tokio-stream = "0.1"
uuid = { version = "1", features = ["v4"] }
dashmap = "6"
url = "2"
base64 = "0.22"
```

- [ ] **Step 2: Create `signaling-proto` crate stub**

`crates/signaling-proto/Cargo.toml`:
```toml
[package]
name = "prdt-signaling-proto"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
```

`crates/signaling-proto/src/lib.rs`:
```rust
//! Wire types for the power-remote-dt signaling protocol.
//!
//! All messages are UTF-8 JSON over WebSocket Text frames, one message per frame.
//! See `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md`.
```

- [ ] **Step 3: Create `signaling-client` crate stub**

`crates/signaling-client/Cargo.toml`:
```toml
[package]
name = "prdt-signaling-client"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-signaling-proto = { path = "../signaling-proto" }
tokio = { workspace = true }
tokio-tungstenite = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
url = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread", "io-util", "sync", "time"] }
```

`crates/signaling-client/src/lib.rs`:
```rust
//! WebSocket client for the power-remote-dt signaling rendezvous.
//!
//! `rendezvous_as_host` / `rendezvous_as_viewer` are the only public entry points.
```

- [ ] **Step 4: Create `signaling-server` crate stub**

`crates/signaling-server/Cargo.toml`:
```toml
[package]
name = "prdt-signaling-server"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "prdt-signaling-server"
path = "src/main.rs"

[lib]
name = "prdt_signaling_server"
path = "src/lib.rs"

[dependencies]
prdt-signaling-proto = { path = "../signaling-proto" }
tokio = { workspace = true }
tokio-stream = { workspace = true }
axum = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
uuid = { workspace = true }
dashmap = { workspace = true }
clap = { workspace = true }

[dev-dependencies]
tokio-tungstenite = { workspace = true }
tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread", "io-util", "sync", "time"] }
```

`crates/signaling-server/src/lib.rs`:
```rust
//! axum-based signaling server. Exposes `router()` for in-process testing.
```

`crates/signaling-server/src/main.rs`:
```rust
fn main() {
    println!("prdt-signaling-server: not yet implemented");
}
```

- [ ] **Step 5: Verify workspace builds**

Run: `cargo build --workspace`
Expected: clean build, 3 new empty crates included.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/signaling-proto crates/signaling-client crates/signaling-server
git commit -m "signaling: scaffold signaling-{proto,client,server} crates"
```

---

## Task 2: signaling-proto — Candidate + primitive enums

**Files:**
- Modify: `crates/signaling-proto/src/lib.rs`
- Create: `crates/signaling-proto/tests/roundtrip.rs`

- [ ] **Step 1: Write failing roundtrip test for Candidate**

`crates/signaling-proto/tests/roundtrip.rs`:
```rust
use prdt_signaling_proto::{Candidate, CandidateType};
use proptest::prelude::*;

fn arb_candidate_type() -> impl Strategy<Value = CandidateType> {
    prop_oneof![
        Just(CandidateType::Host),
        Just(CandidateType::Srflx),
        Just(CandidateType::Relay),
    ]
}

fn arb_candidate() -> impl Strategy<Value = Candidate> {
    (
        arb_candidate_type(),
        "[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}\\.[0-9]{1,3}".prop_map(String::from),
        any::<u16>(),
        any::<u32>(),
    )
        .prop_map(|(typ, ip, port, priority)| Candidate { typ, ip, port, priority })
}

proptest! {
    #[test]
    fn candidate_json_roundtrip(c in arb_candidate()) {
        let s = serde_json::to_string(&c).unwrap();
        let back: Candidate = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(c.typ, back.typ);
        prop_assert_eq!(c.ip, back.ip);
        prop_assert_eq!(c.port, back.port);
        prop_assert_eq!(c.priority, back.priority);
    }
}

#[test]
fn candidate_type_snake_case() {
    assert_eq!(serde_json::to_string(&CandidateType::Host).unwrap(), "\"host\"");
    assert_eq!(serde_json::to_string(&CandidateType::Srflx).unwrap(), "\"srflx\"");
    assert_eq!(serde_json::to_string(&CandidateType::Relay).unwrap(), "\"relay\"");
}
```

- [ ] **Step 2: Run the test and confirm it fails to compile**

Run: `cargo test -p prdt-signaling-proto`
Expected: FAIL with "cannot find type `Candidate`" etc.

- [ ] **Step 3: Implement Candidate + CandidateType + Role + DoneOutcome + ErrorCode**

`crates/signaling-proto/src/lib.rs`:
```rust
//! Wire types for the power-remote-dt signaling protocol.
//!
//! All messages are UTF-8 JSON over WebSocket Text frames, one message per frame.
//! See `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Candidate {
    pub typ: CandidateType,
    pub ip: String,
    pub port: u16,
    pub priority: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    Host,
    Srflx,
    Relay,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DoneOutcome {
    Connected,
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    HostNotFound,
    HostAlreadyRegistered,
    UnsupportedCandidateType,
    ProtocolError,
    InternalError,
}

/// Default priorities per spec §Wire Protocol.
pub const PRIORITY_HOST: u32 = 100;
pub const PRIORITY_SRFLX: u32 = 50;
pub const PRIORITY_RELAY: u32 = 10;
```

- [ ] **Step 4: Run tests and verify pass**

Run: `cargo test -p prdt-signaling-proto`
Expected: PASS (proptest runs 256 cases by default + 1 literal)

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-proto
git commit -m "signaling-proto: add Candidate, Role, DoneOutcome, ErrorCode types"
```

---

## Task 3: signaling-proto — ClientMessage + ServerMessage + wire fixtures

**Files:**
- Modify: `crates/signaling-proto/src/lib.rs`
- Modify: `crates/signaling-proto/tests/roundtrip.rs`
- Create: `crates/signaling-proto/tests/wire_format.rs`

- [ ] **Step 1: Write failing wire format fixture test**

`crates/signaling-proto/tests/wire_format.rs`:
```rust
use prdt_signaling_proto::*;

/// The JSON literals here MUST match the wire format promised in the spec.
/// If any assertion changes, the wire format has broken — review downstream consumers.

#[test]
fn parse_register() {
    let json = r#"{"t":"register","host_id":"alice-desktop","pubkey_b64":"ZXhhbXBsZQ=="}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Register { host_id, pubkey_b64 } => {
            assert_eq!(host_id, "alice-desktop");
            assert_eq!(pubkey_b64, "ZXhhbXBsZQ==");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_connect() {
    let json = r#"{"t":"connect","host_id":"alice-desktop"}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    assert!(matches!(msg, ClientMessage::Connect { host_id } if host_id == "alice-desktop"));
}

#[test]
fn parse_candidate() {
    let json = r#"{"t":"candidate","session_id":"s1","candidate":{"typ":"host","ip":"127.0.0.1","port":55000,"priority":100}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Candidate { session_id, candidate } => {
            assert_eq!(session_id, "s1");
            assert_eq!(candidate.typ, CandidateType::Host);
            assert_eq!(candidate.ip, "127.0.0.1");
            assert_eq!(candidate.port, 55000);
            assert_eq!(candidate.priority, 100);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_done_connected() {
    let json = r#"{"t":"done","session_id":"s1","outcome":{"t":"connected"}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    assert!(matches!(
        msg,
        ClientMessage::Done { ref session_id, outcome: DoneOutcome::Connected } if session_id == "s1"
    ));
}

#[test]
fn parse_done_failed() {
    let json = r#"{"t":"done","session_id":"s1","outcome":{"t":"failed","reason":"x"}}"#;
    let msg: ClientMessage = serde_json::from_str(json).unwrap();
    match msg {
        ClientMessage::Done { outcome: DoneOutcome::Failed { reason }, .. } => {
            assert_eq!(reason, "x");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_session_start_host() {
    let json = r#"{"t":"session_start","session_id":"s1","role":"host","peer_pubkey_b64":null}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::SessionStart { session_id, role, peer_pubkey_b64 } => {
            assert_eq!(session_id, "s1");
            assert_eq!(role, Role::Host);
            assert_eq!(peer_pubkey_b64, None);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_session_start_viewer() {
    let json = r#"{"t":"session_start","session_id":"s1","role":"viewer","peer_pubkey_b64":"Pa=="}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::SessionStart { role, peer_pubkey_b64, .. } => {
            assert_eq!(role, Role::Viewer);
            assert_eq!(peer_pubkey_b64.as_deref(), Some("Pa=="));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parse_server_error() {
    let json = r#"{"t":"error","code":"host_not_found","message":"no such host"}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::Error { code, message } => {
            assert_eq!(code, ErrorCode::HostNotFound);
            assert_eq!(message, "no such host");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unknown_variant_rejected() {
    let json = r#"{"t":"invented","foo":1}"#;
    let err = serde_json::from_str::<ClientMessage>(json).unwrap_err();
    assert!(err.to_string().contains("invented") || err.is_data());
}
```

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-signaling-proto --test wire_format`
Expected: FAIL (ClientMessage/ServerMessage unknown)

- [ ] **Step 3: Implement ClientMessage and ServerMessage enums**

Append to `crates/signaling-proto/src/lib.rs`:
```rust
/// Messages sent by host or viewer to the signaling server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        host_id: String,
        pubkey_b64: String,
    },
    Connect {
        host_id: String,
    },
    Candidate {
        session_id: String,
        candidate: Candidate,
    },
    Done {
        session_id: String,
        outcome: DoneOutcome,
    },
}

/// Messages sent by the signaling server to host or viewer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMessage {
    Registered {
        host_id: String,
    },
    SessionStart {
        session_id: String,
        role: Role,
        peer_pubkey_b64: Option<String>,
    },
    PeerCandidate {
        session_id: String,
        candidate: Candidate,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}
```

- [ ] **Step 4: Extend roundtrip.rs with ClientMessage + ServerMessage proptest**

Append to `crates/signaling-proto/tests/roundtrip.rs`:
```rust
fn arb_client_message() -> impl Strategy<Value = prdt_signaling_proto::ClientMessage> {
    use prdt_signaling_proto::*;
    prop_oneof![
        ("[a-z]{1,8}", "[A-Za-z0-9+/=]{4,40}")
            .prop_map(|(host_id, pubkey_b64)| ClientMessage::Register { host_id, pubkey_b64 }),
        "[a-z]{1,8}".prop_map(|host_id| ClientMessage::Connect { host_id }),
        ("[a-z0-9]{4,12}", arb_candidate())
            .prop_map(|(session_id, candidate)| ClientMessage::Candidate { session_id, candidate }),
        "[a-z0-9]{4,12}"
            .prop_map(|session_id| ClientMessage::Done { session_id, outcome: DoneOutcome::Connected }),
    ]
}

proptest! {
    #[test]
    fn client_message_roundtrip(m in arb_client_message()) {
        let s = serde_json::to_string(&m).unwrap();
        let back: prdt_signaling_proto::ClientMessage = serde_json::from_str(&s).unwrap();
        prop_assert_eq!(m, back);
    }
}
```

- [ ] **Step 5: Run tests and verify all pass**

Run: `cargo test -p prdt-signaling-proto`
Expected: all wire_format + roundtrip tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/signaling-proto
git commit -m "signaling-proto: add ClientMessage/ServerMessage + wire format fixtures"
```

---

## Task 4: signaling-server — state + /health endpoint

**Files:**
- Create: `crates/signaling-server/src/state.rs`
- Modify: `crates/signaling-server/src/lib.rs`
- Create: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Write failing /health test**

`crates/signaling-server/tests/server_tests.rs`:
```rust
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::sync::Arc;

#[tokio::test]
async fn health_endpoint_returns_counts() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // small yield to let the server come up
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let body = reqwest::get(format!("http://{addr}/health")).await.unwrap().text().await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["hosts"], 0);
    assert_eq!(v["sessions"], 0);
}
```

(`reqwest` を dev-deps に追加:
`crates/signaling-server/Cargo.toml` の `[dev-dependencies]` に:
```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```
)

- [ ] **Step 2: Run and verify compile error**

Run: `cargo test -p prdt-signaling-server --test server_tests -- health_endpoint`
Expected: FAIL (`router`, `ServerConfig`, `ServerState` not found)

- [ ] **Step 3: Implement state.rs**

`crates/signaling-server/src/state.rs`:
```rust
use dashmap::DashMap;
use prdt_signaling_proto::ServerMessage;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

pub type Tx = mpsc::Sender<ServerMessage>;

pub struct HostEntry {
    pub pubkey_b64: String,
    pub tx: Tx,
    pub registered_at: Instant,
}

pub struct SessionEntry {
    pub host_id: String,
    pub host_tx: Tx,
    pub viewer_tx: Tx,
    pub created_at: Instant,
}

#[derive(Default)]
pub struct ServerState {
    pub hosts: DashMap<String, HostEntry>,
    pub sessions: DashMap<String, SessionEntry>,
}

impl ServerState {
    pub fn new() -> Self { Self::default() }

    pub fn counts(&self) -> (usize, usize) {
        (self.hosts.len(), self.sessions.len())
    }
}

pub type SharedState = Arc<ServerState>;
```

- [ ] **Step 4: Implement router() with /health in lib.rs**

`crates/signaling-server/src/lib.rs`:
```rust
pub mod state;
pub mod ws;

use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;
use std::time::Duration;

pub use state::{ServerState, SharedState};

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub session_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { session_timeout: Duration::from_millis(60_000) }
    }
}

pub fn router(state: SharedState, _cfg: ServerConfig) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(state)
}

async fn health(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let (hosts, sessions) = state.counts();
    Json(json!({ "hosts": hosts, "sessions": sessions }))
}
```

`crates/signaling-server/src/ws.rs` (empty for now):
```rust
//! WebSocket handler — wired in later tasks.
```

- [ ] **Step 5: Run and verify test passes**

Run: `cargo test -p prdt-signaling-server --test server_tests health_endpoint`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/signaling-server
git commit -m "signaling-server: add ServerState, /health endpoint, router() scaffolding"
```

---

## Task 5: signaling-server — WS upgrade + Register flow

**Files:**
- Modify: `crates/signaling-server/src/lib.rs`
- Modify: `crates/signaling-server/src/ws.rs`
- Modify: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Write failing test for Register ACK**

Append to `crates/signaling-server/tests/server_tests.rs`:
```rust
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::{ClientMessage, ServerMessage};
use tokio_tungstenite::tungstenite::Message;

async fn start_test_server() -> (std::net::SocketAddr, Arc<ServerState>) {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, state)
}

fn ws_url(addr: std::net::SocketAddr) -> String {
    format!("ws://{addr}/signal")
}

async fn ws_send(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, msg: ClientMessage) {
    let s = serde_json::to_string(&msg).unwrap();
    ws.send(Message::Text(s)).await.unwrap();
}

async fn ws_recv(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> ServerMessage {
    let frame = ws.next().await.unwrap().unwrap();
    let text = match frame {
        Message::Text(t) => t,
        other => panic!("expected Text, got {other:?}"),
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn register_gets_ack() {
    let (addr, state) = start_test_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();

    ws_send(&mut ws, ClientMessage::Register {
        host_id: "h1".into(),
        pubkey_b64: "AAA".into(),
    }).await;

    let msg = ws_recv(&mut ws).await;
    assert!(matches!(msg, ServerMessage::Registered { host_id } if host_id == "h1"));

    // state should have 1 host
    assert_eq!(state.counts().0, 1);
}

#[tokio::test]
async fn duplicate_register_rejected() {
    let (addr, _state) = start_test_server().await;

    let (mut ws1, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws1, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "A".into() }).await;
    let _ = ws_recv(&mut ws1).await;

    let (mut ws2, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws2, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "B".into() }).await;

    let msg = ws_recv(&mut ws2).await;
    match msg {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::HostAlreadyRegistered);
        }
        other => panic!("unexpected: {other:?}"),
    }
}
```

(dev-dep 追加: `futures-util = "0.3"` を `crates/signaling-server/Cargo.toml` `[dev-dependencies]` に)

- [ ] **Step 2: Verify test fails to compile (no /signal route yet)**

Run: `cargo test -p prdt-signaling-server --test server_tests register_gets_ack`
Expected: compile passes but runtime fails — connection to `/signal` gets 404.

- [ ] **Step 3: Add /signal WS route**

`crates/signaling-server/src/lib.rs` の `router()` を更新:
```rust
pub fn router(state: SharedState, cfg: ServerConfig) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/signal", get(ws::handle_upgrade))
        .with_state(AppState { state, cfg })
}

#[derive(Clone)]
pub struct AppState {
    pub state: SharedState,
    pub cfg: ServerConfig,
}
```

そして `health` を `State<AppState>` で取るよう書き換え:
```rust
async fn health(State(app): State<AppState>) -> Json<serde_json::Value> {
    let (hosts, sessions) = app.state.counts();
    Json(json!({ "hosts": hosts, "sessions": sessions }))
}
```

- [ ] **Step 4: Implement ws::handle_upgrade and connection loop with Register + HostAlreadyRegistered**

`crates/signaling-server/src/ws.rs`:
```rust
use crate::state::{HostEntry, SharedState, Tx};
use crate::AppState;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
};
use prdt_signaling_proto::{ClientMessage, ErrorCode, ServerMessage};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, warn};

const SEND_CHAN_CAP: usize = 32;

pub async fn handle_upgrade(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, app))
}

async fn handle_socket(mut socket: WebSocket, app: AppState) {
    let state = app.state.clone();

    // Wait for the first message to classify role.
    let first = match socket.recv().await {
        Some(Ok(Message::Text(t))) => t,
        _ => return,
    };
    let msg: Result<ClientMessage, _> = serde_json::from_str(&first);
    let classified = match msg {
        Ok(m) => m,
        Err(e) => {
            send_error(&mut socket, ErrorCode::ProtocolError, &format!("bad first message: {e}")).await;
            return;
        }
    };

    match classified {
        ClientMessage::Register { host_id, pubkey_b64 } => {
            let (tx, rx) = mpsc::channel::<ServerMessage>(SEND_CHAN_CAP);
            if state.hosts.contains_key(&host_id) {
                send_error(&mut socket, ErrorCode::HostAlreadyRegistered, "host_id already in use").await;
                return;
            }
            state.hosts.insert(host_id.clone(), HostEntry {
                pubkey_b64,
                tx: tx.clone(),
                registered_at: Instant::now(),
            });
            info!(host_id = %host_id, "register");
            send_message(&mut socket, &ServerMessage::Registered { host_id: host_id.clone() }).await;
            host_loop(socket, state, host_id, rx).await;
        }
        _other => {
            // Connect flow / other — implemented in later tasks.
            send_error(&mut socket, ErrorCode::ProtocolError, "not yet implemented").await;
        }
    }
}

async fn host_loop(mut socket: WebSocket, state: SharedState, host_id: String, mut rx: mpsc::Receiver<ServerMessage>) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(_))) => {
                        // Candidate / Done handling in later tasks.
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore binary / ping etc
                    Some(Err(e)) => { warn!(error = %e, "host ws error"); break }
                }
            }
            outbound = rx.recv() => {
                let Some(m) = outbound else { break };
                if send_message(&mut socket, &m).await.is_err() { break; }
            }
        }
    }
    state.hosts.remove(&host_id);
    info!(host_id = %host_id, "host_disconnected");
}

pub(crate) async fn send_message(socket: &mut WebSocket, m: &ServerMessage) -> Result<(), ()> {
    let s = serde_json::to_string(m).map_err(|_| ())?;
    socket.send(Message::Text(s)).await.map_err(|_| ())
}

pub(crate) async fn send_error(socket: &mut WebSocket, code: ErrorCode, message: &str) {
    let _ = send_message(socket, &ServerMessage::Error { code, message: message.into() }).await;
}
```

- [ ] **Step 5: Run tests and verify pass**

Run: `cargo test -p prdt-signaling-server --test server_tests -- register_gets_ack duplicate_register_rejected`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/signaling-server
git commit -m "signaling-server: accept WS Register, ACK with Registered, reject duplicates"
```

---

## Task 6: signaling-server — Connect + session creation

**Files:**
- Modify: `crates/signaling-server/src/ws.rs`
- Modify: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Write failing test for SessionStart fan-out**

Append to `tests/server_tests.rs`:
```rust
#[tokio::test]
async fn connect_triggers_session_start_on_both_sides() {
    let (addr, _) = start_test_server().await;

    // host registers
    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register {
        host_id: "h1".into(),
        pubkey_b64: "PUBKEY".into(),
    }).await;
    let _ = ws_recv(&mut host_ws).await;

    // viewer connects
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;

    let host_start = ws_recv(&mut host_ws).await;
    let viewer_start = ws_recv(&mut viewer_ws).await;

    let (host_sid, viewer_sid) = match (host_start, viewer_start) {
        (
            ServerMessage::SessionStart { session_id: h, role: prdt_signaling_proto::Role::Host, peer_pubkey_b64: None },
            ServerMessage::SessionStart { session_id: v, role: prdt_signaling_proto::Role::Viewer, peer_pubkey_b64: Some(pk) },
        ) => {
            assert_eq!(pk, "PUBKEY");
            (h, v)
        }
        (h, v) => panic!("unexpected fan-out: host={h:?} viewer={v:?}"),
    };
    assert_eq!(host_sid, viewer_sid, "both sides must see the same session_id");
}

#[tokio::test]
async fn connect_unknown_host_returns_error() {
    let (addr, _) = start_test_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut ws, ClientMessage::Connect { host_id: "ghost".into() }).await;
    let msg = ws_recv(&mut ws).await;
    assert!(matches!(msg, ServerMessage::Error { code, .. } if code == prdt_signaling_proto::ErrorCode::HostNotFound));
}
```

- [ ] **Step 2: Run and verify failures**

Run: `cargo test -p prdt-signaling-server --test server_tests connect_`
Expected: FAIL (Connect returns "not yet implemented")

- [ ] **Step 3: Implement Connect branch + session creation in ws.rs**

`crates/signaling-server/src/ws.rs` の `handle_socket` の `_other =>` ブランチを差し替え:
```rust
ClientMessage::Connect { host_id } => {
    let (viewer_tx, viewer_rx) = mpsc::channel::<ServerMessage>(SEND_CHAN_CAP);
    let host_entry = match state.hosts.get(&host_id) {
        Some(e) => e,
        None => {
            send_error(&mut socket, ErrorCode::HostNotFound, "no such host_id").await;
            return;
        }
    };
    let session_id = uuid::Uuid::new_v4().to_string();
    let pubkey_b64 = host_entry.pubkey_b64.clone();
    let host_tx = host_entry.tx.clone();
    drop(host_entry);

    state.sessions.insert(session_id.clone(), crate::state::SessionEntry {
        host_id: host_id.clone(),
        host_tx: host_tx.clone(),
        viewer_tx: viewer_tx.clone(),
        created_at: Instant::now(),
    });
    info!(host_id = %host_id, session_id = %session_id, "connect");

    // Fan out SessionStart.
    let _ = host_tx.send(ServerMessage::SessionStart {
        session_id: session_id.clone(),
        role: prdt_signaling_proto::Role::Host,
        peer_pubkey_b64: None,
    }).await;
    let _ = viewer_tx.send(ServerMessage::SessionStart {
        session_id: session_id.clone(),
        role: prdt_signaling_proto::Role::Viewer,
        peer_pubkey_b64: Some(pubkey_b64),
    }).await;

    viewer_loop(socket, state, session_id, viewer_rx).await;
}
_ => {
    send_error(&mut socket, ErrorCode::ProtocolError, "first message must be register or connect").await;
}
```

追加で `viewer_loop` を定義:
```rust
async fn viewer_loop(mut socket: WebSocket, state: SharedState, session_id: String, mut rx: mpsc::Receiver<ServerMessage>) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(_))) => {
                        // Candidate / Done handling in later tasks.
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { warn!(error = %e, "viewer ws error"); break }
                }
            }
            outbound = rx.recv() => {
                let Some(m) = outbound else { break };
                if send_message(&mut socket, &m).await.is_err() { break; }
            }
        }
    }
    state.sessions.remove(&session_id);
    info!(session_id = %session_id, "viewer_disconnected");
}
```

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-server --test server_tests connect_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-server
git commit -m "signaling-server: handle Connect, create session, fan out SessionStart"
```

---

## Task 7: signaling-server — Candidate forwarding

**Files:**
- Modify: `crates/signaling-server/src/ws.rs`
- Modify: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Write failing test for bidirectional candidate forward**

Append to `tests/server_tests.rs`:
```rust
use prdt_signaling_proto::{Candidate, CandidateType, PRIORITY_HOST};

#[tokio::test]
async fn candidate_forwarded_both_ways() {
    let (addr, _) = start_test_server().await;

    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;

    let h_start = ws_recv(&mut host_ws).await;
    let v_start = ws_recv(&mut viewer_ws).await;
    let sid = match h_start {
        ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };
    let _ = v_start;

    // viewer sends its candidate
    ws_send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 60001, priority: PRIORITY_HOST },
    }).await;

    // host should receive PeerCandidate
    let m = ws_recv(&mut host_ws).await;
    match m {
        ServerMessage::PeerCandidate { session_id, candidate } => {
            assert_eq!(session_id, sid);
            assert_eq!(candidate.port, 60001);
        }
        other => panic!("unexpected: {other:?}"),
    }

    // host sends its candidate
    ws_send(&mut host_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 60002, priority: PRIORITY_HOST },
    }).await;

    let m = ws_recv(&mut viewer_ws).await;
    match m {
        ServerMessage::PeerCandidate { session_id, candidate } => {
            assert_eq!(session_id, sid);
            assert_eq!(candidate.port, 60002);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn non_host_candidate_type_rejected() {
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
        candidate: Candidate { typ: CandidateType::Srflx, ip: "1.2.3.4".into(), port: 1, priority: 50 },
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

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-signaling-server --test server_tests candidate_`
Expected: FAIL (no candidate forwarding yet).

- [ ] **Step 3: Implement candidate handling in both loops**

`crates/signaling-server/src/ws.rs` の `host_loop` と `viewer_loop` 内の `Some(Ok(Message::Text(_)))` を実装に置き換え:

`host_loop` 内:
```rust
Some(Ok(Message::Text(t))) => {
    match serde_json::from_str::<ClientMessage>(&t) {
        Ok(ClientMessage::Candidate { session_id, candidate }) => {
            if candidate.typ != prdt_signaling_proto::CandidateType::Host {
                send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "only host candidates supported in W1").await;
                continue;
            }
            if let Some(sess) = state.sessions.get(&session_id) {
                let _ = sess.viewer_tx.send(ServerMessage::PeerCandidate { session_id: session_id.clone(), candidate }).await;
            }
        }
        Ok(ClientMessage::Done { session_id, .. }) => {
            state.sessions.remove(&session_id);
        }
        Ok(_) => {}
        Err(e) => {
            send_error(&mut socket, ErrorCode::ProtocolError, &format!("{e}")).await;
            break;
        }
    }
}
```

`viewer_loop` 内も対称に(`host_tx` に転送、session 削除時は `session_id.clone()` を local に取ってから `drop(sess)` → `remove`):
```rust
Some(Ok(Message::Text(t))) => {
    match serde_json::from_str::<ClientMessage>(&t) {
        Ok(ClientMessage::Candidate { session_id: sid, candidate }) => {
            if candidate.typ != prdt_signaling_proto::CandidateType::Host {
                send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "only host candidates supported in W1").await;
                continue;
            }
            if let Some(sess) = state.sessions.get(&sid) {
                let _ = sess.host_tx.send(ServerMessage::PeerCandidate { session_id: sid.clone(), candidate }).await;
            }
        }
        Ok(ClientMessage::Done { .. }) => {
            state.sessions.remove(&session_id);
        }
        Ok(_) => {}
        Err(e) => {
            send_error(&mut socket, ErrorCode::ProtocolError, &format!("{e}")).await;
            break;
        }
    }
}
```

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-server --test server_tests candidate_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-server
git commit -m "signaling-server: forward Candidate across session, reject non-host types"
```

---

## Task 8: signaling-server — session inactivity timeout

**Files:**
- Modify: `crates/signaling-server/src/lib.rs`
- Modify: `crates/signaling-server/src/ws.rs`
- Modify: `crates/signaling-server/tests/server_tests.rs`

- [ ] **Step 1: Write failing test with short timeout**

Append to `tests/server_tests.rs`:
```rust
#[tokio::test]
async fn session_timeout_kills_silent_session() {
    let state = Arc::new(ServerState::new());
    let cfg = ServerConfig { session_timeout: std::time::Duration::from_millis(300) };
    let app = router(state.clone(), cfg);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (mut host_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut host_ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() }).await;
    let _ = ws_recv(&mut host_ws).await;

    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url(addr)).await.unwrap();
    ws_send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;
    let _h_start = ws_recv(&mut host_ws).await;
    let _v_start = ws_recv(&mut viewer_ws).await;

    // Don't send anything, wait past the timeout.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    // Either side should now receive an Error(InternalError, "session timeout") before close.
    let err = ws_recv(&mut host_ws).await;
    match err {
        ServerMessage::Error { code, message } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::InternalError);
            assert!(message.contains("timeout"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-signaling-server --test server_tests session_timeout_`
Expected: FAIL (no timeout enforcement yet).

- [ ] **Step 3: Add timeout enforcement**

Update `SessionEntry` and add a spawned task per session. In ws.rs ConnectBranch (after inserting the session), spawn:
```rust
let timeout_state = state.clone();
let timeout_sid = session_id.clone();
let timeout_host_tx = host_tx.clone();
let timeout_viewer_tx = viewer_tx.clone();
let session_timeout = app.cfg.session_timeout;
tokio::spawn(async move {
    tokio::time::sleep(session_timeout).await;
    if timeout_state.sessions.remove(&timeout_sid).is_some() {
        let _ = timeout_host_tx.send(ServerMessage::Error {
            code: ErrorCode::InternalError,
            message: "session timeout".into(),
        }).await;
        let _ = timeout_viewer_tx.send(ServerMessage::Error {
            code: ErrorCode::InternalError,
            message: "session timeout".into(),
        }).await;
        tracing::info!(session_id = %timeout_sid, "session_timeout");
    }
});
```

Note: `handle_socket` now needs the full `app: AppState` (not just `state`). Make sure the signature takes `AppState` and is used for both `state` and `cfg`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-server --test server_tests session_timeout_`
Expected: PASS.

Run full server test suite: `cargo test -p prdt-signaling-server`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-server
git commit -m "signaling-server: enforce session inactivity timeout"
```

---

## Task 9: signaling-server — CLI + binary

**Files:**
- Modify: `crates/signaling-server/src/main.rs`

- [ ] **Step 1: Implement CLI with clap + tracing init**

`crates/signaling-server/src/main.rs`:
```rust
use clap::Parser;
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

#[derive(Parser, Debug)]
#[command(version, about = "power-remote-dt signaling server")]
struct Args {
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    /// Tracing log level.
    #[arg(long, default_value = "info")]
    log: String,
    /// Session inactivity timeout in milliseconds.
    #[arg(long = "session-timeout-ms", default_value_t = 60_000)]
    session_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(args.log.clone())
        .init();

    let state = Arc::new(ServerState::new());
    let cfg = ServerConfig { session_timeout: Duration::from_millis(args.session_timeout_ms) };
    let app = router(state, cfg);

    info!(bind = %args.bind, "server_started");
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 2: Verify builds and CLI works**

Run: `cargo build -p prdt-signaling-server`
Run: `cargo run -p prdt-signaling-server -- --help`
Expected: help text showing `--bind`, `--log`, `--session-timeout-ms`.

- [ ] **Step 3: Commit**

```bash
git add crates/signaling-server/src/main.rs
git commit -m "signaling-server: add CLI (--bind, --log, --session-timeout-ms)"
```

---

## Task 10: signaling-client — types + error + connect helper

**Files:**
- Create: `crates/signaling-client/src/config.rs`
- Create: `crates/signaling-client/src/error.rs`
- Create: `crates/signaling-client/src/rendezvous.rs`
- Modify: `crates/signaling-client/src/lib.rs`

- [ ] **Step 1: Define public types (no test yet — types only)**

`crates/signaling-client/src/config.rs`:
```rust
use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
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
}
```

`crates/signaling-client/src/error.rs`:
```rust
use prdt_signaling_proto::ErrorCode;

#[derive(thiserror::Error, Debug)]
pub enum SignalingError {
    #[error("websocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("server: {code:?} {message}")]
    Server { code: ErrorCode, message: String },
    #[error("timeout waiting for {stage}")]
    Timeout { stage: &'static str },
    #[error("bad candidate: {0}")]
    BadCandidate(String),
    #[error("unexpected message: {0}")]
    Protocol(String),
}
```

`crates/signaling-client/src/rendezvous.rs` (empty skeleton for now):
```rust
use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;

pub async fn rendezvous_as_host(
    _cfg: RendezvousConfig,
    _identity: HostIdentity,
    _local_udp_addr: std::net::SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    unimplemented!("Task 11")
}

pub async fn rendezvous_as_viewer(
    _cfg: RendezvousConfig,
    _local_udp_addr: std::net::SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    unimplemented!("Task 12")
}
```

`crates/signaling-client/src/lib.rs`:
```rust
//! WebSocket client for the power-remote-dt signaling rendezvous.

mod config;
mod error;
mod rendezvous;

pub use config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
pub use error::SignalingError;
pub use rendezvous::{rendezvous_as_host, rendezvous_as_viewer};
```

- [ ] **Step 2: Verify builds**

Run: `cargo build -p prdt-signaling-client`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/signaling-client
git commit -m "signaling-client: add config, error, rendezvous scaffolding"
```

---

## Task 11: signaling-client — rendezvous_as_host happy path

**Files:**
- Modify: `crates/signaling-client/src/rendezvous.rs`
- Create: `crates/signaling-client/tests/mock_host_flow.rs`

**Approach:** Full integration test against a real in-process server (reuses Task 4-9 infrastructure). dev-dep on `prdt-signaling-server` so we can spawn one.

- [ ] **Step 1: Add dev-dep on signaling-server**

Edit `crates/signaling-client/Cargo.toml`:
```toml
[dev-dependencies]
prdt-signaling-server = { path = "../signaling-server" }
tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread", "io-util", "sync", "time"] }
```

- [ ] **Step 2: Write failing happy-path test for host rendezvous**

`crates/signaling-client/tests/mock_host_flow.rs`:
```rust
use prdt_signaling_client::{rendezvous_as_host, HostIdentity, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

async fn start_server() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn host_rendezvous_completes_when_viewer_arrives() {
    let addr = start_server().await;
    let ws_url: Url = format!("ws://{addr}/signal").parse().unwrap();

    let local_udp: SocketAddr = "127.0.0.1:40001".parse().unwrap();
    let host_task = tokio::spawn(async move {
        rendezvous_as_host(
            RendezvousConfig { url: ws_url, host_id: "h1".into(), timeout: Duration::from_secs(5) },
            HostIdentity { pubkey_b64: "HOSTPK".into() },
            local_udp,
        ).await
    });

    // Viewer side as raw WS mock.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ws_url_str = format!("ws://{addr}/signal");
    let (mut viewer_ws, _) = tokio_tungstenite::connect_async(ws_url_str).await.unwrap();
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let send = |ws: &mut _, m: ClientMessage| async move {
        let s = serde_json::to_string(&m).unwrap();
        SinkExt::send(ws, Message::Text(s)).await.unwrap();
    };
    let recv = |ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>| async move {
        let f = ws.next().await.unwrap().unwrap();
        let t = match f { Message::Text(s) => s, o => panic!("{o:?}") };
        serde_json::from_str::<prdt_signaling_proto::ServerMessage>(&t).unwrap()
    };

    send(&mut viewer_ws, ClientMessage::Connect { host_id: "h1".into() }).await;
    let start = recv(&mut viewer_ws).await;
    let sid = match start {
        prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
        _ => unreachable!(),
    };

    // Receive host's candidate
    let pc = recv(&mut viewer_ws).await;
    match pc {
        prdt_signaling_proto::ServerMessage::PeerCandidate { candidate, .. } => {
            assert_eq!(candidate.port, 40001);
        }
        _ => unreachable!(),
    }

    // Viewer replies with its own candidate
    send(&mut viewer_ws, ClientMessage::Candidate {
        session_id: sid.clone(),
        candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40002, priority: PRIORITY_HOST },
    }).await;

    let outcome = host_task.await.unwrap().unwrap();
    assert_eq!(outcome.session_id, sid);
    assert_eq!(outcome.peer_addr.port(), 40002);
    assert_eq!(outcome.peer_addr.ip().to_string(), "127.0.0.1");
    assert_eq!(outcome.peer_pubkey_b64, None);
}
```

- [ ] **Step 3: Run and verify fails**

Run: `cargo test -p prdt-signaling-client --test mock_host_flow`
Expected: FAIL with "unimplemented: Task 11".

- [ ] **Step 4: Implement rendezvous_as_host**

Replace `crates/signaling-client/src/rendezvous.rs`:
```rust
use crate::config::{HostIdentity, RendezvousConfig, RendezvousOutcome};
use crate::error::SignalingError;
use futures_util::{SinkExt, StreamExt};
use prdt_signaling_proto::*;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, instrument};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTERED_TIMEOUT: Duration = Duration::from_secs(5);
const PEER_CANDIDATE_TIMEOUT: Duration = Duration::from_secs(5);

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn candidate_for(local: SocketAddr) -> Candidate {
    Candidate {
        typ: CandidateType::Host,
        ip: local.ip().to_string(),
        port: local.port(),
        priority: PRIORITY_HOST,
    }
}

fn parse_peer_addr(c: &Candidate) -> Result<SocketAddr, SignalingError> {
    format!("{}:{}", c.ip, c.port)
        .parse()
        .map_err(|e| SignalingError::BadCandidate(format!("{e}: {}:{}", c.ip, c.port)))
}

async fn ws_connect(url: &url::Url) -> Result<Ws, SignalingError> {
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()))
        .await
        .map_err(|_| SignalingError::Timeout { stage: "connect" })??;
    Ok(ws)
}

async fn send_msg(ws: &mut Ws, m: &ClientMessage) -> Result<(), SignalingError> {
    let s = serde_json::to_string(m)?;
    ws.send(Message::Text(s)).await?;
    Ok(())
}

async fn recv_msg(ws: &mut Ws, stage: &'static str, dur: Duration) -> Result<ServerMessage, SignalingError> {
    let frame = timeout(dur, ws.next())
        .await
        .map_err(|_| SignalingError::Timeout { stage })?;
    let frame = frame
        .ok_or_else(|| SignalingError::Protocol("connection closed".into()))?
        .map_err(SignalingError::from)?;
    match frame {
        Message::Text(t) => Ok(serde_json::from_str(&t)?),
        other => Err(SignalingError::Protocol(format!("non-text frame: {other:?}"))),
    }
}

#[instrument(skip(cfg, identity), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_host(
    cfg: RendezvousConfig,
    identity: HostIdentity,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(&mut ws, &ClientMessage::Register {
        host_id: cfg.host_id.clone(),
        pubkey_b64: identity.pubkey_b64,
    }).await?;

    match recv_msg(&mut ws, "registered", REGISTERED_TIMEOUT).await? {
        ServerMessage::Registered { .. } => {}
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected Registered, got {other:?}"))),
    }

    let session_id = match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
        ServerMessage::SessionStart { session_id, role: Role::Host, .. } => session_id,
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected SessionStart, got {other:?}"))),
    };
    info!(%session_id, "session_start");

    send_msg(&mut ws, &ClientMessage::Candidate {
        session_id: session_id.clone(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    let peer = match recv_msg(&mut ws, "peer_candidate", PEER_CANDIDATE_TIMEOUT).await? {
        ServerMessage::PeerCandidate { candidate, .. } => {
            if candidate.typ != CandidateType::Host {
                return Err(SignalingError::BadCandidate(format!("unsupported typ {:?}", candidate.typ)));
            }
            parse_peer_addr(&candidate)?
        }
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected PeerCandidate, got {other:?}"))),
    };

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome { session_id, peer_addr: peer, peer_pubkey_b64: None })
}
```

(`futures-util = "0.3"` を `crates/signaling-client/Cargo.toml` [dependencies] に追加)

- [ ] **Step 5: Run and verify pass**

Run: `cargo test -p prdt-signaling-client --test mock_host_flow`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/signaling-client
git commit -m "signaling-client: implement rendezvous_as_host happy path"
```

---

## Task 12: signaling-client — rendezvous_as_viewer happy path

**Files:**
- Modify: `crates/signaling-client/src/rendezvous.rs`
- Create: `crates/signaling-client/tests/mock_viewer_flow.rs`

- [ ] **Step 1: Write failing test**

`crates/signaling-client/tests/mock_viewer_flow.rs`:
```rust
use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage, PRIORITY_HOST};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn viewer_rendezvous_gets_host_addr_and_pubkey() {
    let state = Arc::new(ServerState::new());
    let app = router(state.clone(), ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Host: raw WS that registers and replies with its candidate when session_start arrives.
    let host_task = tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        let send = |ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, m: ClientMessage| async move {
            ws.send(Message::Text(serde_json::to_string(&m).unwrap())).await.unwrap();
        };
        send(&mut ws, ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "HPK".into() }).await;
        // consume Registered
        let _ = ws.next().await.unwrap();
        // wait for SessionStart
        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };
        send(&mut ws, ClientMessage::Candidate {
            session_id: sid.clone(),
            candidate: Candidate { typ: CandidateType::Host, ip: "127.0.0.1".into(), port: 40010, priority: PRIORITY_HOST },
        }).await;
        // Keep the WS alive briefly so the server can deliver.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local_udp: SocketAddr = "127.0.0.1:40011".parse().unwrap();
    let outcome = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(5) },
        local_udp,
    ).await.unwrap();
    host_task.await.unwrap();

    assert_eq!(outcome.peer_addr.port(), 40010);
    assert_eq!(outcome.peer_pubkey_b64.as_deref(), Some("HPK"));
}
```

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-signaling-client --test mock_viewer_flow`
Expected: FAIL (`unimplemented: Task 12`).

- [ ] **Step 3: Implement rendezvous_as_viewer**

Append to `crates/signaling-client/src/rendezvous.rs`:
```rust
#[instrument(skip(cfg), fields(host_id = %cfg.host_id))]
pub async fn rendezvous_as_viewer(
    cfg: RendezvousConfig,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError> {
    let mut ws = ws_connect(&cfg.url).await?;

    send_msg(&mut ws, &ClientMessage::Connect { host_id: cfg.host_id.clone() }).await?;

    let (session_id, peer_pubkey_b64) = match recv_msg(&mut ws, "session_start", cfg.timeout).await? {
        ServerMessage::SessionStart { session_id, role: Role::Viewer, peer_pubkey_b64 } => (session_id, peer_pubkey_b64),
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected SessionStart, got {other:?}"))),
    };
    info!(%session_id, "session_start");

    send_msg(&mut ws, &ClientMessage::Candidate {
        session_id: session_id.clone(),
        candidate: candidate_for(local_udp_addr),
    }).await?;

    let peer = match recv_msg(&mut ws, "peer_candidate", PEER_CANDIDATE_TIMEOUT).await? {
        ServerMessage::PeerCandidate { candidate, .. } => {
            if candidate.typ != CandidateType::Host {
                return Err(SignalingError::BadCandidate(format!("unsupported typ {:?}", candidate.typ)));
            }
            parse_peer_addr(&candidate)?
        }
        ServerMessage::Error { code, message } => return Err(SignalingError::Server { code, message }),
        other => return Err(SignalingError::Protocol(format!("expected PeerCandidate, got {other:?}"))),
    };

    send_msg(&mut ws, &ClientMessage::Done {
        session_id: session_id.clone(),
        outcome: DoneOutcome::Connected,
    }).await?;

    let _ = ws.close(None).await;
    Ok(RendezvousOutcome { session_id, peer_addr: peer, peer_pubkey_b64 })
}
```

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-client --test mock_viewer_flow`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-client
git commit -m "signaling-client: implement rendezvous_as_viewer happy path"
```

---

## Task 13: signaling-client — timeout stage + error mapping tests

**Files:**
- Create: `crates/signaling-client/tests/timeout_stages.rs`
- Create: `crates/signaling-client/tests/error_mapping.rs`

- [ ] **Step 1: Write session_start timeout test**

`crates/signaling-client/tests/timeout_stages.rs`:
```rust
use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig, SignalingError};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

async fn spawn_server() -> SocketAddr {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig { session_timeout: Duration::from_millis(10_000) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn viewer_session_start_timeout() {
    let addr = spawn_server().await;
    // No host registered → viewer will get HostNotFound error, not SessionStart.
    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50001".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "ghost".into(), timeout: Duration::from_millis(300) },
        local,
    ).await.unwrap_err();
    match err {
        SignalingError::Server { code, .. } => {
            assert_eq!(code, prdt_signaling_proto::ErrorCode::HostNotFound);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn viewer_peer_candidate_timeout() {
    let addr = spawn_server().await;
    // Register a host that will just hang (never send a candidate).
    let (mut host_ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    host_ws.send(Message::Text(serde_json::to_string(
        &prdt_signaling_proto::ClientMessage::Register { host_id: "h1".into(), pubkey_b64: "P".into() },
    ).unwrap())).await.unwrap();
    let _ = host_ws.next().await;

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50002".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(1) },
        local,
    ).await.unwrap_err();
    match err {
        SignalingError::Timeout { stage } => assert_eq!(stage, "peer_candidate"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn connect_timeout_when_server_unreachable() {
    // port 1 is reserved; tokio should fail quickly or we bound the connect timeout via the library (5s).
    let url: Url = "ws://127.0.0.1:1/signal".parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50003".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(1) },
        local,
    ).await.unwrap_err();
    // Either WebSocket connect error OR Timeout — both are acceptable signals; in CI we prefer the
    // explicit timeout so the assertion accepts both shapes.
    match err {
        SignalingError::Timeout { stage: "connect" } => {}
        SignalingError::WebSocket(_) => {}
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 2: Run and verify pass (all error paths already implemented in Task 11/12)**

Run: `cargo test -p prdt-signaling-client --test timeout_stages`
Expected: PASS.

- [ ] **Step 3: Write error_mapping.rs for BadCandidate**

`crates/signaling-client/tests/error_mapping.rs`:
```rust
use prdt_signaling_client::{rendezvous_as_viewer, RendezvousConfig, SignalingError};
use prdt_signaling_proto::{Candidate, CandidateType, ClientMessage};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn bad_candidate_parse_error() {
    let state = Arc::new(ServerState::new());
    let app = router(state, ServerConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Host replies with unparseable IP.
    tokio::spawn(async move {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/signal")).await.unwrap();
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Register {
            host_id: "h1".into(), pubkey_b64: "P".into(),
        }).unwrap())).await.unwrap();
        let _ = ws.next().await;
        let start = ws.next().await.unwrap().unwrap();
        let text = match start { Message::Text(t) => t, o => panic!("{o:?}") };
        let m: prdt_signaling_proto::ServerMessage = serde_json::from_str(&text).unwrap();
        let sid = match m {
            prdt_signaling_proto::ServerMessage::SessionStart { session_id, .. } => session_id,
            _ => unreachable!(),
        };
        ws.send(Message::Text(serde_json::to_string(&ClientMessage::Candidate {
            session_id: sid,
            candidate: Candidate { typ: CandidateType::Host, ip: "not-an-ip".into(), port: 1, priority: 100 },
        }).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let url: Url = format!("ws://{addr}/signal").parse().unwrap();
    let local: SocketAddr = "127.0.0.1:50100".parse().unwrap();
    let err = rendezvous_as_viewer(
        RendezvousConfig { url, host_id: "h1".into(), timeout: Duration::from_secs(2) },
        local,
    ).await.unwrap_err();
    match err {
        SignalingError::BadCandidate(msg) => assert!(msg.contains("not-an-ip")),
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-signaling-client --test error_mapping`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/signaling-client
git commit -m "signaling-client: add timeout-stage + error-mapping coverage tests"
```

---

## Task 14: crypto — KnownHosts insert/save + verify_or_record

**Files:**
- Modify: `crates/crypto/src/known_hosts.rs`

- [ ] **Step 1: Write failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/crypto/src/known_hosts.rs`:
```rust
    #[test]
    fn insert_and_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let mut kh = KnownHosts::new();
        let pk = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        kh.insert("alice-desktop".into(), pk.clone());
        kh.save(&path).unwrap();
        let reloaded = KnownHosts::load(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.get("alice-desktop").is_some());
    }

    #[test]
    fn verify_or_record_first_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let pk = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let verdict = KnownHosts::verify_or_record(&path, "alice-desktop", &pk).unwrap();
        assert!(matches!(verdict, TofuVerdict::FirstSeen));
        let reloaded = KnownHosts::load(&path).unwrap();
        assert!(reloaded.get("alice-desktop").is_some());
    }

    #[test]
    fn verify_or_record_matched_and_mismatched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let pk1 = PubKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let pk2 = PubKey::from_base64("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB").unwrap();
        let _ = KnownHosts::verify_or_record(&path, "alice-desktop", &pk1).unwrap();
        let v = KnownHosts::verify_or_record(&path, "alice-desktop", &pk1).unwrap();
        assert!(matches!(v, TofuVerdict::Matched));
        let v = KnownHosts::verify_or_record(&path, "alice-desktop", &pk2).unwrap();
        assert!(matches!(v, TofuVerdict::Mismatch { .. }));
    }
```

`tempfile = "3"` を `crates/crypto/Cargo.toml` の `[dev-dependencies]` に追加。

- [ ] **Step 2: Run and verify fails**

Run: `cargo test -p prdt-crypto`
Expected: FAIL (missing `insert`, `save`, `verify_or_record`, `TofuVerdict`).

- [ ] **Step 3: Implement insert, save, verify_or_record + TofuVerdict**

Append to `crates/crypto/src/known_hosts.rs` (inside `impl KnownHosts` and add the verdict type at module scope):
```rust
pub enum TofuVerdict {
    FirstSeen,
    Matched,
    Mismatch { expected: PubKey, got: PubKey },
}

impl KnownHosts {
    pub fn insert(&mut self, host_key: String, pubkey: PubKey) {
        self.entries.insert(host_key, pubkey);
    }

    /// Serialize to the same plaintext format `parse` accepts.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), KnownHostsError> {
        let mut content = String::new();
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        for k in keys {
            let pk = &self.entries[k];
            content.push_str(k);
            content.push(' ');
            content.push_str(&pk.to_base64());
            content.push('\n');
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    /// TOFU: create-if-missing, verify-if-present. Records on first sight.
    pub fn verify_or_record<P: AsRef<Path>>(
        path: P,
        host_key: &str,
        pubkey: &PubKey,
    ) -> Result<TofuVerdict, KnownHostsError> {
        let path = path.as_ref();
        let mut kh = if path.exists() {
            Self::load(path)?
        } else {
            Self::new()
        };
        let verdict = match kh.get(host_key) {
            None => {
                kh.insert(host_key.to_string(), pubkey.clone());
                kh.save(path)?;
                TofuVerdict::FirstSeen
            }
            Some(existing) if existing == pubkey => TofuVerdict::Matched,
            Some(existing) => TofuVerdict::Mismatch {
                expected: existing.clone(),
                got: pubkey.clone(),
            },
        };
        Ok(verdict)
    }
}
```

Note: `PubKey::to_base64()` and `PubKey: Clone + PartialEq` are required. Confirm from `keypair.rs`; if missing, add:
```rust
// in crates/crypto/src/keypair.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubKey(/* existing */);

impl PubKey {
    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(self.as_bytes())
    }
}
```
(Check existing API first; if `to_base64` already exists, skip.)

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p prdt-crypto`
Expected: all known_hosts tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/crypto
git commit -m "crypto: add KnownHosts::insert/save and verify_or_record (TOFU) helper"
```

---

## Task 15: host bin — CLI flags + rendezvous integration

**Files:**
- Modify: `crates/host/Cargo.toml`
- Modify: `crates/host/src/main.rs`

- [ ] **Step 1: Add dependencies**

Edit `crates/host/Cargo.toml` `[dependencies]` section, add:
```toml
prdt-signaling-client = { path = "../signaling-client" }
prdt-signaling-proto = { path = "../signaling-proto" }
url = { workspace = true }
base64 = { workspace = true }
```

- [ ] **Step 2: Add CLI flags to host Args**

Find the clap-derive `Args` struct in `crates/host/src/main.rs` (near the top). Add:
```rust
    /// Rendezvous via a signaling server instead of listening directly.
    #[arg(long)]
    signaling_url: Option<url::Url>,

    /// Opaque host identifier registered with the signaling server.
    /// Required when --signaling-url is specified.
    #[arg(long, required_unless_present = "listen")]
    host_id: Option<String>,

    /// Rendezvous overall timeout in seconds.
    #[arg(long, default_value_t = 10)]
    signaling_timeout: u64,
```

- [ ] **Step 3: Insert rendezvous branch before `host_handshake`**

Find the site where the code calls `CustomUdpTransport::bind(...)` and subsequently `host_handshake(...)`. Replace with (pseudo-diff — exact symbol names depend on existing main.rs; adapt as needed):
```rust
let transport = CustomUdpTransport::bind(bind_addr, udp_cfg).await?;
let local = transport.local_addr()?;

if let Some(url) = args.signaling_url.clone() {
    let host_id = args.host_id.clone().expect("clap required_unless_present=listen");
    let pubkey_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(static_key.public_bytes())
    };
    let outcome = prdt_signaling_client::rendezvous_as_host(
        prdt_signaling_client::RendezvousConfig {
            url,
            host_id: host_id.clone(),
            timeout: std::time::Duration::from_secs(args.signaling_timeout),
        },
        prdt_signaling_client::HostIdentity { pubkey_b64 },
        local,
    ).await?;
    tracing::info!(peer_addr = %outcome.peer_addr, session_id = %outcome.session_id, "signaling_rendezvous_completed");
    transport.configure_peer(outcome.peer_addr).await;
} else if args.listen.is_some() {
    // existing fixed-address path (unchanged)
}

host_handshake(&transport, &static_key).await?;
```

If the existing `Args` does not have a `listen` field by that name, use whatever the current one is (e.g. `--bind` or `--viewer-addr`) and adjust `required_unless_present` accordingly.

If `PubKey` of the Noise static key is exposed via a method other than `public_bytes()`, match the existing name in `crates/crypto/src/keypair.rs`.

- [ ] **Step 4: Build and make sure the binary compiles**

Run: `cargo build -p prdt-host`
Expected: clean build.

- [ ] **Step 5: Verify CLI help shows new flags**

Run: `cargo run -p prdt-host -- --help`
Expected: output includes `--signaling-url`, `--host-id`, `--signaling-timeout`.

- [ ] **Step 6: Commit**

```bash
git add crates/host
git commit -m "host: add --signaling-url / --host-id CLI; rendezvous when provided"
```

---

## Task 16: viewer bin — CLI flags + rendezvous + TOFU integration

**Files:**
- Modify: `crates/viewer/Cargo.toml`
- Modify: `crates/viewer/src/main.rs`

- [ ] **Step 1: Add dependencies**

Edit `crates/viewer/Cargo.toml` `[dependencies]`:
```toml
prdt-signaling-client = { path = "../signaling-client" }
prdt-signaling-proto = { path = "../signaling-proto" }
url = { workspace = true }
```

- [ ] **Step 2: Add CLI flags + known-host-ids path**

Find the viewer's clap `Args`. Add:
```rust
    /// Rendezvous via a signaling server instead of direct host address.
    #[arg(long)]
    signaling_url: Option<url::Url>,

    /// Opaque host identifier to look up in the signaling server.
    #[arg(long, required_unless_present = "host_addr")]
    host_id: Option<String>,

    /// Rendezvous overall timeout in seconds.
    #[arg(long, default_value_t = 10)]
    signaling_timeout: u64,

    /// Path to the host_id-indexed known-hosts file.
    #[arg(long, default_value = "known-host-ids")]
    known_host_ids: std::path::PathBuf,

    /// Proceed even when TOFU pubkey mismatches.
    #[arg(long)]
    force_tofu: bool,
```

- [ ] **Step 3: Resolve peer_addr via rendezvous + TOFU check**

Replace the site where viewer sets up `CustomUdpTransport::configure_peer(...)`:
```rust
let transport = CustomUdpTransport::bind(bind_addr, udp_cfg).await?;
let local = transport.local_addr()?;

let peer_addr = if let Some(url) = args.signaling_url.clone() {
    let host_id = args.host_id.clone().expect("clap required_unless_present=host_addr");
    let outcome = prdt_signaling_client::rendezvous_as_viewer(
        prdt_signaling_client::RendezvousConfig {
            url,
            host_id: host_id.clone(),
            timeout: std::time::Duration::from_secs(args.signaling_timeout),
        },
        local,
    ).await?;
    tracing::info!(peer_addr = %outcome.peer_addr, session_id = %outcome.session_id, "signaling_rendezvous_completed");

    if let Some(pk_b64) = outcome.peer_pubkey_b64.as_deref() {
        let pk = prdt_crypto::PubKey::from_base64(pk_b64)
            .map_err(|e| anyhow::anyhow!("bad host pubkey from signaling: {e}"))?;
        match prdt_crypto::KnownHosts::verify_or_record(&args.known_host_ids, &host_id, &pk)? {
            prdt_crypto::TofuVerdict::FirstSeen => {
                tracing::info!(%host_id, "tofu_first_seen: recorded host pubkey");
            }
            prdt_crypto::TofuVerdict::Matched => {
                tracing::info!(%host_id, "tofu_matched");
            }
            prdt_crypto::TofuVerdict::Mismatch { .. } if args.force_tofu => {
                tracing::warn!(%host_id, "tofu_mismatch forced-through by --force-tofu");
            }
            prdt_crypto::TofuVerdict::Mismatch { .. } => {
                anyhow::bail!("TOFU pubkey mismatch for host_id={host_id}. Refusing to connect. Pass --force-tofu to override (NOT RECOMMENDED).");
            }
        }
    }

    outcome.peer_addr
} else {
    args.host_addr.expect("either --signaling-url or --host-addr required")
};

transport.configure_peer(peer_addr).await;
viewer_handshake(&transport, &known_hosts).await?;
```

- [ ] **Step 4: Build + CLI help**

Run: `cargo build -p prdt-viewer`
Run: `cargo run -p prdt-viewer -- --help`
Expected: new flags visible.

- [ ] **Step 5: Commit**

```bash
git add crates/viewer
git commit -m "viewer: add --signaling-url / --host-id / --force-tofu; rendezvous + TOFU"
```

---

## Task 17: W1 smoke integration test — server + both rendezvous + Noise handshake

**Files:**
- Create: `crates/signaling-client/tests/w1_smoke.rs`
- Modify: `crates/signaling-client/Cargo.toml` (dev-deps)

- [ ] **Step 1: Add dev-deps needed to exercise transport + crypto**

Edit `crates/signaling-client/Cargo.toml`:
```toml
[dev-dependencies]
prdt-signaling-server = { path = "../signaling-server" }
prdt-transport = { path = "../transport" }
prdt-crypto = { path = "../crypto" }
prdt-protocol = { path = "../protocol" }
tokio = { workspace = true, features = ["test-util", "macros", "rt-multi-thread", "io-util", "sync", "time"] }
base64 = { workspace = true }
```

- [ ] **Step 2: Write the end-to-end smoke test**

`crates/signaling-client/tests/w1_smoke.rs`:

```rust
//! End-to-end: in-process signaling-server + rendezvous_as_host + rendezvous_as_viewer +
//! CustomUdpTransport Noise handshake. Must complete within 15s.
//!
//! This test locks in the Phase 2 W1 exit criterion: same-machine LAN loopback works
//! through signaling.

use prdt_crypto::{KeyPair, KnownHosts};
use prdt_signaling_client::{rendezvous_as_host, rendezvous_as_viewer, HostIdentity, RendezvousConfig};
use prdt_signaling_server::{router, ServerConfig, ServerState};
use prdt_transport::{CustomUdpTransport, UdpTransportConfig};
use prdt_transport::handshake::{host_handshake, viewer_handshake};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

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
async fn w1_smoke_noise_handshake_completes() {
    let signaling_url = spawn_signaling().await;

    // Generate a fresh keypair for this test.
    let host_kp = KeyPair::generate();
    let host_pub_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(host_kp.public_bytes())
    };

    let host_url = signaling_url.clone();
    let host_fut = async move {
        let transport = CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default()).await.unwrap();
        let local = transport.local_addr().unwrap();
        let outcome = rendezvous_as_host(
            RendezvousConfig { url: host_url, host_id: "w1-test".into(), timeout: Duration::from_secs(5) },
            HostIdentity { pubkey_b64: host_pub_b64.clone() },
            local,
        ).await.unwrap();
        transport.configure_peer(outcome.peer_addr).await;
        host_handshake(&transport, &host_kp).await.unwrap();
    };

    let viewer_url = signaling_url.clone();
    let host_kp_pub_for_viewer = host_kp.public_bytes();
    let viewer_fut = async move {
        // Small head-start so the host future can Register before we Connect.
        // The server returns HostNotFound immediately if the host isn't registered yet.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let transport = CustomUdpTransport::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap(), UdpTransportConfig::default()).await.unwrap();
        let local = transport.local_addr().unwrap();
        let outcome = rendezvous_as_viewer(
            RendezvousConfig { url: viewer_url, host_id: "w1-test".into(), timeout: Duration::from_secs(5) },
            local,
        ).await.unwrap();
        transport.configure_peer(outcome.peer_addr).await;

        // TOFU happens in bin; here we synthesize a trusted KnownHosts.
        let mut kh = KnownHosts::new();
        kh.insert("w1-test".into(), prdt_crypto::PubKey::from_bytes(&host_kp_pub_for_viewer));
        viewer_handshake(&transport, &kh).await.unwrap();
    };

    let overall = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(host_fut, viewer_fut)
    }).await.expect("w1 smoke must complete within 15s");
    let _ = overall; // both tasks ok'd via unwrap above
}
```

Notes for the implementer:
- The exact symbol `KeyPair::generate()`, `KeyPair::public_bytes()`, `host_handshake(transport, keypair)`, `viewer_handshake(transport, known_hosts)`, `PubKey::from_bytes`, `UdpTransportConfig::default()` may differ in the existing code. Find the current names in `crates/crypto/src/keypair.rs`, `crates/transport/src/handshake.rs`, `crates/transport/src/udp.rs` and adapt.
- If `UdpTransportConfig::default()` is not `Clone + Default`, construct it with whatever minimal config existing smoke tests use.

- [ ] **Step 3: Run the smoke test**

Run: `cargo test -p prdt-signaling-client --test w1_smoke`
Expected: PASS within 15s.

- [ ] **Step 4: Commit**

```bash
git add crates/signaling-client
git commit -m "signaling-client: add W1 end-to-end smoke (server + rendezvous + Noise)"
```

---

## Task 18: Regression, clippy, manual smoke, tag

- [ ] **Step 1: Full workspace test pass (LAN regression)**

Run: `cargo test --workspace`
Expected: all previously green tests still green; new signaling tests also green.

If anything regresses: the signaling flags must be fully optional, so check `host`/`viewer` Args defaults and ensure LAN-only smoke behaves exactly as before.

- [ ] **Step 2: clippy clean**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Fix any findings inline. Common leftovers: unused imports in tests, `let _ =` noise, `unused_must_use` on `send()` / `close()` calls.

- [ ] **Step 3: Manual 3-terminal smoke**

Terminal 1:
```bash
cargo run -p prdt-signaling-server --release -- --bind 127.0.0.1:8080 --log debug
```

Terminal 2 (host):
```bash
cargo run -p prdt-host --release -- --signaling-url ws://127.0.0.1:8080/signal --host-id w1-manual
```

Terminal 3 (viewer):
```bash
cargo run -p prdt-viewer --release -- --signaling-url ws://127.0.0.1:8080/signal --host-id w1-manual
```

Expected: viewer window opens and host screen is visible within 2-3 seconds of viewer launch. Close viewer with Esc / window close.

Confirm the signaling-server log shows `register { host_id: "w1-manual" }` → `connect` → `candidate_forwarded` (both directions) → `session_completed`.

- [ ] **Step 4: Update project memory (completed phases)**

Edit `C:\Users\nakan\.claude\projects\E--project-rust-desktop-power-remote-dt\memory\project_overview.md`. In the "Completed phases (git tags)" list append:
```markdown
- `phase2-w1-complete` — signaling-proto/client/server + host_id TOFU; LAN loopback via signaling → Noise → video end-to-end
```
And update "Remaining (not done)" to strike W1 from the Phase 2 line (e.g. "Phase 2 W2〜W6" instead of "Phase 2").

- [ ] **Step 5: Tag and commit**

```bash
git add C:/Users/nakan/.claude/projects/E--project-rust-desktop-power-remote-dt/memory/project_overview.md 2>/dev/null || true
git commit -am "phase2-w1: tag milestone" --allow-empty
git tag phase2-w1-complete
```

---

## Self-Review checklist (reviewer: the agent executing this plan)

After implementing all tasks, verify the plan against the spec:

- [ ] All 8 Exit Criteria in `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md` are met
- [ ] Existing LAN smoke (`--host-addr` fixed path) still passes without any signaling flags
- [ ] `crates/signaling-client/tests/w1_smoke.rs` completes in < 15s
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] git log shows clean commit-per-task history
- [ ] `phase2-w1-complete` tag points to the last commit that completes the plan
