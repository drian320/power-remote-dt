# Phase 2 W3: Hole Punching + Candidate Selection — Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W3 — hole punching + candidate selection (uses Srflx learned in W2)
**Date**: 2026-04-24
**Status**: Draft (W2 `phase2-w2-complete` merged to master)

---

## Summary

W1 で signaling skeleton、W2 で Srflx candidate の学習 + signaling 経路での交換が完了。peer_addr は「最初に届いた Host」にコミットしていた。

W3 の目的: **peer_candidates(Host + Srflx 混在)から「両端で実際に UDP が通る組」を動的に選ぶ** ことで、同一 LAN と NAT 越えの両方で自動的に最適経路を確立する。合成の難所は:
- 両端が同時に送受信する必要がある(NAT hole punching の前提)
- W2 の残制約「STUN probe が transport socket と違う port」を解消(srflx が実 port を指すように)
- Noise handshake より前の段階に新しいフェーズを挟む

到達目標:
- 同一 LAN: Host candidate が勝って即成立(現状 W1/W2 と同じ挙動)
- 片側 NAT: 相手の Srflx candidate が勝つ
- 両側 NAT(Full Cone / Restricted Cone): Srflx で成立 ~80%
- 両側 Symmetric NAT: 失敗(W4 TURN で救済)

W3 単独で「実 2 台別ネットワーク接続が 10 秒以内に成立」を目指す。

---

## Scope

### In-scope

- `ControlMessage::Probe { nonce: [u8; 16] }` / `ProbeAck { nonce: [u8; 16] }` の追加
- `CustomUdpTransport::socket() -> Arc<UdpSocket>` の公開
- `CustomUdpTransport::probe_and_commit_peer(candidates, timeout) -> Result<SocketAddr>` — 並行 probe + ACK で winner を commit
- `nat-traversal::stun::learn_public_addr` の socket 引数を `&Arc<UdpSocket>` 等に広げ、transport socket で STUN 可能に
- `signaling-client::rendezvous_as_{host,viewer}` の「最初に届いた Host を peer_addr にコミット」ロジックを撤去、peer_candidates を返すのみ(RendezvousOutcome.peer_addr は廃止 or deprecated)
- `host/viewer bin` のオーケストレーション:rendezvous → STUN(共有 socket) → probe_and_commit_peer → Noise
- W3 E2E smoke(mock STUN + signaling + probe + Noise、unreachable Host + reachable Srflx のテスト)
- W1/W2 smoke の新フロー対応
- 手動スモーク手順

### Out (W4 以降)

- TURN リレー(W4 の範疇)
- NAT behavior discovery (RFC 5780)
- 対称 NAT 対応
- ICE full compliance(SDP, trickle ICE の本物, ICE restart など)
- host 側の複数 viewer 同時接続(W5 の常駐ループ)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| Probe protocol | `ControlMessage::Probe/ProbeAck` に 16-byte nonce | 既存の prdt-protocol enum に足すだけ、wire schema 拡張が自然。Noise は embedded nonce で守られる、probe は plain で OK(security 上は Noise 前の session 確立チェックのみ、盗聴で session が奪取されるリスクなし) |
| Probe timeout | 並行送信 + 10 秒で winner なしなら失敗 | 10 秒は spec §NAT Traversal Details §"pragmatic subset" と一致 |
| Nonce 長 | 16-byte (128-bit) | 衝突ほぼ無し、コスト小 |
| 採用する winner の基準 | **最初に自分の nonce echo を持つ ProbeAck が届いた source addr** | シンプル、race-free |
| Priority 情報の使用 | **W3 では未使用** | 最初到達 = 最適、priority tie-breaker は W5 まで延期 |
| STUN socket | transport socket を共有(`&Arc<UdpSocket>`) | W2 の別 socket 問題を解消、srflx port が実 port と一致 |
| signaling-client の peer_addr commit | 撤去、`peer_candidates` のみ返す | W3 で probe 層が選ぶ、重複責任の排除 |
| RendezvousOutcome.peer_addr | **廃止、SemVer 的に breaking change** | プレ 0.1.0 の prdt-signaling-client は internal crate、互換性負荷ゼロ。既存 bin は同じコミット内で書き換え |
| transport の socket() 露出 | `pub fn socket(&self) -> Arc<UdpSocket>` をそのまま提供 | Arc clone でコスト低、封印するメリットなし |
| 既存 handshake_as_* との組合せ | probe_and_commit_peer の後に handshake を呼ぶ(内部 auto でなく明示) | API が単純 |
| Host 側の auto-peer-update | W3 の probe_and_commit_peer が commit するので handshake 側のレガシー "auto-set from first packet" は無影響 | 既存動作を保持 |

