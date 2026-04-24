# Phase 2 W5: 9-digit ID System — Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W5 — server-allocated persistent 9-digit host IDs + pubkey pinning
**Date**: 2026-04-24
**Status**: Draft (W4 `phase2-w4-complete` merged to master)

---

## Summary

現状 `host_id` はユーザー任意文字列("alice-desktop" 等)。W5 では **signaling-server が SQLite に永続化された 9 桁数字 ID を採番** し、`pubkey_b64` とペアで管理する。host は初回に空 `host_id` で register → サーバが採番 → `host-id.txt` に保存。2 回目以降は既存 ID + pubkey 送信で検証、pubkey 一致時のみ成功。不一致は新 `ErrorCode::HostIdPubkeyMismatch` で拒否。

到達目標: Parsec/Moonlight 風に「viewer が 9 桁 ID を入力するだけ」で接続でき、サーバが ID↔pubkey 対応を管理する。

---

## Scope

### In-scope
- signaling-proto: `ErrorCode::HostIdPubkeyMismatch` 追加(wire 互換性破壊、project は internal なので許容)
- signaling-server: `rusqlite` 0.31(bundled)依存追加、`hosts(host_id, pubkey_b64, registered_at)` テーブル
- signaling-server: `Register` 処理を更新 — 空 host_id は採番、既存 host_id は pubkey 検証
- signaling-server CLI: `--db <PATH>` オプション(default `prdt-signaling.sqlite`)
- signaling-client: `rendezvous_as_host` の戻り値が server 採番 ID を伝えるよう変更
- host bin: `host-id.txt` を `host-key.bin` と同階層に作成(空送信 → 採番後 書き込み)、`--host-id-file <PATH>` で上書き可
- viewer bin: `--host-id` 入力時にダッシュ除去正規化
- W5 smoke テスト: 空 register → 採番 → 再 register → 同 ID 返却 → pubkey 不一致で Mismatch

### Out (W6+)
- Phase 5 PostgreSQL 移行
- ID のプライバシー保護機能(レート制限、匿名モード等)
- ID 変更機能、ID 削除機能
- 複数 TURN / 多リージョン対応

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| DB クレート | `rusqlite = { version = "0.31", features = ["bundled"] }` | Windows で SQLite DLL 同梱が不要 |
| スキーマ | `hosts(host_id TEXT PRIMARY KEY, pubkey_b64 TEXT NOT NULL, registered_at INTEGER NOT NULL)` | 最小、W6 拡張で karma/rate 列追加可 |
| ID 形式(wire) | `"123-456-789"` 文字列 | 人間可読、normalize は server 側 |
| ID 形式(DB) | ダッシュ無し 9 桁 `"123456789"` | クエリ時 trim |
| 採番 | 9 桁 random u32 (range 100_000_000..=999_999_999)、衝突時 5 回 retry | space 10^9 で sparse |
| host_id 空文字の wire セマンティクス | 「採番希望」 | schema 変更不要 |
| 再 register | 既存 host_id + pubkey 送信、pubkey 一致なら OK、不一致なら `HostIdPubkeyMismatch` | key 紛失時は別の ID を割り当て直す運用 |
| host-id.txt | `host-key.bin` と同階層、空 register 後 writethrough | 鍵と ID をセットで管理 |
| viewer input | `--host-id 123-456-789` OR `--host-id 123456789` 両方受理 | UX |
| TOFU known-host-ids | 引き続き使用(server 補強) | 信頼チェーン 2 本立て |

---

## Architecture

### signaling-server state 変更

`ServerState` に `HostStore` を追加:
```rust
pub struct HostStore {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl HostStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> { /* open + CREATE TABLE IF NOT EXISTS */ }
    pub fn allocate_or_verify(&self, host_id: Option<&str>, pubkey_b64: &str) -> Result<String, StoreError>;
}
```

- `allocate_or_verify(None, pubkey)` → 新規 ID 採番 + insert → Ok(new_id)
- `allocate_or_verify(Some(id), pubkey)` → 既存行検索 → pubkey 一致: Ok(id) / 不一致: Err(Mismatch) / 未登録: 新規 insert(初回登録として扱う。衝突無し前提、サーバ再起動で DB 紛失等のケースで有効)
- 採番時衝突(rusqlite UNIQUE 違反)は 5 回まで retry

### Register 処理フロー

```
Client → Server: Register { host_id: "", pubkey_b64: "AAA" }
Server: store.allocate_or_verify(None, "AAA") → "123456789"
Server → Client: Registered { host_id: "123-456-789" }
```

```
Client → Server: Register { host_id: "123-456-789", pubkey_b64: "AAA" }
Server: store.allocate_or_verify(Some("123456789"), "AAA") → Ok("123456789")
Server → Client: Registered { host_id: "123-456-789" }
```

```
Client → Server: Register { host_id: "123-456-789", pubkey_b64: "BBB" }
Server: store.allocate_or_verify → Err(Mismatch)
Server → Client: Error { code: HostIdPubkeyMismatch, message: "..." }
```

