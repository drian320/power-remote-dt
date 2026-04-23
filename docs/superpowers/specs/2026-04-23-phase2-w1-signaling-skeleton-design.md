# Phase 2 W1: Signaling Protocol Skeleton — Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W1 — signaling protocol skeleton
**Date**: 2026-04-23
**Status**: Draft (implementation plan 未作成)
**Prereq**: Phase 0〜3 完了、Phase 2 全体設計書 `2026-04-23-phase2-wan-nat-design.md`

---

## Summary

Phase 2 全体設計書で定義された W1 の詳細設計。

W1 の目的: **signaling サーバ / クライアントのプロトコル骨組みを作り、LAN 同一マシン上の 2 プロセス(host + viewer)が signaling 経由でお互いの UDP アドレスを学習し、既存の Noise_NK ハンドシェイク + 映像パイプラインを走らせるところまで通す。**

STUN / TURN / NAT 越えは W2 以降。W1 は純粋に「signaling メッセージの往復と transport への受け渡し」を検証する。

---

## Scope

### In-scope

- 新クレート 3 本: `signaling-proto`、`signaling-client`、`signaling-server`
- JSON over WebSocket のワイヤ型定義と serde 実装
- axum 0.7 ベースの最小 server(WS upgrade + in-memory state + `/health`)
- `tokio-tungstenite` ベースの client と `rendezvous_as_host` / `rendezvous_as_viewer` 関数
- host / viewer binary に `--signaling-url` / `--host-id` / `--signaling-timeout` フラグ追加
- W1 合格条件: **同一マシンで host + viewer が signaling 経由で Noise ハンドシェイク成立、映像 1 フレーム表示**

### Out-of-scope(W2 以降)

- STUN による public addr 学習(W2)
- hole punching による NAT 越え(W3)
- TURN リレー(W4)
- 9 桁数字 ID の永続化 / サーバ採番 / SQLite(W5)
- rendezvous 失敗時の `--host-addr` 自動フォールバック
- 映像パイプライン自体の変更(既存 Phase 0〜3 コードは無改変)
- signaling 経由の複数 viewer 同時接続(W1 では host が 1 セッション受理後 exit)
- WebRTC / SDP 互換性

---

## Decisions (brainstorming 合意)

| 決定事項 | 採用 | 理由 |
|---|---|---|
| W1 の合格線 | LAN ループバックで Noise まで成立 | 「JSON 疎通のみ」だと transport ハンドオフの検証にならず W2/W3 で詰まる |
| Server スタック | axum 0.7 + tokio-tungstenite | W5 で REST / metrics / SQLite を足すとき axum に載せておくと素直 |
| host_id | 任意 opaque string (`--host-id alice-desktop`) | W5 で 9 桁数字に置換する際、opaque key を差し替えるだけで済む |
| Candidate 列挙 | 単一 `local_addr()` を `Host` typ で送信、schema は array 前提 | W1 は同一マシン成立が条件、srflx/relay は W2/W3 で variant 追加 |
| Schema 互換 | W1 最小 variant + `Srflx`/`Relay` 先置き、未知は strict エラー | W1 で unknown 握りつぶしよりバグ検知を優先、type は先置きして先々の wire break 回避 |
| Client 形態 | **Rendezvous 関数型**(`async fn` が一発で `SocketAddr` を返す) | main 差分が最小(10〜20 行)、cancel-safe、W4 で handle 型に昇格するリファクタは局所的 |
| LAN バイパス | `--signaling-url` 未指定なら既存 `--host-addr`/`--listen` パス | 既存 smoke テストを完全互換に保つ |
| Rendezvous 失敗時 | fallback せず non-zero exit | 自動フォールバックは W3 の TURN 相当、W1 では明示失敗が診断しやすい |

---

## Architecture

### Crate 構成

```
crates/
  signaling-proto/     # 型定義のみ (serde)。host/viewer/server すべてが依存。
  signaling-client/    # WS 接続 + rendezvous 関数。host/viewer が依存。
  signaling-server/    # 公式リレー実装(bin crate)。
```

