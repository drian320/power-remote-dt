# Phase 2: WAN + NAT Traversal + Signaling — Design

**Project**: power-remote-dt
**Document**: Phase 2 (WAN・NAT 越え + シグナリング) 設計書
**Date**: 2026-04-23
**Status**: Draft (brainstorming 合意待ち、実装計画未作成)
**Prereq**: Phase 0 〜 Phase 3 完了(Noise 暗号化・クリップボード・音声・双方向 FT まで)、Phase 4 GUI は並行または後行可

---

## Summary

**Phase 2 の目的**: 今 LAN でしか使えない power-remote-dt を、**異なるネットワーク・NAT 配下** にあるホストとビューワーが直接接続できるようにする。最終目標は「ビューワー側でホストの 16 桁 ID を入力するだけで接続成立」する Parsec / Moonlight WAN 相当の体験。

達成には以下 3 要素が揃う必要がある:

1. **シグナリングサーバ** — 両端が公開 IP:port を交換できる共通ランデブーポイント
2. **NAT 越え** — STUN で public addr を学習、hole punching で直接 UDP 路を開く
3. **TURN リレー** — NAT が対称型で直接路が開かない場合の最終手段

Phase 0〜3 で確立した **Noise_NK 暗号化と pubkey pinning は無改変で流用**。Phase 2 は「暗号化済み UDP セッションをどこに向けるか」を決める層を追加するだけ。

---

## Context & Scope

### Why Phase 2 after Phase 3 and Phase 4

歴史的には「Phase 0 → Phase 1 (Linux) → Phase 2 (WAN) → ...」の順を想定していたが、実装順は実態に合わせ以下で進んだ:

- Phase 3 (暗号化・双方向 FT・音声・多モニタ) を先に実装 → Phase 0〜3 で LAN 機能はほぼ完成
- Phase 4 (GUI) を先に入れる判断 → ユーザー到達性を上げる
- Phase 1 (Linux) は優先度低(macOS はさらに後)
- **Phase 2 は OSS 配布時の差別化の核**。LAN のみだと既存 RustDesk / Moonlight に対する優位性が薄い

よって Phase 4 の MSI 配布後、最優先で取り組む。Phase 1 (Linux) は Phase 2 のプロトコルが安定してから着手。

### In-scope (Phase 2)

- **シグナリングプロトコル** 設計(JSON over WebSocket、または gRPC 検討)
- **公式リレーサーバ** の spec(実装/運用は Phase 5)
- **host 側クライアント**: signaling 登録、STUN による public addr 学習、peer からの接続要求受諾
- **viewer 側クライアント**: signaling で host ID を問い合わせ → public addr 取得 → hole punch → Noise
- **ICE candidate 形式**の交換(host → signaling → viewer)
- **TURN 統合**(`webrtc-rs` の TURN モジュール、または `coturn` 外部サーバ + 独自クライアント)
- **ID 体系**: 16 桁数字 ID(Parsec 式) or base32 短縮 pubkey(Moonlight 式)の選定
- **フォールバック**: signaling サーバ到達不能時の挙動、TURN 不要時のダイレクト path 確率測定

### Out (Phase 2 の外)

- **Linux / macOS 対応**(Phase 1 の責任範囲、Windows と共通のプロトコル層は Phase 2 で書く)
- **アカウント管理**(メール登録、友だちリスト、履歴同期) — Phase 5
- **公式 ID サーバの運用**(DB スキーマ、スケーリング、監視) — Phase 5
- **Zero-trust identity**(メール OTP、OAuth、二要素認証) — Phase 5
- **モバイル(iOS/Android)視聴** — Phase 5+

---

## Architecture Overview

```
┌────────────┐         ┌─────────────────────┐         ┌──────────────┐
│  host      │ ══WS══▶ │  signaling server   │ ◀══WS═══ │  viewer      │
│  (behind   │         │  (公式リレー)        │         │  (different  │
│   NAT-A)   │         │  - peer 登録         │         │   NAT-B)     │
│            │         │  - candidate 転送    │         │              │
│            │         │  - offer/answer      │         │              │
│            │         └─────────────────────┘         │              │
│            │                                          │              │
│            │ ◀─────── STUN / TURN ────────────────▶  │              │
│            │                                          │              │
│            │ ═══════ hole-punched UDP (Noise) ═════▶ │              │
└────────────┘                                          └──────────────┘
```

