# Phase 2 W4: TURN Relay — Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W4 — TURN relay client for cross-symmetric-NAT fallback
**Date**: 2026-04-24
**Status**: Draft (W3 `phase2-w3-complete` merged to master)

---

## Summary

W1-W3 で signaling → STUN → hole-punching まで完成。W4 は **両端が対称 NAT で probe が失敗するケース** を TURN relay で救済する。

TURN (RFC 5766) は STUN method 拡張。Allocate / CreatePermission / Send Indication / Data Indication の 4 つを実装すれば relay として機能する。W4 ではそれらを実装し、relay 経由でも peer_addr ベースの既存 transport がそのまま動くよう `TurnRelaySocket` で wrap/unwrap を透過化する。

到達目標: 実 2 台(両側対称 NAT)で、手持ち TURN サーバ経由で 15 秒以内に映像接続成立。

---

## Scope

### In-scope

- `nat-traversal::turn` モジュール
  - `TurnClient` — allocate(長期クレデンシャル認証 + 401 challenge 再送)、CreatePermission、Send Indication の encode、Data Indication の decode
  - mock TURN server(in-process、テスト用 ~80 行)
- `TurnRelaySocket` — `tokio::net::UdpSocket` 互換の薄いラッパ。`send_to(peer, data)` → Send Indication で TURN 経由送信、`recv_from()` → Data Indication 受信時に `(peer, data)` 形に unwrap
- `CustomUdpTransport::bind_with_relay(TurnConfig)` — 新コンストラクタ。内部で `TurnRelaySocket` を使う
- `signaling-client::RendezvousConfig.turn_url` 追加、allocate 成功時に Relay candidate を signaling 経由で送信
- host/viewer bin に `--turn-url turn://user:pass@host:port` CLI
- W4 E2E smoke — in-process mock TURN + Host/Srflx candidate 全て unreachable のシナリオで Relay が probe に勝つ
- W1/W2/W3 smoke の追従(turn_url: None での既存動作)

### Out

- Refresh(allocation lifetime 10 分固定、W5 以降で追加)
- ChannelBind(Send Indication で十分)
- TCP/TLS TURN (RFC 6062/5766)
- TURN over TURN / complex routing
- 公式 TURN サーバ運用(Phase 5)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| TURN crate | 自前 + `stun_codec` 拡張(TURN method + attribute 追加) | rustun は abandoned 気味、webrtc-rs は overkill、stun_codec で十分 |
| auth 方式 | Long-term credential(RFC 5389 §10.2.2) | 標準、ほぼ全 TURN サーバがサポート |
| 401 処理 | Allocate 送信 → 401 で realm/nonce 受領 → MESSAGE-INTEGRITY 付きで再送 | 標準手順 |
| URL 形式 | `turn://user:pass@host:port`、port 省略時 3478 | STUN の `stun://` と並列 |
| 統合点 | `TurnRelaySocket` が `UdpSocket` の関連 API を実装、transport は socket を abstract 化 | wrap/unwrap を 1 箇所に閉じ込める |
| `CustomUdpTransport` への変更 | 新 `bind_with_relay` constructor、既存 `bind` は無変更 | 後方互換、既存の LAN / direct テストは無改造 |
| Permission 管理 | Allocate 直後に probe 候補の peer に CreatePermission、probe 中に新 peer は来ない前提 | W4 スコープ内で十分 |
| Lifetime | Allocate 時 600 秒要求、受け入れ時点の lifetime を保持、refresh しない | 試験運用には充分、W5 で自動 refresh |
| Relay candidate の生成 | `signaling-client` が rendezvous 前に turn_url に対して Allocate、RELAYED-ADDRESS を `CandidateType::Relay` として signaling に送る | priority=10 |
| probe 順序 | 既存の「first-to-ack」のまま(順序無関係) | W3 と同じ、Relay が最後に ack してもそれを採用 |

---

## Architecture

### Crate 変更マップ

```
crates/nat-traversal/
  src/turn.rs                    # TurnClient + TurnConfig + TURN message helpers
  src/turn_socket.rs             # TurnRelaySocket (TurnClient + UdpSocket 合成)
  src/lib.rs                     # re-exports
  tests/mock_turn.rs             # in-process TURN server + client roundtrip

crates/transport/src/udp.rs       # CustomUdpTransport::bind_with_relay

crates/signaling-client/src/config.rs   # RendezvousConfig.turn_url
crates/signaling-client/src/rendezvous.rs  # allocate + Relay candidate emission

crates/host/src/main.rs           # --turn-url CLI + bind_with_relay
crates/viewer/src/main.rs         # 同上

crates/signaling-client/tests/w4_smoke.rs  # Relay-only E2E
```

