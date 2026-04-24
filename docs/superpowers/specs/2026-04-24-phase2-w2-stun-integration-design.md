# Phase 2 W2: STUN Integration — Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W2 — STUN public-addr learning
**Date**: 2026-04-24
**Status**: Draft (awaits user spec review, then writing-plans)
**Prereq**: W1 `phase2-w1-complete` merged to master

---

## Summary

W2 の目的: host と viewer の起動時に **STUN で自機の public IP:port(srflx candidate)を学習**し、W1 の signaling 経路に流して相手に届ける。W1 の `Host` candidate と並列に `Srflx` candidate が流れる経路を作るだけで、**candidate 選択ロジックは W3 まで触らない**。

W2 完了後の状態:
- host/viewer は `--stun-url` 指定時に STUN binding request を送って public addr を取得
- signaling を介して Host + Srflx の 2 種類の candidate を交換
- signaling-server は `UnsupportedCandidateType` を Srflx について出さなくなる
- 受信側は複数 PeerCandidate を収集できるようになり、**Host 型が届いた時点で即 peer_addr 確定**(= W1 と同じ動作)
- Srflx は届いていることを ログで確認できるが、まだ使われない(W3 の hole-punching で使用開始)

つまり W2 単独では **現状の LAN ループバック動作が壊れないこと** と **srflx candidate が signaling 経由で正しく流れること** を示すのが合格条件。

---

## Scope

### In-scope

- 新クレート `nat-traversal` — STUN binding request クライアント
- `stun_codec` 0.3 + `bytecodec` + 自前 tokio client(~50 行)
- `signaling-client::rendezvous_as_{host,viewer}` の拡張:
  - `RendezvousConfig` に `stun_url: Option<Url>` を追加
  - STUN 成功時に Host + Srflx の 2 candidate を signaling に送る
  - 受信: 複数 PeerCandidate を受け取り、list に貯めるが **Host が来た時点で即 return**
- `signaling-server` の修正: `UnsupportedCandidateType` 拒否を撤廃、srflx も中継
- host / viewer bin に `--stun-url <URL>` オプション追加(例: `stun://stun.l.google.com:19302`)、未指定時は STUN スキップ
- in-process モック STUN サーバを使う単体/結合テスト
- W1 の LAN ループバック smoke が無改造で pass することを確認

### Out (W3 以降)

- **candidate 選択ロジックの変更**(ICE-lite 的な host>srflx>relay + first-success)
- **hole punching**(両端が相互に UDP を投げ合って NAT に穴を開ける)
- **TURN リレー**
- **外部 STUN サーバへの実接続を含む CI テスト**(モックで十分、実接続は manual smoke 範囲)
- **NAT behavior discovery (RFC 5780)** — W2 ではやらない

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| STUN crate | `stun_codec = "0.3"` + `bytecodec = "0.4"` + 自前 tokio binding client | `stunclient` は sync、`rustun` は過剰、stun_codec+bytecodec はメッセージ codec の薄い層として最小依存 |
| STUN 取得属性 | `XOR-MAPPED-ADDRESS` のみ(fallback `MAPPED-ADDRESS` 不要) | RFC 5389/8489 準拠の公共サーバは全て XOR を返す、fallback は W5 で追加可能 |
| タイムアウト/リトライ | 1 回送信、3 秒タイムアウト、失敗時は srflx 無し | シンプル、UDP パケ欠ならユーザーがリトライ |
| Default STUN URL | 指定なし(opt-in) | LAN 用途で不要な外部依存を作らない。`--stun-url <URL>` で明示的に enable |
| Public STUN サーバ(推奨値) | `stun://stun.l.google.com:19302`(ドキュメント記載のみ) | 無認証、24/7 稼働、RFC 準拠 |
| signaling-server: srflx | 条件なしで中継 | W1 の特別扱いを解除するだけ、wire schema 不変 |
| signaling-client: receive | 複数 PeerCandidate を list に貯める、Host が来たら即 peer_addr 採用して return | W1 と同動作を維持、Srflx を storage に残すだけ |
| 選択ロジック W3 対応 | `RendezvousOutcome` に `peer_candidates: Vec<Candidate>` を追加 | W3 で selection を変える時に signaling-client を触らずに済む |
| テスト | in-process モック STUN サーバ | 外部依存なしで決定論的。stun_codec を使って 30 行で書ける |
| Regression gate | W1 smoke(`--stun-url` なし)が無改造で pass | 既存ユーザーの LAN 動作を壊さない |