### 典型的なフロー(ICE-like)

1. **host 起動** → signaling server に WS 接続 → `register { host_id, pubkey }` 送信
2. **host** が STUN サーバに問い合わせ、public addr (`ip:port`) を学習
3. **viewer 起動** → host ID 入力 → signaling server に `connect_request { host_id }` を送信
4. **signaling server** が両端に "now exchange ICE candidates" を通知
5. **host / viewer** それぞれ local / public candidate を列挙 → signaling 経由で相互送信
6. **hole punching**: 両端が相互に公開 ポートへ UDP パケットを送り合って NAT に状態を作る
7. **直接路成立** → 既存の Noise_NK ハンドシェイク → Phase 0〜3 で作ったパイプラインが動き出す
8. **直接路不可**(対称 NAT 両端など、全体の ~10%):TURN リレー経由で UDP を中継

### 暗号化との関係

- **既存の Noise_NK (Phase 3a) は無変更**。WAN 経路でも LAN と同じく Hello の前に Noise ハンドシェイクを走らせる
- pubkey pinning は signaling サーバ上で登録される `pubkey` を host の正統性の源として使う
- signaling サーバは **peer の IP:port を知る** が、**ペイロード(暗号化されたメディア/入力)は見られない** — ゼロ知識リレー
- TURN リレーもトランスポート層で盲目のまま(TLS 化された TURN か、TURN UDP + allocate だけで中継)

---

## Signaling Protocol

### Choice: JSON over WebSocket

候補比較:

| プロトコル | pros | cons |
|---|---|---|
| JSON/WebSocket | 実装容易、デバッグ容易、既存 tokio-tungstenite 利用 | 帯域効率は劣る(今回問題にならない) |
| gRPC (tonic) | 型安全、双方向ストリーム | プロキシ越えにくい、WS ほどのウェブ親和性なし |
| WebRTC DataChannel | 標準互換 | WebRTC スタック巨大、host サイドで過剰 |
| 独自 TCP | 最軽量 | ロードバランサ裏への展開が面倒(HTTP 経路に乗せたい) |

