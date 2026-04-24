# Phase 2 W6 Polish — Probe Retry + Host Auto-Detect Design

**Project**: power-remote-dt
**Phase**: 2 (WAN + NAT Traversal + Signaling)
**Step**: W6 polish — UX improvements surfaced by real 2-machine LAN verification
**Date**: 2026-04-24
**Status**: Draft (built on `phase2-w6-complete` tag)

---

## Summary

W6 の実機 2台 LAN 検証 で顕在化した 2 つの UX 摩擦を解消する:

1. **Probe retry** — `CustomUdpTransport::probe_and_commit_peer` は Probe を 1 候補あたり 1 回しか送らず、ファイアウォールが初回 UDP を drop する環境では最大 10 秒(全体タイムアウト)待たされる。200ms 間隔 × 5 回の再送で、単発 drop を隠蔽する。
2. **Host-side auto-detect** — host bin は現在 `--bind <LAN_IP>:9000` の手動指定必須。viewer は W6 で `--bind 0.0.0.0:0` デフォルト + 自動検出を実装済み。同じ機能を host にも入れて、`--bind 0.0.0.0:9000` のみで正しい LAN インターフェイスを自動選択できるようにする。

副次的に viewer に生えた `discover_outbound_ip` インライン関数を `signaling-client` クレートへ抽出し、viewer/host で共有する。

到達目標: 両機が `--signaling-url` を渡すだけで特別な bind 指定なしに LAN/WAN 接続できる。ファイアウォールの「最初の UDP を捨てる」挙動で待たされない。

---

## Scope

### In-scope
- `signaling-client`: `pub async fn discover_outbound_ip(url: &Url) -> io::Result<IpAddr>` を追加 + unit test
- `viewer`: 既存インライン版を削除、signaling-client の関数を呼び直す
- `host`: `args.bind.ip().is_unspecified()` 時のみ auto-detect(signaling mode 限定)
- `transport`: `probe_and_commit_peer` に 200ms × 5 回の再送ループ + packet-drop シミュレーション integration test
- `PROBE_RETRY_INTERVAL` / `PROBE_RETRY_COUNT` を `pub const` として公開(テストから参照できるように)
- `phase2-w6-polish-complete` タグ

### Out (W7+ / 他フェーズ)
- 指数バックオフ(LAN で 200ms 固定で十分、必要になれば別フェーズで)
- Firewall 自動登録(Q1 brainstorm で除外、管理者昇格が必要で自動 E2E 不可)
- Probe の並列送信最適化(現在の直列送信で十分)
- direct-mode での auto-detect(signaling URL が無いので判断材料なし、`--bind` 必須のまま)
- IPv6 特有のインターフェイス選択ロジック(現行の `discover_outbound_ip` が IPv4/IPv6 両方扱える)

---

## Decisions

| 項目 | 採用 | 理由 |
|---|---|---|
| Retry 間隔 | 200ms 固定 | LAN RTT は通常 <5ms、200ms あれば 1 RTT + ファイアウォール state install に十分 |
| Retry 回数 | 5 回(初回 + 4 再送) | 5 × 200ms = 1 秒。全体 10s タイムアウトに対して早期に候補の反応を引き出しつつ、残り 9s は受動待機 |
| Retry 停止条件 | `pending` が空 or timeout | 既存の ProbeAck マッチ時 return ロジックでカバー |
| `discover_outbound_ip` 置き場所 | `signaling-client` クレート | signaling URL を元に判定するのでここが自然。nat-traversal は STUN/TURN スコープなので違う |
| viewer のインライン実装 | 削除して signaling-client 経由に切替 | DRY |
| host の auto-detect 起動条件 | `args.bind.ip().is_unspecified()` かつ `args.signaling_url.is_some()` | direct-mode は判断材料なし |
| auto-detect 失敗時の挙動 | warn ログを出して元の `args.bind` を維持 | viewer と同じ失敗モード |
| Retry タイマー実装 | `tokio::time::interval` を `select!` で recv と並行に | 既存 recv ループに自然に組み込める |

---

## Architecture

### モジュール変更マップ