---

## Architecture

### Crate 構成

```
crates/
  nat-traversal/           # 新規。STUN client (+ 将来の TURN client を足す余地)
    src/lib.rs
    src/stun.rs            # learn_public_addr(bound_udp_socket, stun_server_addr) -> Result<SocketAddr>
    tests/mock_server.rs   # in-process STUN server で roundtrip 検証
```

### 依存向き

```
nat-traversal     ← signaling-client (new dep)
       ↑
signaling-client  ← host / viewer
       ↑
signaling-proto
```

`nat-traversal` は `signaling-proto` を知らない。`signaling-client` が STUN 結果を Srflx candidate に変換する。

### 追加 workspace 依存

| crate | 用途 | 影響先 |
|---|---|---|
| `stun_codec = "0.3"` | STUN メッセージの encode/decode | `nat-traversal` |
| `bytecodec = "0.4"` | stun_codec の基盤 | `nat-traversal` (推移) |

---

## STUN Client (`nat-traversal::stun`)

### 公開 API

```rust
use std::net::SocketAddr;
use std::time::Duration;

#[derive(thiserror::Error, Debug)]
pub enum StunError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout waiting for STUN response")]
    Timeout,
    #[error("decode: {0}")]
    Decode(String),
    #[error("no XOR-MAPPED-ADDRESS attribute")]
    NoMappedAddress,
}

/// Send a STUN Binding Request on the provided UDP socket, return the
/// public (IP:port) the STUN server observed.
///
/// `server_addr` is the STUN server's UDP endpoint (e.g.
/// "stun.l.google.com:19302" → resolve → SocketAddr).
///
/// The socket MUST already be bound. Do NOT call `connect()` on it — the
/// client is share-compatible so the caller can keep using it for other
/// traffic (we send to server, receive from server, filter by transaction ID).
pub async fn learn_public_addr(
    socket: &tokio::net::UdpSocket,
    server_addr: SocketAddr,
    timeout: Duration,
) -> Result<SocketAddr, StunError>;
```

### 実装ノート

- STUN Binding Request: 20 バイトヘッダ、magic cookie `0x2112A442`、transaction ID 96 bits(random)
- 受信ループ: UDP からパケット読む → stun_codec でデコード → transaction ID 一致確認 → `XOR-MAPPED-ADDRESS` 属性抽出
- 1 回送信、無応答時は `timeout` 後に `StunError::Timeout`
- サーバが複数回応答した場合は最初だけ採用
- URL パース: `stun://host:port` 形式を受け、`:port` 省略時は 3478(STUN default)

### モック STUN サーバ(テスト用)

`nat-traversal/tests/mock_server.rs` に ~50 行のモック実装:
- tokio の UdpSocket を bind
- Binding Request を受信したら XOR-MAPPED-ADDRESS 付きの Response を即返す
- テストでは `learn_public_addr(&client_socket, mock_addr, 1s)` を呼んで公開 addr が返ってくることを assert

### 実 STUN サーバに対する手動確認

CI テストは mock のみ。`stun.l.google.com:19302` への実接続は手動スモーク時に確認。

---

## Signaling Proto — Schema Changes

**無改変**。Wire types (`Candidate`, `CandidateType::{Host, Srflx, Relay}`, `ServerMessage::PeerCandidate`) は W1 で既に Srflx を含む形で定義済み。`PRIORITY_SRFLX = 50` 定数も存在。

---

## Signaling Server Changes

### 変更 1 箇所: `UnsupportedCandidateType` 判定を撤廃

現状 `ws.rs` の `host_loop` / `viewer_loop` で:
```rust
if candidate.typ != CandidateType::Host {
    send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "only host candidates supported in W1").await;
    continue;
}
```

W2 で:
```rust
// W2: accept Host and Srflx. Relay still rejected (W4).
if candidate.typ == CandidateType::Relay {
    send_error(&mut socket, ErrorCode::UnsupportedCandidateType, "relay candidates require W4 TURN").await;
    continue;
}
```

### 影響するテスト