**採用**: JSON over WebSocket(wss://)。

### メッセージ例

`host → signaling`:
```json
{"t":"register","host_id":"123-456-789","pubkey":"b64=="}
{"t":"ready"}
{"t":"candidate","for_session":"abc","candidate":{"ip":"1.2.3.4","port":55000,"typ":"srflx"}}
```

`viewer → signaling`:
```json
{"t":"connect","host_id":"123-456-789"}
{"t":"candidate","for_session":"abc","candidate":{"ip":"5.6.7.8","port":44000,"typ":"host"}}
```

`signaling → both`:
```json
{"t":"offer","session":"abc","peer_pubkey":"b64==","peer_candidates":[...]}
{"t":"answer","session":"abc","peer_candidates":[...]}
```

### ID 体系

- **採用案**: 9 桁数字(`123-456-789` 表記)、グローバルユニーク、signaling サーバが割当
- pubkey は別途登録されるが、人間が読む/喋る/メモするのは数字 ID
- ID の永続性: `host-key.bin` 初回生成時に signaling 初回登録 → サーバから ID 付与 → `%APPDATA%\prdt\host-id.txt` に保存
- **プライバシー**: ID ↔ pubkey のマッピングは signaling サーバのみ。外部に出さない

代替案として **base32 短縮 pubkey**(`MOO-NBK-X7Q` 形式)も検討。公式 ID サーバに依存せず peer-to-peer で ID 検証できる利点はあるが、ID の短縮で衝突リスクと人間可読性のトレードオフ。初期は数字 ID で始め、Phase 5 で「サーバレス ID」オプション追加の道を残す。

---

## NAT Traversal Details

### STUN integration

- 採用クレート候補: `stun_codec` + 自前クライアント、あるいは `webrtc-rs` の一部を抜き出し
- Public STUN: `stun.l.google.com:19302` をデフォルト、公式リレー配下に独自 STUN も用意(Phase 5)
- 学習する属性: `XOR-MAPPED-ADDRESS`、`MAPPED-ADDRESS`(fallback)

### NAT 種別の判定

Phase 2 実装は **完全な NAT behavior discovery (RFC 5780) をやらない**。代わりに:

- host は起動時に STUN で public addr を取得 → signaling に登録
- viewer は同じく public addr を取得 → host の public addr 宛てに UDP 送信
- 10 秒タイムアウト以内にハンドシェイクパケットが届かなければ TURN にフォールバック

この「希望的 hole punch → 失敗で TURN」方式は実装が単純で、実用上 90%+ の成功率が出る(実験データ: Parsec/Moonlight も近い戦略)。

### TURN integration

- 採用候補: 自前 TURN client(`turn-rs` が生きていれば候補)、または `coturn` 外部サーバ前提で RFC 5766 クライアントを直接実装
- 公式リレーは Phase 5 で運用。Phase 2 時点では **ユーザー持ち込み TURN URL** でテスト可能にする:
  ```
  prdt-viewer --turn-url turn://user:pass@turn.example.com:3478
  ```
- TURN 使用時のオーバーヘッド: 片道 20〜40ms 追加。品質劣化は許容(対称 NAT が両端同時にあるケースは全体の 10〜20%)

### ICE simulation

完全 ICE (RFC 5245/8445) は実装しない。pragmatic subset:

- candidate 種別: `host` (local LAN addr), `srflx` (STUN 経由 public addr), `relay` (TURN)
- priority: 明示的に `host > srflx > relay`
- 先に成立したものを選ぶ、残りは捨てる(ICE-lite 的)
- 接続後の keepalive は既存の Ping/Pong 制御メッセージで兼用

---

## Crate Layout

新 crate:

- `crates/signaling-proto/`: ワイヤ型(`SigningMessage` enum、JSON serde)を定義
- `crates/signaling-client/`: host / viewer から使う WebSocket クライアント
- `crates/signaling-server/`: 公式リレー実装(tokio-axum 想定、Phase 5 で運用)
- `crates/nat-traversal/`: STUN / TURN クライアント、hole-punch 司令塔

既存 `transport` crate に **`set_peer_addr(SocketAddr)` API** を追加して、動的に宛先を切り替えられるようにする(現在は `CustomUdpTransport::bind` で固定)。Noise ハンドシェイクが始まる前に peer addr を確定するだけで、後は Phase 0〜3 のまま動く。

---

## Implementation Plan (段階分割)

### Plan 2-W1: signaling protocol skeleton (~2 週)

- `signaling-proto` crate、JSON 型定義、`SigningMessage` enum
- 最小 server (tokio-axum): register / connect / candidate echo
- host / viewer に `--signaling-url` オプション追加、LAN 接続時はバイパス
- 整合性テスト: 2 プロセス host + viewer を単一マシンで立ち上げ、signaling 経由で接続成立

### Plan 2-W2: STUN integration (~1 週)

- `nat-traversal::stun::learn_public_addr()` 実装
- host は起動時に public addr 学習 → signaling に register
- viewer も同じく public addr 学習 → signaling に connect_request
- 実環境テスト: 別ネットワーク(tethering + LAN)でハンドシェイク成立

### Plan 2-W3: hole punching + fallback (~2 週)

- 両端が受信した candidate リストに対し、並行 UDP 送信で hole punch
- タイムアウト 10 秒で最速成立した path を採用
- 成立しなかった場合 TURN へフォールバック(次フェーズで実装)

### Plan 2-W4: TURN client (~2 週)

- RFC 5766 の allocate/send/data indication をクライアント側実装
- `--turn-url` 指定でユーザー持ち込み TURN 検証
- 既存の `CustomUdpTransport` に TURN-allocated addr を差し込むだけで動く設計

### Plan 2-W5: ID system (~1 週)

- signaling-server に DB 追加(初期は SQLite、Phase 5 で PostgreSQL)
- host 初回登録時に数字 ID 発行、`host-id.txt` に保存
- viewer 側 GUI(Phase 4 F2)のランチャーに ID 入力欄を追加
- pubkey は signaling サーバが host_id に紐付けて保管、viewer に返す(TOFU の根拠を人間→サーバ経由に置き換え)

### Plan 2-W6: E2E on public internet (~1 週)

- 別ネットワーク 2 台(自宅 LAN + モバイル tethering)で実接続
- 計測: ハンドシェイク成立時間、STUN + hole punch 成功率
- 目標: LAN 路で成立可能なケースは **2 秒以内**、TURN 経由でも **5 秒以内**

合計見積もり: 9 週(~2.5 ヶ月)。Phase 5 運用インフラ(公式 signaling / TURN)はさらに + 4 週。

---

## Security Considerations

### signaling サーバ侵害時の影響

- サーバは各 peer の **pubkey と public addr のみ** を知る
- メディアペイロードは Noise_NK で暗号化済み → 傍受不可
- 侵害時の攻撃: ID ↔ pubkey の紐付けを **差し替え** されると MitM 可能
  - 対策 1: 既存の `known-hosts.json`(Phase 3d)で TOFU 固定
  - 対策 2: Phase 5 で transparent logging(Certificate Transparency 風)、第三者監査

### TURN リレー侵害時の影響

- パケットは通過するだけ、Noise 暗号化済み → 盲目のまま
- traffic analysis(パケットサイズ・タイミングからの推定)だけが残るリスク
- 公式リレーでログを 30 分で期限切らせるポリシー(Phase 5)

### rate limiting

- signaling サーバ: IP 単位 で register / connect 回数制限(Phase 5 運用)
- TURN サーバ: allocate 回数 + 転送帯域制限(Phase 5 運用)

---

## Exit Criteria

- [ ] 2 台のマシンを別ネットワーク(片方は自宅 LAN、もう片方はモバイルルータ経由)に接続し、GUI から host ID 入力のみで 5 秒以内にセッション確立
- [ ] TURN フォールバックが走るケース(強制的に UDP 直接路を塞いで検証)で機能する
- [ ] signaling server 側のログに pubkey/addr 以外の payload が流れていないことを確認
- [ ] LAN モード(`--signaling-url` 未指定)が既存と完全互換で動作(Phase 0 smoke test 全 pass)
- [ ] 実ネットワークでの latency オーバーヘッド測定: LAN 比 +10ms 以内が目標(TURN 経由は +20〜40ms)

---

## Open Questions (Phase 2 着手前に決めたいこと)

1. **signaling server の第一実装言語**: Rust(tokio-axum)? 他言語選択肢(Go 等)は範囲外で OK?
2. **ID 付与ポリシー**: 完全にサーバ依存(固定数字)/ 短縮 pubkey / 両方提供
3. **公式リレー運用の責任**: OSS 配布時、コミュニティが立てるのか、プロジェクトチームが運用するのか
4. **TURN 外部サーバ**: `coturn` 採用 vs 自前実装。自前実装は Rust 学習素材として良いが工数増
5. **NAT behavior discovery**: 不要と判断したが、実環境の成功率が低すぎた場合に備えて追加実装する余地を残すか
6. **WebRTC 互換性**: Phase 5 でブラウザクライアント作る場合、DataChannel との互換性が要るか(JSON signaling を SDP オファー/アンサーにマップ可能な設計にしておくか)

---

## Notes

- Phase 2 のゴールは **「LAN でしか使えない」→「どこからでも使える」** の変換。これが公開 OSS として差別化できる核心。
- 既存の Phase 0〜3 の暗号化レイヤー(Noise_NK)は無変更で流用できる設計になっている。Phase 2 の追加コードは「どこに Noise を向けるか」の決定と、UDP hole punch の手続きのみ。
- Phase 1 (Linux) より先に Phase 2 を実装する意義: OSS 公開時の「これで何が嬉しいの?」に対する最大の答えが WAN 対応だから。