### 依存向き

```
signaling-proto    ← signaling-client
        ↑               ↑
        └─ signaling-server
                        ↑
                        └── (独立 bin、他 crate に依存されない)

host / viewer  ──→  signaling-client, signaling-proto
                   (transport crate は signaling を知らない)
```

**crate 境界の不変条件**: `transport` crate は signaling を一切知らない。signaling は `CustomUdpTransport::configure_peer(SocketAddr)`(既存 API、udp.rs:92)を利用側から呼ばれるだけで接続する。

### 追加 workspace 依存

| crate | 用途 | 影響先 |
|---|---|---|
| `tokio-tungstenite = "0.24"` | WS client | `signaling-client` |
| `axum = { version = "0.7", features = ["ws"] }` | WS server + REST 余地 | `signaling-server` |
| `serde_json = "1"` | JSON wire format | `signaling-proto`, client, server |
| `tokio-stream = "0.1"` | axum WS stream 取り回し | `signaling-server` |
| `uuid = { version = "1", features = ["v4"] }` | session_id 発行 | `signaling-server` |
| `dashmap = "6"` | server in-memory state | `signaling-server` |
| `url = "2"` | `RendezvousConfig::url` | `signaling-client` |
| `base64 = "0.22"` | pubkey の base64 エンコード | `signaling-client` + host + viewer |

---

## Wire Protocol (`signaling-proto`)

### 型定義

```rust
// Wire messages — serde tag "t"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMessage {
    Register {
        host_id: String,
        pubkey_b64: String,      // Noise static key base64
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMessage {
    Registered {
        host_id: String,
    },
    SessionStart {
        session_id: String,
        role: Role,
        peer_pubkey_b64: Option<String>, // viewer のみ Some
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub typ: CandidateType,
    pub ip: String,
    pub port: u16,
    pub priority: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType { Host, Srflx, Relay }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role { Host, Viewer }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DoneOutcome {
    Connected,
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    HostNotFound,
    HostAlreadyRegistered,
    UnsupportedCandidateType,
    ProtocolError,
    InternalError,
}
```

### Wire format 契約

- すべて UTF-8 JSON、WebSocket の Text フレームとして 1 メッセージ 1 フレーム
- 未知 variant は strict エラー(`Error(ProtocolError)` を server から、client 側は `SignalingError::Json` で exit)
- W1 時点で発行される typ は `Host` のみ。`Srflx`/`Relay` を受信した server は `UnsupportedCandidateType` を返す
- `priority` は W1 では未使用(W2 で選択ロジックに入る前置き)。W1 は定数 `host=100, srflx=50, relay=10` を送信

### Regression fixture

`signaling-proto/tests/wire_format.rs` に設計書 §Signaling Protocol の JSON 例文字列を literal として埋め、parse → expected enum を assert。wire break の直接検知。

---

## Signaling Server (`signaling-server`)

### State(in-memory のみ)

```rust
struct ServerState {
    hosts: DashMap<String, HostEntry>,
    sessions: DashMap<String, SessionEntry>,
}

struct HostEntry {
    pubkey_b64: String,
    tx: mpsc::Sender<ServerMessage>,
    registered_at: Instant,
}

struct SessionEntry {
    host_id: String,
    host_tx: mpsc::Sender<ServerMessage>,
    viewer_tx: mpsc::Sender<ServerMessage>,
    created_at: Instant,
}
```

永続化なし。restart で全 host が再 register する前提(W5 で SQLite)。

### Endpoint

- `GET /signal` — axum `WebSocketUpgrade`。最初のメッセージ(`Register` or `Connect`)で役割決定
- `GET /health` — `200 OK`、JSON `{"hosts": N, "sessions": M}`

### 接続ライフサイクル