### 依存向き

```
nat-traversal ─→ stun_codec / bytecodec / tokio / rand_core
                                  ↑
                            signaling-client
                                  ↑
                            host / viewer
transport ─→ nat-traversal (TurnRelaySocket 使用時のみ、optional dep)
```

### TURN メッセージ基本形

既存の `stun_codec` は RFC 5389 primitives を完備。TURN は:
- 新 method: Allocate=0x003, Refresh=0x004(省略), Send=0x006, Data=0x007, CreatePermission=0x008, ChannelBind=0x009(省略)
- 新 attribute: LIFETIME(0x000D), XOR-PEER-ADDRESS(0x0012), DATA(0x0013), XOR-RELAYED-ADDRESS(0x0016), REQUESTED-TRANSPORT(0x0019), USERNAME(0x0006), MESSAGE-INTEGRITY(0x0008), REALM(0x0014), NONCE(0x0015), ERROR-CODE(0x0009)

`stun_codec` の `rfc5766` モジュール に一部実装あり。使える部分は使う、ないものは `define_attribute_enums!` で拡張。

### Allocate フロー(auth 付き)

```
1. Client → Server: AllocateRequest { REQUESTED-TRANSPORT=UDP, LIFETIME=600 }
2. Server → Client: 401 Error { REALM, NONCE }
3. Client: computes MESSAGE-INTEGRITY key = MD5(username:realm:password)
4. Client → Server: AllocateRequest {
      REQUESTED-TRANSPORT=UDP, LIFETIME=600,
      USERNAME, REALM, NONCE,
      MESSAGE-INTEGRITY = HMAC-SHA1(message_bytes_up_to_MI, md5_key)
   }
5. Server → Client: AllocateSuccess { XOR-RELAYED-ADDRESS, LIFETIME }
```

クライアントが保持する state: `relayed_addr: SocketAddr`, `username`, `realm`, `nonce`, `password`.

### CreatePermission フロー

```
Client → Server: CreatePermissionRequest {
    XOR-PEER-ADDRESS = peer,
    USERNAME, REALM, NONCE,
    MESSAGE-INTEGRITY
}
Server → Client: CreatePermissionSuccess {}  (attrs mostly empty)
```

### データ転送(Send Indication)

```
Client → Server: SendIndication {
    XOR-PEER-ADDRESS = peer,
    DATA = opaque bytes
}
→ Server が peer に opaque bytes を素の UDP で送る

peer → Server: 素の UDP (bytes)
Server → Client: DataIndication {
    XOR-PEER-ADDRESS = peer,
    DATA = opaque bytes
}
```

Send/Data Indication は MESSAGE-INTEGRITY 不要(RFC 5766 §10)。実装が少し軽くなる。

### TurnRelaySocket

```rust
pub struct TurnRelaySocket {
    inner: Arc<UdpSocket>,
    client: TurnClient,  // holds server_addr, relayed_addr, perms, auth state
}

impl TurnRelaySocket {
    /// Create by binding a new UDP socket, allocating on the given TURN server.
    pub async fn allocate(config: TurnConfig) -> Result<Self, TurnError>;

    pub fn local_addr(&self) -> std::io::Result<SocketAddr>;  // underlying socket addr
    pub fn relayed_addr(&self) -> SocketAddr;  // what peers see us as

    /// Ensure permission exists for `peer`; idempotent.
    pub async fn ensure_permission(&mut self, peer: SocketAddr) -> Result<(), TurnError>;

    /// Wrap data in Send Indication and send via TURN server.
    pub async fn send_to(&self, data: &[u8], peer: SocketAddr) -> std::io::Result<usize>;

    /// Read from underlying socket; if it's a Data Indication from the TURN
    /// server, unwrap to (real_peer_addr, data_bytes). If it's direct traffic
    /// (e.g. leaked), return as-is.
    pub async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;
}
```

### CustomUdpTransport Integration

