# power-remote-dt — Project Status & Roadmap

**Last updated:** 2026-04-25
**Latest tag:** `phase4-g1-complete`
**Branch state:** master (all phase work merged)
**Test count:** 229 automated tests across the workspace, all passing

---

## Project elevator pitch

OSS / 配布可能な Parsec / Moonlight / RustDesk 競合を目指す Rust 製 ultra-low-latency リモートデスクトップ。ゲーミング + デスクワーク両方で使える低遅延を狙う。Windows-only から始めて、cross-platform に拡張できるアーキテクチャ。

---

## 1. 完了済み(タグ別)

### Phase 0 — コアパイプライン
| タグ | 内容 |
|---|---|
| `phase0-plan1-complete` | foundation crates(protocol/transport/crypto)構築 |
| `phase0-plan2a-complete` | D3D11 デバイス + テクスチャ + swapchain |
| `phase0-plan2b-complete` | DXGI Desktop Duplication capture + NVENC H.265 encode |
| `phase0-plan2c-complete` | Media Foundation H.265 decode + IMFDXGIBuffer zero-copy |
| `phase0-plan3-complete` | host/viewer 配線、Hello/HelloAck/Ping/Bye |
| `phase0-complete` | E2E PoC(暗号化なし)— 86 tests pass |

### Phase 3a〜3d — 機能拡張
| タグ | 内容 |
|---|---|
| `phase3a-complete` | Noise_NK_25519_ChaChaPoly_BLAKE2s E2E 暗号化、明示 nonce で UDP reorder 耐性 |
| `phase3b-clipboard-complete` | 双方向クリップボードテキスト同期 |
| `phase3b-audio-complete` | WASAPI loopback capture + Opus + 双方向再生 |
| `phase3c-filetransfer-complete` | viewer→host drag-drop ファイル転送(64MB cap、衝突回避、8KB chunks) |
| `phase3c-multimonitor-complete` | HelloAck で host_monitor_rect/host_virtual_desktop_rect 共有、`MOUSEEVENTF_VIRTUALDESK` |
| `phase3c-bidirectional-filetransfer-complete` | host→viewer 方向追加(`prdt-filetransfer` crate、`--outgoing-dir` 2s polling) |
| `phase3d-complete` | handshake timeout 5s、known_hosts file、Noise rekey |

### Plan 4 — ロバスト化と観測
| タグ | 内容 |
|---|---|
| `plan4-f6-complete` | DXGI AccessLost 自動復旧(UAC/lock/フルスク game)|
| `plan4-f7-step1-complete` | DXGI_ERROR_DEVICE_REMOVED 分類、session_id ランダム化 |
| `plan4-f7-producer-complete` | producer 側の DEVICE_REMOVED 検出 |
| `plan4-m1-complete` | viewer LatencyProbe(arrival/decode/present の p50/p95/p99 を 1Hz 出力) |
| `plan4-m2-complete` | `prdt-latency-bench` per-packet 統計 + CSV(loopback p95 ≈ 40µs) |
| `plan4-m2-full-pipeline-complete` | フル NVENC + MF in-process bench(1080p60 e2e p95 ≈ 19ms) |
| `plan4-stats-complete` | viewer→host `LatencyReport` 制御メッセージ |

### Plan 2d — NVDEC 実装
| タグ | 内容 |
|---|---|
| `plan2d-scaffold-complete` | NVDEC モジュール骨組み、viewer `--decoder` フラグ |
| `plan2d-step2a-complete` | bindgen(nvcuvid.h + cudaD3D11.h + cuda.h)+ CUDA context wrapper |
| `plan2d-step2b-complete` | CUvideoparser + CUvideodecoder E2E(CPU NV12 出力) |
| `plan2d-complete` | D3D11 NV12 テクスチャ出力 via UpdateSubresource、viewer 統合可 |
| `plan2d-bench-complete` | NVDEC 経路ベンチ(MF との比較) |
| `plan2d-zerocopy-complete` | dual R8 + R8G8 D3D11 textures + CUDA-D3D11 device-to-device cuMemcpy2D。`DualPlaneFrame` / `DualPlaneYuvRenderer`(自前 HLSL VS+PS、BT.709 limited-range YUV→BGRA)。viewer の `--decoder nvdec` 経路で zero-copy。`cpu-nv12` feature でテスト用 CPU readback パス保持。218 tests pass |