```
新規 WS connect
  ├── 最初のメッセージ受信
  │     Register ──→ hosts.insert → Registered 返す → Host モード(常駐)
  │     Connect  ──→ host 検索
  │                   ├── 見つかった → session 生成(uuid)
  │                   │     ├── host_tx に SessionStart(Host)
  │                   │     ├── viewer_tx に SessionStart(Viewer, peer_pubkey)
  │                   │     └── Viewer モード
  │                   └── 未登録 → Error(HostNotFound) → close
  │     他       ──→ Error(ProtocolError) → close
  │
  ├── Host モード recv
  │     Candidate{session_id, c} → session 検索 → viewer_tx に PeerCandidate 転送
  │     Done{session_id, .}      → sessions.remove(session_id)、WS は継続
  │     切断                       → hosts.remove + 関連 session 全削除
  │
  └── Viewer モード recv
        Candidate{session_id, c} → host_tx に PeerCandidate 転送
        Done{session_id, .}      → sessions.remove、WS 切断
        切断                       → sessions.remove
```

### Timeout

| 対象 | 時間 | 失敗時 |
|---|---|---|
| Session inactivity | 60s | 両端に `Error(InternalError, "session timeout")` + WS close |
| Host registration idle | 30min | host 側 WS close(静かに) |
| WS ping/pong | axum default (30s) | tungstenite 側が切断扱い |

テスト時は server 側 timeout を CLI `--session-timeout-ms` で短縮可能(production default 60000ms、テストは 500ms)。

### CLI

```
prdt-signaling-server [OPTIONS]
  --bind <ADDR>                listen address [default: 127.0.0.1:8080]
  --log <LEVEL>                tracing level [default: info]
  --session-timeout-ms <MS>    session inactivity [default: 60000]
```

### ログ(tracing info レベル)

- `server_started { bind }`
- `register { host_id }` / `register_rejected { host_id, reason }`
- `connect { host_id, session_id }` / `connect_rejected { host_id, code }`
- `candidate_forwarded { session_id, direction }`
- `session_completed { session_id, took_ms }`
- `session_timeout { session_id }`
- `host_disconnected { host_id }`

---

## Signaling Client (`signaling-client`)

### 公開 API

```rust
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,       // default 10s
}

pub struct HostIdentity {
    pub pubkey_b64: String,
}

pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_addr: SocketAddr,
    pub peer_pubkey_b64: Option<String>,  // viewer のみ Some
}

pub async fn rendezvous_as_host(
    cfg: RendezvousConfig,
    identity: HostIdentity,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError>;

pub async fn rendezvous_as_viewer(
    cfg: RendezvousConfig,
    local_udp_addr: SocketAddr,
) -> Result<RendezvousOutcome, SignalingError>;

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

### Host フロー

```
1. WS connect                                        timeout 5s → connect
2. send Register { host_id, pubkey_b64 }
3. recv Registered                                   timeout 5s → registered
4. recv SessionStart { session_id, Host, None }      timeout cfg.timeout → session_start
5. send Candidate { session_id, Host(local_udp_addr, priority=100) }
6. recv PeerCandidate                                timeout 5s → peer_candidate
     ├── candidate.typ == Host → 採用
     └── それ以外 → Error Server(UnsupportedCandidateType) 相当を返して exit
7. send Done { session_id, Connected }
8. return RendezvousOutcome { peer_addr: parsed, peer_pubkey_b64: None }
```

W1 では host も 1 セッションのみ受理して `rendezvous_as_host` は return する。常駐ループは W5 で追加。

### Viewer フロー

```
1. WS connect                                        timeout 5s
2. send Connect { host_id }
3. recv SessionStart { session_id, Viewer, Some(pk) } timeout cfg.timeout
4. send Candidate { session_id, Host(local_udp_addr, priority=100) }
5. recv PeerCandidate                                timeout 5s
6. send Done { session_id, Connected }
7. return RendezvousOutcome { peer_addr, peer_pubkey_b64: Some(pk) }
```

### Cancel safety

`rendezvous_*` は future drop 時に WS を閉じれば整合が取れる設計(内部 state は local 変数のみ)。呼び出し側 `tokio::select!` で race させても問題ない。

### Tracing

- span `signaling_client::rendezvous { role, host_id }`
- 成立時: `rendezvous_completed { session_id, peer_addr, took_ms }`
- 失敗時: `rendezvous_failed { error, stage }`

---

## Host / Viewer Integration

### CLI 追加フラグ(両 bin 共通)

```
--signaling-url <URL>       ws://... または wss://... [optional]
--host-id <ID>              opaque string [required if --signaling-url が指定]
--signaling-timeout <SECS>  [default: 10]
--force-tofu                known-hosts 不一致を警告のみで続行(debug)
```

### 優先順位

- `--signaling-url` が指定 → signaling 経路、既存 `--listen` / `--host-addr` は warn 付きで無視
- 未指定 → 既存 LAN fixed 経路(Phase 0〜3 完全互換)

### Host bin 差分

```rust
let transport = CustomUdpTransport::bind(bind_addr, udp_cfg).await?;
let local = transport.local_addr()?;