Two parallel APIs:
```rust
impl CustomUdpTransport {
    // existing:
    pub async fn bind(addr: SocketAddr, cfg: UdpTransportConfig) -> Result<Self, TransportError>;

    // NEW:
    pub async fn bind_with_relay(
        addr: SocketAddr,
        cfg: UdpTransportConfig,
        turn: TurnConfig,
    ) -> Result<Self, TransportError>;
}
```

Internally, `bind_with_relay` constructs a `TurnRelaySocket` and stores it in a new `RelayMode` enum inside `CustomUdpTransport`. The send/recv paths consult this enum:
- Direct mode: `self.socket.send_to(buf, peer)` / `self.socket.recv_from(buf)` — unchanged
- Relay mode: `self.relay_socket.send_to(buf, peer)` / `self.relay_socket.recv_from(buf)` — auto-wraps

Because `TurnRelaySocket` presents a `send_to`/`recv_from` interface compatible with `UdpSocket`, the change is minimal — introduce a `SocketKind` enum that either owns the raw `Arc<UdpSocket>` or a `Arc<TurnRelaySocket>`, and have send/recv dispatch through a trait or match.

#### Simpler: box as dyn object

```rust
#[async_trait::async_trait]
trait SendRecvSocket: Send + Sync {
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> std::io::Result<usize>;
    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}
```

Implement for `Arc<UdpSocket>` and `Arc<TurnRelaySocket>`. `CustomUdpTransport` stores `socket: Arc<dyn SendRecvSocket>`.

This is cleaner but adds a trait object and changes the struct field type (minor breakage). Fallback: use an enum.

**Decision**: Use the enum approach (`Socket::Direct(Arc<UdpSocket>)` / `Socket::Relay(Arc<TurnRelaySocket>)`) — simpler, zero runtime overhead, no trait object indirection.