```
crates/signaling-client/src/
  lib.rs または net.rs (新規 or 既存追加)
    + pub async fn discover_outbound_ip(url: &Url) -> io::Result<IpAddr>
    + #[cfg(test)] mod tests { localhost URL → 127.0.0.1 }

crates/viewer/src/main.rs
  - async fn discover_outbound_ip(...)  (削除)
  + use signaling_client::discover_outbound_ip;

crates/host/src/main.rs
  + use signaling_client::discover_outbound_ip;
  + if args.bind.ip().is_unspecified() && args.signaling_url.is_some() {
  +     match discover_outbound_ip(&signaling_url).await { ... }
  + }

crates/transport/src/udp.rs
  pub const PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(200);
  pub const PROBE_RETRY_COUNT: u32 = 5;

  pub async fn probe_and_commit_peer(...) {
      // 初回送信
      // tokio::time::interval(PROBE_RETRY_INTERVAL) でループ
      // select! {
      //     _ = retry_ticker.tick() if sends_done < PROBE_RETRY_COUNT => { resend_pending }
      //     recv = self.socket.recv_from(&mut buf) => { handle Probe/ProbeAck }
      //     _ = sleep(remaining_until_deadline) => return HandshakeTimeout
      // }
  }
```

### Probe retry 状態機械

```
State { pending: HashSet<nonce>, sends_done: u32, deadline: Instant }

初期: sends_done=1, pending={nonce per candidate}, 全候補へ Probe 送信

イベント:
  - interval tick (200ms おき):
      if sends_done < PROBE_RETRY_COUNT && !pending.is_empty():
          各 pending nonce の候補に Probe 再送
          sends_done += 1
  - recv Probe(nonce): ProbeAck を from に返す(既存ロジック)
  - recv ProbeAck(nonce):
      if nonce in pending:
          configure_peer(from)
          return Ok(from)  # 早期終了
  - deadline 到達: return HandshakeTimeout  # 既存
```

pending は送信時 nonce を入れるだけで、ProbeAck 受領時に `remove` はしない(最初の成功で即 return するため)。再送候補の判定はシンプルに「pending 全部に再送」でよい。

### Host auto-detect フロー

```
host bin startup:
  args = Args::parse()
  signaling_url = args.signaling_url.clone()  // Option<Url>

  let effective_bind = if args.bind.ip().is_unspecified() {
      if let Some(ref url) = signaling_url {
          match discover_outbound_ip(url).await {
              Ok(ip) => {
                  let new_bind = SocketAddr::new(ip, args.bind.port());
                  info!("host auto-detected bind: {} -> {}", args.bind, new_bind);
                  new_bind
              }
              Err(e) => {
                  warn!("auto-detect failed: {e}; keeping {} (may bind to wrong iface)", args.bind);
                  args.bind
              }
          }
      } else {
          args.bind  // direct mode: no signaling URL, keep as-is
      }
  } else {
      args.bind
  };

  CustomUdpTransport::bind(effective_bind, ...).await
```

---

## `discover_outbound_ip` 仕様

```rust
pub async fn discover_outbound_ip(url: &url::Url) -> std::io::Result<std::net::IpAddr> {
    let host = url.host_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing host"))?;
    let port = url.port().unwrap_or(80);
    let resolved = tokio::net::lookup_host(format!("{host}:{port}")).await?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no addr"))?;
    let probe = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    probe.connect(resolved).await?;
    Ok(probe.local_addr()?.ip())
}
```

動作原理: UDP connect はパケットを送らないが、kernel のルーティングテーブルから outbound route を選択する。選ばれた経路の local IP を `local_addr()` で読める。これで Hyper-V 仮想 NIC ではなく物理 NIC 経由の IP が取れる(`0.0.0.0` バインドは未確定)。

URL の scheme は無関係(`ws://` でも `wss://` でも `http://` でも host:port が解決できれば十分)。port 省略時は 80 を仮定(signaling URL 実運用は明示ポート前提だが、未指定時のフォールバック)。

IPv6 対応: `lookup_host` は AAAA/A どちらでも返す。先頭の一つを使う。dual-stack 環境で AAAA が先頭に来ると IPv6 アドレスを返すが、これは kernel のルーティング優先度と一致しているので問題ない。

---

## Testing Strategy

### 1. `signaling-client::discover_outbound_ip` unit

`crates/signaling-client/src/lib.rs`(または `net.rs`)の `#[cfg(test)]` 内:

```rust
#[tokio::test]
async fn discover_outbound_ip_resolves_localhost() {
    let url = url::Url::parse("ws://127.0.0.1:8080/signal").unwrap();
    let ip = discover_outbound_ip(&url).await.unwrap();
    assert!(ip.is_loopback(), "expected loopback for 127.0.0.1 target, got {ip}");
}

#[tokio::test]
async fn discover_outbound_ip_rejects_missing_host() {
    let url = url::Url::parse("file:///tmp/x").unwrap();
    let err = discover_outbound_ip(&url).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}
```

### 2. `probe_and_commit_peer` retry — integration

`crates/transport/tests/probe_retry.rs`:

```rust
// シナリオ: server 側が最初の 2 つの Probe を無視、3 つ目に ProbeAck を返す
// 期待: client は 3 つ目の Probe が送信された直後(~400ms 後)に成功

#[tokio::test]
async fn probe_retry_survives_first_packet_drop() {
    // 2 つの CustomUdpTransport を loopback で bind
    // server 側の socket を別途開いて、Probe を 2 回 recv_from で捨ててから、3 つ目に ProbeAck を返す
    // client 側で probe_and_commit_peer(10s) を呼ぶ
    // 400ms 程度で Ok(addr) が返ることをアサート
    // 送信回数が 3 以上であることを(server 側カウンタで)アサート
}
```

テスト実装ヒント: 既存の `tests/holepunch.rs` が同様の 2-socket セットアップをしているのでそのパターンを踏襲。Probe の順序カウントは server 側 `recv_from` ループで個別にカウント。

### 3. viewer/host auto-detect — 既存 smoke で回帰確認

- `crates/signaling-client/tests/w1_smoke.rs` 〜 `w5_smoke.rs` は全部 `127.0.0.1` 環境で動く。これらが pass すれば `discover_outbound_ip` の置き換えは破壊していない。
- host bin の auto-detect は bin 内ロジックなので unit test は追加しない(実機 W6 検証が回帰チェック)。

### 4. Clippy / format

- `cargo clippy --all-targets --all-features -- -D warnings` clean
- `cargo fmt --check` clean

### 5. 実機 2-machine 検証(手動、spec の exit 基準に含めない)

- Machine A: `prdt-host.exe --bind 0.0.0.0:9000 --signaling-url ws://192.168.100.101:8080/signal`
  - ログに `host auto-detected bind: 0.0.0.0:9000 -> 192.168.100.101:9000` が出ること
- Machine B: Probe retry が観測できるよう、いったんファイアウォールルールを削除 → `prdt-viewer.exe` を一度実行 → 最初の UDP ダイアログ approve
  - 初回接続が 10s 待たずに成功することを確認(体感 1 秒以内)

---

## Exit Criteria

- [ ] `signaling-client::discover_outbound_ip` 実装 + 2 unit tests
- [ ] viewer インライン版を削除、signaling-client 経由に切替、build 通過
- [ ] host bin に auto-detect ロジック + `info!`/`warn!` ログ
- [ ] transport の `probe_and_commit_peer` に retry ループ
- [ ] `PROBE_RETRY_INTERVAL` / `PROBE_RETRY_COUNT` pub const
- [ ] `crates/transport/tests/probe_retry.rs` integration test pass
- [ ] W1-W5 smoke regression 全 pass
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] git tag `phase2-w6-polish-complete`

---

## Risks & Mitigations

| リスク | 影響 | 緩和策 |
|---|---|---|
| retry ループで `tokio::time::interval` が `recv_from` の select と噛み合わない | テスト失敗、デッドロック | `tokio::select!` で両方を並行待機、`biased;` は使わない(公平) |
| retry 送信中に ProbeAck が来て pending を残したまま return すると次回呼び出しに影響 | 状態リーク | pending は関数ローカル変数、return で破棄 |
| host auto-detect で signaling 以外のトラフィック経路が選ばれる(VPN 経由など) | 不正な LAN IP が公開される | これは現行の viewer も同じ挙動、許容。`--bind` で明示上書きできる |
| `discover_outbound_ip` が DNS lookup で遅延 → 起動時間悪化 | 数百ms〜秒 | 実運用 signaling URL は通常 IP リテラルか短いホスト名、無視できる |
| port=80 fallback が signaling URL で非 80 の想定と食い違う | auto-detect は UDP probe を connect するだけで HTTP 接続はしないので実際には無害 | 既存 viewer と同じフォールバック、一貫性優先 |

---

## Open Questions (実装中に決めてよい)

- `signaling-client` のファイル構成: 既存 `lib.rs` に追加 vs 新規 `net.rs` — 小さい関数なので `lib.rs` で十分
- retry のログレベル: `trace!("probe retry #{sends_done}")` にして通常は見えない
- integration test の server 側 socket を `tokio::net::UdpSocket` 直接開くか、既存の `CustomUdpTransport` を拡張するか — 直接開く方が単純

---

## References

- W6 findings: `docs/superpowers/plans/2026-04-24-phase2-w6-real-2-machine-lan.md`
- Phase 2 全体: `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md`
- W3 holepunch spec(probe 実装の元): `docs/superpowers/specs/2026-04-24-phase2-w3-holepunch-design.md`