---

## Architecture

### Protocol layer changes (prdt-protocol)

`ControlMessage` に 2 variant を追加:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ControlMessage {
    // ... existing variants ...
    Probe { nonce: [u8; 16] },
    ProbeAck { nonce: [u8; 16] },
}
```

既存 wire 上で ControlMessage がどう enumerated されているか次第で小調整あり(bincode tag 差分 → 既存受信側は未知 variant を error で返す想定 → 互換 break)。**プロジェクトは プレ 0.1.0 なので受容**。

### Transport layer (prdt-transport)

#### Socket 露出
```rust
impl CustomUdpTransport {
    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }
}
```

#### Probe/commit API
```rust
impl CustomUdpTransport {
    /// For each candidate, send a Probe packet with a unique nonce.
    /// Listen concurrently for incoming packets on the shared socket:
    ///   - If we see a Probe from X, echo ProbeAck to X.
    ///   - If we see a ProbeAck matching one of our outgoing nonces, lock
    ///     peer_addr = source and return it.
    /// Total timeout guards the whole operation.
    pub async fn probe_and_commit_peer(
        &self,
        candidates: &[SocketAddr],
        timeout: Duration,
    ) -> Result<SocketAddr, TransportError>;
}
```

Implementation notes:
- Each candidate gets ONE send (no retries in W3; W4/W5 can add retry)
- Nonces: generate per-candidate random 16 bytes
- Packet format: pre-Noise plain `ControlMessage` wrapped in the existing PacketHeader (no ENCRYPTED flag)
- Receive loop handles both directions (acting as probe responder AND probe requester concurrently)
- On first matching ProbeAck: `self.peer = Some(src)` via the existing `configure_peer`, return Ok(src)
- On timeout: return `TransportError::HandshakeTimeout` (reuse existing error)

### NAT-traversal (prdt-nat-traversal)

`learn_public_addr` の signature 緩和: take `&UdpSocket` も `&Arc<UdpSocket>` も可能にするため、**既存の `&UdpSocket`** のままで OK(`&*arc` で渡せる)。ドキュメントに「transport socket を共有できる」と追記するだけで足りる。

### Signaling-client (prdt-signaling-client)

`RendezvousOutcome` 構造変更:
```rust
#[derive(Debug, Clone)]
pub struct RendezvousOutcome {
    pub session_id: String,
    pub peer_pubkey_b64: Option<String>,
    pub peer_candidates: Vec<Candidate>,
    // peer_addr: SocketAddr,   ← 削除
}
```

`rendezvous_as_{host,viewer}` 内部の `recv_peer_candidates` は「Host-typ が来たら即 return」ロジックを **全 candidate 受信まで待つ** に変更(もしくは、適度な時間で収束 / 一定 idle で return)。

採用: **2 秒の aggregation window**(最初の candidate 受信から 2 秒間、追加 candidate を受け続け、その後 return)。これで Srflx が届く時間を確保しつつ、待ちすぎない。Host しか来ない LAN ケースでは 2 秒待つのみ(許容)。

### STUN integration simplification

`signaling-client` の `resolve_and_learn_srflx` は現在 `0.0.0.0:0` に独自 bind しているが、これを **transport が持つ socket を借りて** STUN に使うよう変更。`rendezvous_as_*` シグネチャに `socket: &Arc<UdpSocket>` を追加。

### Host / viewer orchestration

#### host bin の流れ(差分)
```rust
// 1. Bind transport
let transport = CustomUdpTransport::bind(...)?;
let shared_socket = transport.socket();

// 2. Rendezvous — pass shared socket so STUN uses transport's port
let outcome = rendezvous_as_host(
    cfg,
    identity,
    &shared_socket,   // NEW — STUN will use this
).await?;

// 3. Probe + commit
let socket_addrs: Vec<SocketAddr> = outcome.peer_candidates.iter()
    .filter_map(|c| parse_socket_addr(c))
    .collect();
let peer_addr = transport.probe_and_commit_peer(&socket_addrs, Duration::from_secs(10)).await?;
info!(?peer_addr, "probe selected winner");