For this phase, the `socket()` getter (used by probe code to share with STUN) will need adjustment: in relay mode, STUN makes no sense (we're already behind NAT), so `socket()` only returns direct. Code paths that use `socket()` should check; in practice only STUN does, and STUN will NOT be called in TURN-only mode.

### Signaling-client integration

`RendezvousConfig.turn_url: Option<Url>` added. Inside `rendezvous_as_*`:
- If `turn_url.is_some()`, call `TurnClient::allocate` BEFORE signaling handshake (produces `relayed_addr`)
- Send Relay candidate via signaling: `Candidate { typ: Relay, ip: relayed_addr.ip(), port: relayed_addr.port(), priority: PRIORITY_RELAY }`
- ALSO: when the peer's Relay candidate is received, the local side must CreatePermission for the peer's relayed_addr (so its Send Indications from their TURN server reach us). Actually this is subtle — the peer sends FROM their allocated addr, so we need permission for THAT addr, not ours.

TURN relay flow in detail:
```
HostView: turn_server_A allocates → host_relayed_addr (on turn_server_A)
ViewerView: turn_server_B allocates → viewer_relayed_addr (on turn_server_B)

Host sends to viewer_relayed_addr via turn_server_A:
  host → turn_server_A [SendIndication(peer=viewer_relayed_addr, data=payload)]
  turn_server_A → viewer_relayed_addr (raw UDP, payload)
  turn_server_B receives it as if from turn_server_A's mapped addr
  turn_server_B delivers to viewer as DataIndication(peer=turn_server_A_addr)
```

Crucial: peer's DataIndication reveals `turn_server_A_addr` as the peer, NOT `host_relayed_addr`. This complicates permission logic and probe matching.

**Simplification for W4**: assume SAME TURN server (use `--turn-url` pointed at one shared server). Then both sides allocate on the same server; relayed addresses are on the same network; Send Indications reach each other via the server without external hops.

This is a reasonable constraint for W4 testing (one TURN server for both peers). Real-world use with different TURN servers is Phase 5.

### Probe interaction with TURN

Probe's `send_control_to(dst, msg)` sends via `self.socket.send_to`. If `self.socket` is a `TurnRelaySocket`, it auto-wraps in Send Indication. For the TURN server to forward, we must have CreatePermission for `dst`.

Order:
1. signaling rendezvous returns peer_candidates (including peer's Relay addr)
2. Before probe: call `relay_socket.ensure_permission(peer_addr)` for EACH candidate (batch)
3. probe sends Probe to each; TURN forwards (for relay candidates) or direct (for host/srflx)
4. Probe ack returns; transport commits winner

Implementation: if transport is in relay mode, `probe_and_commit_peer` first calls `ensure_permission` for each candidate.

---

## Testing Strategy

### 1. `nat-traversal::turn` unit

- Mock TURN server (in-process): handles Allocate (with 401 on first, success after MI), CreatePermission, Send/Data Indication echo
- Tests:
  - `allocate_without_auth_gets_401`
  - `allocate_with_auth_succeeds`
  - `create_permission_for_peer`
  - `send_indication_roundtrip` — TurnClient sends, mock echoes as DataIndication, client unwraps

### 2. `TurnRelaySocket` unit

- 2 TurnRelaySockets on same mock TURN server → `A.send_to(data, B.relayed_addr)` → `B.recv_from` returns `(data, A.relayed_addr)`

### 3. Transport `bind_with_relay`

- Build CustomUdpTransport with relay config, exchange control messages through it
- Assert raw packets on the underlying UdpSocket are Send/Data Indications

### 4. W4 E2E smoke

`crates/signaling-client/tests/w4_smoke.rs`:
- in-process mock TURN server
- in-process signaling server
- 2 transports via `bind_with_relay` — both allocate on the same mock TURN server
- Each rendezvous-ed with `turn_url` set → Relay candidate emitted in signaling
- peer_candidates for the OTHER side: only Relay reaches (Host/Srflx are fake/unreachable)
- probe picks Relay, Noise establishes, Hello/HelloAck succeeds
- assert relayed traffic took the TURN-wrap path (inspect byte counts)

### 5. W1/W2/W3 smoke follow-through

Add `turn_url: None` to every `RendezvousConfig` literal. No behavioral change expected.

### 6. Manual smoke

`docs/superpowers/plans/2026-04-24-phase2-w4-manual-smoke-TODO.md`:
- Spin up local `coturn` (docker) with credentials
- Run host + viewer with `--turn-url turn://user:pass@127.0.0.1:3478`
- Inject iptables drop rule to block direct probe → force TURN path
- Expect 60 fps via relay

---

## Exit Criteria

- [ ] TurnClient + mock tests pass
- [ ] TurnRelaySocket wrap/unwrap test passes
- [ ] `CustomUdpTransport::bind_with_relay` works
- [ ] signaling-client emits Relay candidate when turn_url set
- [ ] host/viewer bins accept `--turn-url`
- [ ] W4 smoke (Relay-only scenario) passes
- [ ] W1/W2/W3 smoke still pass (regression)
- [ ] clippy clean
- [ ] `phase2-w4-complete` tag

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| stun_codec が TURN attribute をサポートしない | 自前 encode/decode が必要 | `define_attribute_enums!` マクロで拡張、またはバイト列直接操作(20〜50 行) |
| MESSAGE-INTEGRITY の HMAC-SHA1 計算実装 | 標準 crate 必要 | `hmac = "0.12"` + `sha1 = "0.10"` 追加(軽量、既存暗号依存と統合しない) |
| mock TURN と本物の coturn の挙動差 | 手動スモーク時に顕在化 | RFC 5766 準拠の mock を書く、coturn で確認 |
| 両側別 TURN サーバの扱い | W4 スコープ外だがユーザー混乱リスク | doc で「same TURN server 前提」と明記 |
| TurnRelaySocket が UdpSocket の全 API を網羅していない(多い) | 将来 transport で追加メソッド使う時詰まる | send_to / recv_from / local_addr だけ実装、その他は呼ばれない前提 |
| Refresh なしで 10 分以上のセッションが切れる | 長時間試験で症状 | W5 で Refresh 追加、W4 では 10 分以内セッション前提 |

---

## Open Questions (W4 実装中に決めてよい)

- TURN server の IPv4/IPv6 同時対応 — W4 は IPv4 のみに絞る
- `allocate()` の retry 回数 — 401 1 回 + 認証再送 1 回で打ち切り
- `TurnConfig` に server_addr を `SocketAddr` で渡すか `Url` で渡すか — 内部は SocketAddr、bin は Url でパース

---

## References

- W3 spec: `docs/superpowers/specs/2026-04-24-phase2-w3-holepunch-design.md`
- RFC 5766: https://datatracker.ietf.org/doc/html/rfc5766
- RFC 5389 auth: https://datatracker.ietf.org/doc/html/rfc5389#section-10.2.2
- `stun_codec` docs: https://docs.rs/stun_codec/