### Phase 2 — WAN + NAT 越え + シグナリング
| タグ | 内容 |
|---|---|
| `phase2-w1-complete` | signaling-proto/client/server + host_id TOFU、LAN loopback E2E |
| `phase2-w2-complete` | STUN client(`nat-traversal` crate)+ srflx 候補のシグナリング伝播 |
| `phase2-w3-complete` | Hole punching via Probe/ProbeAck(kinds 20/21)、`probe_and_commit_peer`(first-to-ack wins)、aggregation 2s window |
| `phase2-w4-complete` | TURN relay(RFC 5766):TurnClient(Allocate + 401/MI auth + CreatePermission + Send/Data Indication)+ TurnRelaySocket + `bind_with_relay` + Relay candidate emission |
| `phase2-w5-complete` | signaling-server SQLite-backed HostStore(9-digit ID 採番 + pubkey 検証)、host bin が `host-id.txt` に永続化、viewer は `--host-id` のダッシュ正規化、`ErrorCode::HostIdPubkeyMismatch` |
| `phase2-w6-complete` | 実機 2 台 LAN 検証(Machine A:192.168.100.101 ↔ Machine B:192.168.100.127)、viewer に `--bind` + outbound-IP 自動検出を追加(commit 49bb3e7) |
| `phase2-w6-polish-complete` | `probe_and_commit_peer` が 200ms × 5 回 Probe 再送(`MissedTickBehavior::Skip`)で stateful firewall の初回 drop を吸収、host も outbound-IP 自動検出、`discover_outbound_ip` を `signaling-client::net` に共通化、`PROBE_RETRY_INTERVAL`/`PROBE_RETRY_COUNT` pub const |

### Phase 4 — UI / 配布(着手済み)
| タグ | 内容 |
|---|---|
| `phase4-title-status-complete` | viewer ウィンドウタイトル動的更新(接続状態反映) |
| `phase4-g1-complete` | egui ベース GUI 基盤(gui-common / gui-host / gui-viewer)。host: 鍵生成 → pubkey/QR 表示 → Start/Stop で tokio task 制御。viewer: 保存接続先一覧 → 接続フォーム → 既存 winit/D3D11 にフォールスルー。`%APPDATA%\prdt\config.toml` 永続化。両 binary とも `--headless` で従来 CLI 互換。 |

---

## 2. 残タスク(優先順)

### **A. すぐ取れる、影響大、規模小**

#### A1. Plan 4 B1-B8 — 実機 2 台ベンチマーク行列
- **状態**: spec 未作成。phase2-w6 で構築した 2 台 LAN 環境(Machine A: RTX 3070 Ti、Machine B: GTX 1080 + GTX 1080)が手元にある今が好機
- **何を測るか**(想定):
  - B1: 解像度マトリクス(1080p / 1440p / 4K)
  - B2: ビットレートマトリクス(5/10/20/30/50 Mbps)
  - B3: コーデック比較(H.265 / 将来 AV1)
  - B4: 経路比較(LAN / loopback / TURN relay)
  - B5: デコーダ比較(MF / NVDEC)
  - B6: FEC 効果(k=8 / 32 / 64、m=2 / 6)
  - B7: input round-trip latency(クリック→画面反映)
  - B8: 長時間安定性(30分連続接続でのレイテンシ・パケットロス推移)
- **アウトプット**: CSV + ヒートマップ画像、各設定の glass-to-glass 推奨ガイド
- **ブロッカー**: Plan 4 M3(カメラ実測)が未着手なので、glass-to-glass の客観値はまだ取れない。CPU タイムスタンプベースで代替する手はある
- **見積もり**: spec(0.5d)+ plan(0.5d)+ 実装 + 実測(2-3d)

#### ~~A2. Plan 2d optimization — NVDEC 真ゼロコピー~~ — **完了 (2026-04-25, `plan2d-zerocopy-complete`)**
- ~~CPU バウンス排除~~ → dual R8 + R8G8 D3D11 textures + CUDA-D3D11 interop 経由で達成
- ~~色変換~~ → 自前 HLSL pixel shader (BT.709 limited-range YUV→BGRA)
- 残り(将来 Plan 4 等で): DualCache のダブルバッファ化、HDR/10bit (P010)、BT.601 自動切替

### **B. 中規模、優先度中**