if let Some(url) = args.signaling_url {
    let outcome = signaling_client::rendezvous_as_host(
        RendezvousConfig { url, host_id: args.host_id.clone(), timeout },
        HostIdentity { pubkey_b64: base64_encode(static_key.public()) },
        local,
    ).await?;
    transport.configure_peer(outcome.peer_addr).await;
}
// 以降 Noise handshake + 既存パイプライン unchanged
```

`bind_addr` は `--signaling-url` 指定時 default `127.0.0.1:0`(ローカルテスト)、`--bind` で override。

### Viewer bin 差分

```rust
let transport = CustomUdpTransport::bind(bind_addr, udp_cfg).await?;
let local = transport.local_addr()?;

let peer_addr = if let Some(url) = args.signaling_url {
    let outcome = signaling_client::rendezvous_as_viewer(
        RendezvousConfig { url, host_id: args.host_id.clone(), timeout },
        local,
    ).await?;
    if let Some(pk) = outcome.peer_pubkey_b64 {
        tofu_verify_or_record(&known_hosts, &args.host_id, &pk, args.force_tofu)?;
    }
    outcome.peer_addr
} else {
    args.host_addr.expect("either --signaling-url or --host-addr required")
};

transport.configure_peer(peer_addr).await;
// 以降 viewer_handshake + 既存パイプライン unchanged
```

### known-hosts 変更

- 既存 schema: `addr -> pubkey`
- W1 追加: `host_id -> pubkey`(signaling 経路用、別 section もしくは別 key)
- 初回接続: signaling の `peer_pubkey_b64` を `host_id` key で書き込み
- 2 回目以降: 値一致を確認、不一致は `TofuMismatchError` で exit(`--force-tofu` で警告のみ)
- LAN 固定経路は既存 `addr -> pubkey` のまま温存

既存 crypto crate に追加: `KnownHosts::verify_or_record_by_host_id(host_id, pubkey) -> Result<(), TofuMismatch>`

---

## Testing Strategy

### 単体 / 結合テスト

1. **`signaling-proto`**
   - 全 variant の serde JSON roundtrip(unit + proptest)
   - wire format regression fixture(設計書の JSON literal を parse して assert)

2. **`signaling-server`** (同一プロセスで `TcpListener::bind("127.0.0.1:0")` 起動)
   - `register_then_connect_full_session`
   - `host_not_found`
   - `host_already_registered`
   - `session_timeout` (`--session-timeout-ms 500` 相当)
   - `candidate_type_unsupported`(Srflx を送りつける)
   - `viewer_disconnect_during_session`(WS 強制切断 → host 側に PeerCandidate が来ない)

3. **`signaling-client`**
   - `tokio::io::duplex` ベースのモック server に対して state machine を各 stage で検証
   - 各 timeout stage で `SignalingError::Timeout{stage}` が正しいラベルで返る
   - 不正 candidate(parse できない `ip`)で `BadCandidate` が返る

### Workspace 統合テスト

`crates/signaling-client/tests/w1_smoke.rs`(既存の `crates/transport/tests/encrypted_test.rs` パターンを拡張):

- 同プロセスで `signaling-server` を `tokio::spawn`(`TcpListener::bind("127.0.0.1:0")` → `local_addr()` を `ws://` URL に組み立て)
- host 側: `CustomUdpTransport::bind` → `rendezvous_as_host` → `configure_peer` → `host_handshake` を tokio task で起動
- viewer 側: 同様に `rendezvous_as_viewer` → `configure_peer` → `viewer_handshake` を並行起動
- `tokio::join!` で両端を回し、15 秒以内に Noise handshake 成立 + 最初の Hello / HelloAck 往復を assert
- 失敗時は `tracing_subscriber` 出力を `target/test-logs/w1_smoke.{server,host,viewer}.log` にリダイレクト