// 4. Noise handshake (existing)
transport.handshake_as_server(&keypair).await?;
// 5. Hello/HelloAck (existing)
```

#### viewer bin
同様のパターン、`rendezvous_as_viewer` + `probe_and_commit_peer` + `handshake_as_client`。

### Priority-based ordering

**W3 では candidate 順序は無関係**(probe は並行送信、最初に成立した側が勝つ)。priority は W5 で selection refinement に使う予定。

---

## Wire Protocol (prdt-protocol) additions

### `ControlMessage::Probe { nonce }` → bincode on UDP

新しい variant。受信側は旧バージョンだと unknown variant でエラー返す → 実質的な wire break。プロジェクトは internal/pre-0.1.0 なので全コンポーネント同時バンプで OK。

### `ControlMessage::ProbeAck { nonce }`

同上。

### Wire fixture regression

`crates/protocol/tests/roundtrip.rs`(または既存の ControlMessage fixture テスト)に Probe/ProbeAck を追加して、bincode encoding が期待値になることを assert。

---

## Transport: probe_and_commit_peer 詳細設計

```rust
pub async fn probe_and_commit_peer(
    &self,
    candidates: &[SocketAddr],
    timeout_duration: Duration,
) -> Result<SocketAddr, TransportError> {
    // 1. Generate per-candidate nonces
    let mut pending: HashMap<[u8; 16], SocketAddr> = HashMap::new();
    for &addr in candidates {
        let nonce = random_16_bytes();
        pending.insert(nonce, addr);
        self.send_control_unencrypted(ControlMessage::Probe { nonce }).await?;
        // NOTE: send_control_unencrypted uses current_peer(). Since peer may
        // not yet be set, we extend it to accept an explicit target addr OR
        // we inline a send_to here on self.socket.
    }

    // 2. Receive loop with timeout
    let deadline = Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::HandshakeTimeout);
        }

        // Read one datagram with timeout.
        let (bytes, src) = match timeout(remaining, self.socket.recv_from(&mut buf)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(TransportError::Io(e)),
            Err(_) => return Err(TransportError::HandshakeTimeout),
        };

        // Parse header + body.
        match parse_control_unencrypted(&bytes[..]) {
            Ok(ControlMessage::Probe { nonce }) => {
                // Respond to peer's probe.
                self.send_control_to(ControlMessage::ProbeAck { nonce }, src).await?;
            }
            Ok(ControlMessage::ProbeAck { nonce }) => {
                if pending.contains_key(&nonce) {
                    // We win! Commit peer and return.
                    self.configure_peer(src).await;
                    return Ok(src);
                }
            }
            _ => {} // ignore non-probe during probe phase
        }
    }
}
```

API の要件:
- `send_control_unencrypted` が target addr を受けるバージョン(`send_control_to`)を用意する。既存 `send_control_unencrypted` は current_peer() 向け — これは残しつつ、addr 明示版を追加。
- `parse_control_unencrypted` は既存の `recv_raw_unencrypted` 内にあるパース処理を切り出した関数。

---

## Signaling-client: `recv_peer_candidates` の変更

新実装(2秒 aggregation window):
```rust
async fn recv_peer_candidates(
    ws: &mut Ws,
    total_timeout: Duration,
    aggregation_window: Duration, // 2s
) -> Result<Vec<Candidate>, SignalingError> {
    let deadline_total = Instant::now() + total_timeout;
    let mut collected = Vec::new();
    let mut first_seen: Option<Instant> = None;

    loop {
        let deadline_eff = match (first_seen, deadline_total) {
            (None, d) => d,  // waiting for first candidate
            (Some(t0), d) => d.min(t0 + aggregation_window),
        };
        let remaining = deadline_eff.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match recv_msg(ws, "peer_candidate", remaining).await {
            Ok(ServerMessage::PeerCandidate { candidate, .. }) => {
                collected.push(candidate);
                if first_seen.is_none() {
                    first_seen = Some(Instant::now());
                }
            }
            Ok(ServerMessage::Error { code, message }) => {
                return Err(SignalingError::Server { code, message });
            }
            Ok(other) => return Err(SignalingError::Protocol(format!("{other:?}"))),
            Err(SignalingError::Timeout { .. }) => break,
            Err(e) => return Err(e),
        }
    }
    if collected.is_empty() {
        return Err(SignalingError::Timeout { stage: "peer_candidate" });
    }
    Ok(collected)
}
```

Corresponding `rendezvous_as_*` の return:
```rust
Ok(RendezvousOutcome {
    session_id,
    peer_pubkey_b64,        // viewer only
    peer_candidates: collected,
})
```

---

## Test Strategy

### 1. `prdt-protocol` unit + fixture

- `Probe/ProbeAck` の serde JSON roundtrip + wire (bincode) literal fixture
- 既存 ControlMessage variant と互換性 assertion

### 2. `prdt-transport` unit

新テストファイル `crates/transport/tests/probe_test.rs`:
- `two_transports_find_each_other`: 2 つの `CustomUdpTransport` を同一プロセスで bind、互いの local_addr を候補として渡し、両方が同じ addr を commit することを assert
- `unreachable_candidate_is_skipped`: `["1.2.3.4:1", "127.0.0.1:<loopback_port>"]` を渡し、loopback が選ばれることを assert
- `all_unreachable_times_out`: 全 candidate が unreachable なら HandshakeTimeout が返ることを assert(short timeout 500ms)

### 3. `prdt-nat-traversal` — 変更なし(既存 3 tests が引き続き pass)

### 4. `prdt-signaling-client`

- 既存 W1 + W2 tests の更新(peer_addr 廃止、peer_candidates のみ、aggregation window の動作確認)
- `w3_aggregation_window`: 100ms 遅れて 2 つ目の candidate が来るケースで両方 collected を確認

### 5. W3 E2E smoke

`crates/signaling-client/tests/w3_smoke.rs`:
- mock STUN(返す「public addr」は各 transport の実 local_addr、W2 の "separate socket" 問題を模擬しないで済む)
- in-process signaling server
- 両端とも rendezvous → probe_and_commit_peer → Noise → Hello/HelloAck
- unreachable Host (例: `"240.0.0.1:1"`) + reachable Srflx (実 local_addr) を持たせ、Srflx が選ばれる mixed-candidate シナリオ
- 合格条件: 15s 以内に Hello/HelloAck、`peer_addr = reachable Srflx`

### 6. W1 / W2 smoke の追従

既存 `w1_smoke` / `w2_smoke` / `w2_peer_candidates` / `w2_stun_mock_host` が新 API で pass すること。多くは `outcome.peer_addr` → `outcome.peer_candidates.first()` 相当に書き換え、`probe_and_commit_peer` を経由するフローに更新。

### 7. 手動スモーク

`docs/superpowers/plans/2026-04-24-phase2-w3-manual-smoke-TODO.md`:
- 3-terminal、W1/W2 と同じコマンド + 期待挙動
- 別ネットワーク 2 台でのスモーク(ユーザーの任意、別マシン必要)

---

## Exit Criteria

- [ ] `prdt-protocol` に Probe/ProbeAck + fixture テスト
- [ ] `prdt-transport` に `socket()` + `probe_and_commit_peer` + 3 単体テスト
- [ ] `prdt-signaling-client` の peer_addr 廃止 + aggregation window 実装
- [ ] host/viewer bin の新オーケストレーション
- [ ] W1/W2/W3 全 smoke tests pass(workspace regression)
- [ ] Mixed-candidate test で Srflx が選ばれることを確認
- [ ] `cargo clippy --workspace` clean(media-win 除く)
- [ ] `phase2-w3-complete` タグ
- [ ] 手動スモークで W2 と同じ 60fps 映像確認

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| `ControlMessage` の wire break | 旧版の host/viewer との互換性喪失 | internal crate、同一コミットで全更新で OK |
| probe と既存トラフィックの同居(probe 受信中に Noise 済トラフィックが混入したら?) | 誤動作 | probe phase は Noise handshake 前にしか呼ばれない。phase 終わったら通常 recv loop に戻る、逆に probe を Noise 後に受信したら無視 |
| 複数 candidate が同時に ack 返るレース | 「最初の ack 先」が ambiguous | `pending` HashMap の remove は逐次処理、最初の match で return → 決定論的 |
| NAT Symmetric 両側失敗 | connection 確立できない | W3 の範囲外、W4 で TURN fallback |
| transport socket の所有権喪失で probe 中に他 task が recv を呼ぶ | 競合 | probe_and_commit_peer は transport の他の recv 操作と排他にする(実装で `&mut self` または mutex で守る) |
| aggregation window で LAN 接続開始が 2 秒遅れる | UX 劣化 | 許容(W2 で 5 秒相当、W3 で 2 秒に短縮は改善)、将来的には「Host 受信後 500ms で cutoff」等に調整可 |

---

## Open Questions (W3 実装中に決めてよい)

- `send_control_to(msg, addr)` の命名・API 詳細
- mixed-candidate test で unreachable Host のアドレス選定(`240.0.0.1`, `192.0.2.1` 等 RFC5737 予約)
- aggregation_window を `RendezvousConfig` で設定可能にするか vs 固定 2 秒か

---

## References

- W2 spec: `docs/superpowers/specs/2026-04-24-phase2-w2-stun-integration-design.md`
- Phase 2 全体: `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md`
- W2 merge: `phase2-w2-complete` (commit `1644e6b` on master branch)