#### B1. Phase 4 GUI(本格) — **G1 完了 (2026-04-25, `phase4-g1-complete`)**
- ~~spec~~ → `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(全体)+ `2026-04-25-phase4-g1-egui-foundation-design.md`(G1)
- ~~G1: egui 基盤 + host GUI + viewer launcher~~ ✅
- 残: G2 in-stream overlay (latency p50/p95、ESC menu、~1 週)、G3 tray + auto-start (~1 週)、G4 MSI installer + 自動更新 (~2 週)、G5 crash reporter + コード署名 (~1 週)、G6 多言語化 (~1 週)
- 合計残: ~6 週(parent spec は元々 8 週見積もり、G1 で ~2 週分が完了)

#### B2. Phase 1 — Linux サポート
- **状態**: 着手前。Windows-specific 部分(`media-win` / `input-win`)を Linux 等価実装に置換
- **必要モジュール**:
  - `media-linux`: PipeWire screencast capture、VA-API (intel/AMD) or NVENC (NVIDIA) encode、libav decode
  - `input-linux`: evdev + uinput injection、X11/Wayland clipboard、virtual desktop (Wayland では複数モニタが API レベルで違う)
- **ブロッカー**: Wayland のキャプチャはコンポジタ依存、screencast portal 経由のフレームレート制限あり
- **見積もり**: 大(3-4 週)。OS-independent な `protocol`/`transport`/`crypto` はそのまま使える

### **C. 計測 / 観測 系(blocker 解消用)**

#### C1. Plan 4 M3 — カメラ実測 glass-to-glass
- **状態**: 着手前
- **やること**: viewer 画面に既知 LED パターン or QR タイムコードを表示、host 画面にカメラ向けて差分から end-to-end 遅延を実測。Pi Camera or USB Webcam(120fps 推奨)
- **見積もり**: ハードウェア準備(0.5d)+ 計測スクリプト(1d)+ 実測 + 分析(1-2d)

### **D. 配布準備(Phase 5 前哨)**

- D1. インストーラ(MSIX / WiX / cargo-wix)
- D2. コードサイニング証明書(Authenticode、~$200/年)
- D3. クラッシュレポーター(`sentry-rust` or 軽量自作)
- D4. オートアップデート(GitHub Releases ベース)
- D5. ライセンス整理 / OSS 公開準備
- **見積もり**: 各 1-3d、合計 1-2 週

---

## 3. 既知の制約 / 技術的負債

memory `known_limitations.md` 参照。要点だけ:

- **MF decoder 単一 GPU loopback で ~3 fps**: 同一プロセス + 同一 GPU でエンコード/デコード両方走らせると decoder が間に合わない。実機 2 台 LAN(W6 で確認済)では 60fps incoming だが decoder が 3fps、これは GPU 負荷ではなく MF 内部のスループット限界。NVDEC 切替で改善余地あり(A2)
- **Multi-monitor 仮想座標系**: HelloAck で送る `host_virtual_desktop_rect` は WIndows API の primary 原点固定。viewer 側のマップは合っているが、host が解像度切替した場合の追従なし(再接続必要)
- **TURN refresh / channel bind 未実装**: Phase 2 W4 では Send/Data Indication で動かしているが、長時間接続 → Allocate lifetime 切れの自動 refresh は未対応。10 分接続で再接続必要

---

## 4. 推奨次アクション

| 順位 | アクション | 理由 |
|---|---|---|
| **1** | Phase 2 W6 polish の実機検証 | branch merge 済み、master でテスト可能。host `--bind 0.0.0.0:9000` で auto-detect ログが出ること、ファイアウォール初回 drop 後の接続が <1s で成功することを観測 |
| 2 | A1 Plan 4 B1-B8 ベンチマーク | 2 台 LAN 環境がある今がベスト。spec→plan→subagent 実装の流れで自動進行可能 |
| 3 | A2 Plan 2d zero-copy NVDEC | decoder ボトルネックの解消は UX 影響大。loopback 単一 GPU でも fps が上がる可能性 |
| 4 | C1 Plan 4 M3 カメラ実測 | B1-B8 の前提として欲しい(glass-to-glass の客観値) |
| 5 | B1 Phase 4 GUI | UX 仕上げ、配布前に必須 |
| 6 | D1-D5 Phase 5 配布準備 | 公開直前 |
| 7 | B2 Phase 1 Linux | 規模大、Phase 5 直前 or Phase 5 完了後 |

---

## 5. 開発フロー(参考)

各 W / Plan は概ね以下のパイプライン:

```
brainstorming  →  spec       →  writing-plans   →  subagent-driven-development  →  tag
   (skill)     ↓ (docs/specs/)  ↓ (docs/plans/)    ↓ (TDD per task, 2-stage review)
   user 同意  Y/N             Y/N               commits per task
```

- spec は `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`
- plan は `docs/superpowers/plans/YYYY-MM-DD-<feature>.md`
- 各 W 完了で `phase<N>-w<M>-complete` タグ + master へ `--no-ff` merge
- subagent-driven なら同セッションで自動進行(controller がレビュー dispatch)

---

## 6. 参考リンク

| 項目 | 場所 |
|---|---|
| Phase 0 全体 spec | `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md` |
| Phase 0 status | `docs/superpowers/PHASE0-STATUS.md` |
| Phase 2 全体 spec | `docs/superpowers/specs/2026-04-23-phase2-wan-nat-design.md` |
| Phase 4 GUI spec | `docs/superpowers/specs/2026-04-23-phase4-gui-design.md` |
| W6 実機 2 台 findings | `docs/superpowers/plans/2026-04-24-phase2-w6-real-2-machine-lan.md` |
| W6 polish spec | `docs/superpowers/specs/2026-04-24-phase2-w6-polish-design.md` |
| W6 polish plan | `docs/superpowers/plans/2026-04-24-phase2-w6-polish.md` |
| 各 W spec | `docs/superpowers/specs/2026-04-2{3,4}-phase2-w{1..6}-*.md` |
| 各 W plan | `docs/superpowers/plans/2026-04-2{3,4}-phase2-w{1..6}-*.md` |
| Plan 3 manual smoke | `docs/superpowers/plan3-manual-smoke.md` |
| Phase 3a smoke | `docs/superpowers/phase3a-smoke.md` |