### signaling-client 変更

`rendezvous_as_host` は既に `Registered { host_id }` を受信 → そのまま戻り値に含める(`RendezvousOutcome` に `host_id: Option<String>` 追加、あるいは既存を流用)。

host 側呼び出し(bin)が `outcome.host_id` を取得して `host-id.txt` に書き込む。

### host/viewer CLI

- host: `--host-id-file <PATH>`(default `host-id.txt`)。起動時 read、無ければ空送信、成功後 write。既存 `--host-id <ID>` は廃止(自動管理)。
- viewer: `--host-id 123-456-789` または `--host-id 123456789`。入力を正規化してから signaling に送る。

---

## Wire Protocol Changes

### `ErrorCode::HostIdPubkeyMismatch` 追加

`signaling-proto/src/lib.rs` の `ErrorCode` enum に variant 追加:
```rust
pub enum ErrorCode {
    HostNotFound,
    HostAlreadyRegistered,
    UnsupportedCandidateType,
    ProtocolError,
    InternalError,
    HostIdPubkeyMismatch,  // NEW
}
```

### Register host_id 空文字

wire 変更なし(既に `String` だった)。server 側のセマンティクスのみ変更。client は empty string を渡すか `Option<String>` を空に扱うかの選択 — 既存の型を保つため empty String として扱う。

---

## Testing Strategy

### 1. signaling-server unit

- `HostStore::allocate_or_verify` に対する unit tests:
  - new ID allocate → readback
  - existing ID + same pubkey → OK
  - existing ID + different pubkey → Mismatch
  - empty DB file (first open) → CREATE TABLE works

### 2. signaling-server integration

- `register_allocates_new_id_when_empty` (`tests/server_tests.rs`): empty host_id の register → Registered に 9 桁 ID
- `register_reuses_existing_id`: 同じ pubkey で再 register → 同じ ID
- `register_rejects_pubkey_mismatch`: 同じ ID / 別 pubkey → `HostIdPubkeyMismatch`

### 3. signaling-client

- RendezvousOutcome に allocated host_id を返すテスト(mock_host_flow 拡張)

### 4. host bin

- `host-id.txt` 読み書きは bin 内部ロジックで unit tests は追加しない(W1/W2/W3 smoke が実質カバー)

### 5. W5 smoke

`crates/signaling-client/tests/w5_smoke.rs`:
- in-process signaling-server(tmp sqlite DB)
- host 1: empty register → 9 桁 ID 受領
- host 1 exit → host 2 を同 pubkey で起動 → 既存 ID で register 成功
- host 3 を同 ID + 別 pubkey で register → Mismatch エラー

### 6. W1-W4 smoke regression

既存テストで `RendezvousConfig { host_id: "w1-smoke" }` 等が使われている。w1-w4 smoke は「任意文字列」を受け付けるか、「empty → 採番」に変更するか?

**方針**: 既存 smoke は `host_id: "non-empty-string"` を使い続ける。サーバが「未登録の non-empty string」を受けた場合、**新規登録として insert** する(即席の ID として機能)。これで既存 smoke は無変更で動く。

---

## Exit Criteria

- [ ] `rusqlite` 依存追加、HostStore 実装 + unit tests
- [ ] signaling-server が Register 採番 / 再登録検証 / Mismatch 拒否
- [ ] CLI `--db` option 動作
- [ ] host bin が `host-id.txt` で ID 永続化
- [ ] viewer bin が `--host-id` ダッシュ正規化
- [ ] W5 smoke: 採番 → 再登録 → mismatch の 3 シナリオ
- [ ] W1-W4 smoke regression 全 pass
- [ ] clippy clean
- [ ] `phase2-w5-complete` タグ

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| rusqlite bundled feature のビルドが Windows で失敗 | blocker | 代替: `sqlite-loadable` or `libsqlite3-sys` の feature フラグ手動指定 |
| 既存 smoke が opaque string の host_id を使う → 新規登録として insert | 挙動変化(意図的) | ドキュメント明記、smoke 自体は通る |
| 9 桁採番の衝突率 | 10^9 space で低い、5 回 retry で実質ゼロ | リトライ追加でカバー |
| server 再起動 + sqlite 破損で既存 host_id が消える | 客は再登録 | 初期スコープでは受容、バックアップは運用者責任 |
| pubkey_b64 文字列比較のエッジケース(改行・whitespace) | 誤判定 | normalize(trim)してから比較 |

---

## Open Questions (実装中に決めてよい)

- DB migration 方式(W5 時点は initial schema only、将来カラム追加時に migration 戦略必要)
- `host-id.txt` の atomicity(tempfile + rename vs 直接 write) — 直接 write で簡素化
- ID 採番の擬似乱数シード — `rand_core::OsRng` を直接使う

---

## References

- W4 spec: `docs/superpowers/specs/2026-04-24-phase2-w4-turn-design.md`
- Phase 2 全体: `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md`
- `rusqlite`: https://docs.rs/rusqlite/