- `non_host_candidate_type_rejected` (Task 7) は現在 Srflx を投げて Error を期待 → **テストを書き換え**:
  - Srflx は中継されて PeerCandidate が飛んでくることを assert
  - Relay を投げると今でも Error が返ることを assert(新テスト)

---

## Signaling Client Changes

### 1. `RendezvousConfig` に `stun_url` 追加

```rust
pub struct RendezvousConfig {
    pub url: Url,
    pub host_id: String,
    pub timeout: Duration,
    pub stun_url: Option<Url>,   // NEW; None = STUN 無効
}
```

### 2. `RendezvousOutcome` を拡張

```rust
pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_addr: SocketAddr,
    pub peer_pubkey_b64: Option<String>,
    pub peer_candidates: Vec<Candidate>, // NEW; 受信した全 candidate(順不同)
}
```

W3 はこの `peer_candidates` を見て selection ロジックを書く。W2 段階で peer_addr は「最初に届いた Host」のまま。

### 3. rendezvous_as_{host,viewer} 内部フロー

```
bind UDP socket (既存)
local_addr() (既存)

if let Some(stun) = cfg.stun_url {
    resolve DNS → SocketAddr
    srflx = learn_public_addr(&socket, stun_addr, 3s).await.ok()   // 失敗は無視
} else {
    srflx = None
}

(signaling handshake 既存: Register/Connect → SessionStart)

send Candidate { Host, local_addr }                     // 必ず送る
if let Some(a) = srflx {
    send Candidate { Srflx, a, priority=50 }            // 学習できたら送る
}

// 受信 loop:
peer_candidates = vec![];
loop with timeout = PEER_CANDIDATE_TIMEOUT {
    recv PeerCandidate -> push to peer_candidates
    if any Host typ seen in peer_candidates {
        peer_addr = first Host candidate
        break
    }
}

send Done { Connected }
return RendezvousOutcome { peer_addr, peer_candidates, ... }
```

重要:
- **送信は 2 メッセージ**(Host + 学習できれば Srflx)、trickle 順序は Host → Srflx
- **受信は時間ベース**: `PEER_CANDIDATE_TIMEOUT` (5 秒) までに Host 型が来れば即 exit、来なければエラー
- **srflx のみで Host 無し**のケース: W2 ではエラー扱い(`BadCandidate` or `Protocol`)。W3 で srflx-only fallback に対応

### 4. `nat-traversal` への依存追加

`signaling-client/Cargo.toml` に `prdt-nat-traversal = { path = "../nat-traversal" }` を追加。

---

## Host / Viewer Bin Changes

### CLI 追加(両 bin 共通)

```
--stun-url <URL>            STUN server, e.g. stun://stun.l.google.com:19302 [optional]
```

### 動作

- `--signaling-url` 未指定 → W1 と完全同一、STUN 不呼出
- `--signaling-url` 指定 + `--stun-url` 未指定 → W1 と同動作(Host candidate のみ)
- `--signaling-url` 指定 + `--stun-url` 指定 → rendezvous 内で STUN 学習 → Host + Srflx の 2 candidate 送信

URL パース: `url::Url` で `stun://` スキームを parse、host+port 抽出。スキームが異なる場合は起動時エラー。

---

## Testing Strategy

### 1) `nat-traversal` 単体

- `mock_server.rs` が in-process で STUN サーバを動かし、`learn_public_addr` が正しく XOR-MAPPED-ADDRESS を取得することを assert
- タイムアウト動作: 応答しないサーバ相手で 500ms に設定し `StunError::Timeout` が返ることを assert
- transaction ID ミスマッチ: 違う ID のレスポンスを返すモック、client はそれを無視して timeout することを assert
- 無効なレスポンス(magic cookie 不正など)で `StunError::Decode` が返ることを assert

### 2) `signaling-server` 修正に伴うテスト更新

- `non_host_candidate_type_rejected` を分割:
  - `srflx_candidate_forwarded`: viewer が Srflx を送ると host 側に PeerCandidate として届く(Relay は削除)
  - `relay_candidate_still_rejected`: Relay を送ると `UnsupportedCandidateType` が返る

### 3) `signaling-client` 拡張テスト