dev-dependencies で `signaling-server`、`transport`、`crypto`、`protocol` を取り込む(host/viewer bin の main は通さない ― main logic は W5 で常駐ループ化する際に別途 bin-level テストが必要になる)。

### 手動スモーク(合格判定)

3 ターミナル手順:

```bash
# term 1
cargo run -p signaling-server -- --bind 127.0.0.1:8080

# term 2 (host)
cargo run -p host -- --signaling-url ws://127.0.0.1:8080/signal --host-id w1-test

# term 3 (viewer)
cargo run -p viewer -- --signaling-url ws://127.0.0.1:8080/signal --host-id w1-test
```

期待: viewer に host 画面が映る(既存 Phase 0 smoke と同等、経路だけが signaling)。

### LAN 互換性 regression

- `--signaling-url` を **指定しない** 既存 smoke test 一式が回帰ゼロで pass
- 既存の `cargo test --workspace` が緑のまま

---

## Exit Criteria

- [ ] `signaling-proto` 全 variant の serde roundtrip + wire fixture テスト 全 pass
- [ ] `signaling-server` の 6 結合テスト 全 pass
- [ ] `signaling-client` の state machine / timeout stage テスト 全 pass
- [ ] `crates/signaling-client/tests/w1_smoke.rs` が 15s 以内に CI 上で pass
- [ ] 手動スモーク: 同一マシン LAN ループバックで映像表示を 1 回確認
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `--signaling-url` 未指定時の既存 LAN 経路テストが regression ゼロ
- [ ] git tag `phase2-w1-complete` を打つ

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| tokio-tungstenite と axum 0.7 の WS 実装差 | subtle bug | client / server 両方を同一 runtime で結合テスト |
| `configure_peer` 呼び出しタイミング(bind 後・handshake 前) | race、peer_addr 未設定で handshake 開始 | rendezvous 関数が return してから handshake 呼び出すことを host/viewer main で enforce |
| known-hosts schema 変更で既存ユーザーのファイルを壊す | LAN 経路が動かなくなる | 既存 `addr->pubkey` section は温存、`host_id->pubkey` は別 section に追加 |
| session_timeout のテスト flakiness | CI で偽失敗 | `--session-timeout-ms` で 500ms に短縮、asserion は 2〜3 倍の余裕を持つ |
| axum 依存増加でビルド時間増 | dev 体験劣化 | `signaling-server` は別 bin crate なので host/viewer のビルドに影響なし |

---

## Open Questions (W1 実装中に決めてよい)

- `base64` の encoding: URL-safe (b64url) vs 標準 — `pubkey_b64` は JSON 内なので標準で OK とする(既存 `known-hosts.json` と揃える)
- `signaling-server` の `--log` 形式: JSON (Phase 5 集約前提) vs pretty — W1 は pretty、Phase 5 で JSON 切替
- tracing span attribute の命名規約: 既存 host/viewer のスタイルに合わせるか別名前空間にするか

---

## References

- Phase 2 全体設計: `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md`
- 既存 transport API: `crates/transport/src/udp.rs:92` (`configure_peer`)
- 既存 crypto crate: `crates/crypto/`(known-hosts、Noise_NK)
- 既存 smoke pattern: `crates/host/tests/` の memory-transport テスト