- `w2_mock_host_flow`: rendezvous_as_host が Host + Srflx の 2 candidate を送ることを raw WS mock で受信して assert
- `w2_peer_candidates_collected`: rendezvous_as_viewer が複数 PeerCandidate(Host + Srflx)を受信し、peer_candidates に両方含まれ、peer_addr は Host のものになることを assert

### 4) W2 E2E smoke

`crates/signaling-client/tests/w2_smoke.rs`:
- mock STUN server + signaling-server + rendezvous_as_host/viewer(両方 stun_url 指定)
- Noise handshake + Hello/HelloAck まで成立すること
- 両端の `RendezvousOutcome.peer_candidates.len() == 2`(Host + Srflx)を assert

### 5) 既存 W1 smoke は無改変で pass

`w1_smoke.rs` は `stun_url: None` で W1 と同じ動作。regression gate。

### 6) 手動スモーク(real STUN)

`docs/superpowers/plans/2026-04-24-phase2-w2-manual-smoke-TODO.md` に 3-terminal 手順:
- signaling server 起動
- host: `--stun-url stun://stun.l.google.com:19302 --bind 0.0.0.0:9000 ...`
- viewer: 同じ `--stun-url` 指定
- ログに `srflx learned public_addr=X.Y.Z.W:PORT` が両端に出ることを確認
- W1 同様 60 FPS で接続成立すること

---

## Exit Criteria

- [ ] `nat-traversal` 単体テスト 4 件(happy, timeout, txn_id_mismatch, bad_response)全 pass
- [ ] `signaling-server` テスト(W1 7 件 + W2 2 件 = 9 件)全 pass
- [ ] `signaling-client` テスト(W1 7 件 + W2 2 件 + w2_smoke = 10 件)全 pass
- [ ] W1 smoke が無改造で pass(regression gate)
- [ ] host/viewer bin が `--stun-url` を受け付け、未指定時 W1 相当、指定時 2 candidate を送ることをログで確認
- [ ] 手動スモーク: `stun.l.google.com:19302` 経由で public addr 学習、signaling 経由で交換、W1 同等の 60 FPS 通信が成立
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean(non-media-win crates)
- [ ] git tag `phase2-w2-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `stun_codec` 0.3 の API が想定と違う | 実装遅延 | 実装時に最小コード書いて先行検証、厳しければ自前で msg 組み立て(20バイト header + attrs) |
| STUN モックが本番サーバと挙動乖離 | 手動スモークで顕在化 | モックは RFC 5389 準拠で書く、Google STUN との差分を手動スモークで確認 |
| 既存 `non_host_candidate_type_rejected` 消すと W1 仕様を壊す | 既存ユーザーが srflx/relay を投げてきた時の挙動が変わる | Relay は引き続き拒否、Srflx は W2 で正式サポートに昇格(メッセージ化) |
| rendezvous_as_* の受信ロジック変更で W1 smoke が赤 | regression | CI で W1 smoke を先に流す、両方同時にパスするまでマージしない |
| host candidate が先に届かないケース(Srflx が先、その後ネット遅延で Host) | W2 時点では存在しないが W3 で問題化 | W2 では peer_candidates リストに両方保持するので W3 で順序不問の選択が書ける |
| opt-in にしたせいで将来 STUN on-by-default に移行する時の互換性 | minor | `--stun-url` を受け付ける設計を保ちつつ、W5 でデフォルト `stun.l.google.com` に切り替えればよい |

---

## Open Questions (W2 実装中に決めてよい)

- `stun://` URL パース — port 省略時の default(3478 vs サーバの実ポート)
- `tracing` span 名 — `nat_traversal::stun::learn_public_addr { server = %server_addr }` で揃える
- `learn_public_addr` を失敗時 Result::Err で返すか、Ok(None) 形式で srflx 学習を optional にするか — 当面は Err 返して呼び出し側が `.ok()` で握りつぶす設計

---

## References

- W1 spec: `docs/superpowers/specs/2026-04-23-phase2-w1-signaling-skeleton-design.md`
- Phase 2 全体: `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md`
- `stun_codec`: https://docs.rs/stun_codec/
- STUN RFC 5389: https://datatracker.ietf.org/doc/html/rfc5389
- W1 implementation: crates/signaling-{proto,client,server}, merge commit `5bf8dfd`
