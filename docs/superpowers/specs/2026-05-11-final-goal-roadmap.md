# Final-Goal Roadmap — RustDesk-equivalent multi-platform + HW codec priority

**Date:** 2026-05-11
**Source:** CCG synthesis of Codex (architecture/risk) + Gemini (UX/product) advisors against current state at master HEAD `0d74dea` (post-L4)
**Scope:** Roadmap spec — each phase will get its own design spec + implementation plan via `superpowers:writing-plans` when ramped up
**Prior status:** `docs/superpowers/STATUS.md` is authoritative for already-completed work (Phase 0-4 + Plan4 大半 + Linux L0-L4 + adaptive bitrate L3 + encoder reconfigure L4)

---

## 1. Goal

> RustDesk 同等のマルチプラットフォーム リモートデスクトップを目指す。9桁 ID で接続、環境に応じて HW encode/decode を優先して高速動作。

**Definition of Done (product-level):**

1. Windows / macOS / Linux 全 host 対応、Windows / macOS / Linux / iOS / Android 全 viewer 対応 (Web は P10 optional)
2. 9桁 ID + Host PIN + 一時パスワード + 接続履歴で RustDesk 等価の接続フロー (既存 9桁 ID は完了済み、PIN/履歴未着手)
3. 各 OS で利用可能な最速 HW codec (NVENC / VAAPI / VideoToolbox / MediaCodec) を **自動選択** + **失敗時 SW fallback**
4. 30 分 soak + 実機マトリクスで安定性検証 + telemetry で fallback 理由可視化

---

## 2. Phase ordering

### 早見表 (合計 ~35-50週 / ~9-12 ヶ月、solo project 前提)

| # | Phase | 内容 | 期間 | Severity | 依存 |
|---|---|---|---:|---|---|
| **P5A** | Capability/Policy Layer | HW/SW codec 自動検出 + 優先順位 + フェイルオーバー + Status UI | 2-3週 | HIGH | (none) |
| **P5B** | Linux Wayland Portal + PipeWire | WSLg 脱却、ネイティブ Wayland capture | 3-4週 | HIGH | (none) |
| **P5C** | Linux HW codec | VAAPI + NVENC-Linux + V4L2 M2M、zero-copy 優先 | 4-6週 | HIGH | P5A |
| **P6** | Auth & Connection UX | Host PIN / 一時パスワード / 履歴 / 許可待ち popup / QR / Onboarding | 3-4週 | HIGH | (none) |
| **P7A** | macOS Viewer | Metal renderer + VT decode + 入力 + 配布 | 3-4週 | MEDIUM | P5A |
| **P7B** | macOS Host | ScreenCaptureKit + VideoToolbox + TCC + Notarization | 5-7週 | HIGH | P5A, P7A |
| **P8A** | iOS Viewer MVP | Rust core + SwiftUI 薄層 | 5-7週 | MEDIUM | P5A, P7A |
| **P8B** | Android Viewer MVP | Rust core + Kotlin/Compose 薄層 | 5-7週 | MEDIUM | P5A |
| **P9** | Hardening / Soak / Telemetry | 実機マトリクス + 30 min soak + fallback 理由 telemetry + crash 相関 | 3-4週 | HIGH | All previous |
| **P10** | (Optional 将来) | Web viewer (WebRTC + WASM decoder)、AV1、mobile host、macOS file transfer | — | LOW | All previous |

### 着手順序

`P5A → (P5B || P6 並行) → P5C → P7A → P7B → P8A/B 並行 → P9 → P10`

理由:

- **P5A を最初**: codec 選択/フェイルオーバーの共通基盤。後続 phase で全 OS が再利用するため、最初に作ると手戻りが減る (Codex 強推)
- **P5B / P6 並行可**: capture と auth UX は独立。両方前進可能
- **P5C は P5A 後**: SelectionPolicy が出来てから VAAPI/NVENC-Linux backend を流し込む
- **P7A → P7B**: macOS viewer 先行で Metal/VT/署名パイプ検証、host (ScreenCaptureKit + TCC + Notarization) は最重リスク
- **P8A/B 並行**: iOS と Android は独立 binary
- **P9 最後**: 全 backend 揃ってから実機マトリクス + soak

---

## 3. Phase 詳細

### P5A: Capability/Policy Layer (2-3週)

**Why first:** 全 phase が再利用する codec 選択基盤。固定チェーンではなく "OS 別候補列 + スコアリング" で書く (Codex 推奨)。

**Components (4 分離):**

```text
prdt-media-core (existing trait crate に追加 or 新 crate)
├── CapabilityProbe          ← 起動時/再接続時に backend 列挙
├── VideoEncoder/Decoder     ← 既存 trait (live reconfigure 対応済み L4)
├── SelectionPolicy          ← 候補列 + スコア (priority + copy-count + 推定 latency + 過去成功率)
└── HealthMonitor            ← DeviceLost/遅延悪化検知 → 次候補へ切替 (cooldown 付)
```

**選択ロジック:**

1. 候補列挙 (OS / driver / codec / profile / 解像度 / zero-copy 可否)
2. スコア計算 (優先度 + コピー回数ペナルティ + 推定遅延 + 過去成功率)
3. 上位から試行、`DeviceLost` 含め失敗時に次候補へ
4. 実行中は AIMD/遅延監視で「同系内 reconfigure (= L4)」優先、だめなら段階的 fallback
5. backend 再試行は cooldown (flapping 防止)

**UX 統合 (Gemini 推奨):**

- viewer overlay に `Video: NVENC (HW)` / `Video: OpenH264 (SW)` バッジ表示
- SW fallback 時、初回のみトースト「パフォーマンス向上のため HW を試みましたが、互換性のため SW モードで動作しています」
- Settings 詳細に `Force Software Encoder` / `Force Software Decoder` toggle

**DoD:**

- Windows で {NVENC, MF, OpenH264} の自動選択 + DeviceLost → 次候補移行が動く
- Linux で {OpenH264} のみだが SelectionPolicy 経由でスコア出力される
- viewer overlay の HW/SW バッジ表示
- 1 backend が DeviceLost を起こしても session 再起動なしで継続

### P5B: Linux Wayland Portal + PipeWire (3-4週)

**Why:** 現状 WSLg 経由の X11 path のみ。実機 Wayland (GNOME/KDE/Sway) ネイティブ capture が必要。

**Tasks:**

- `xdg-desktop-portal` 経由 ScreenCast 取得
- `pipewire-rs` で PipeWire stream consume
- DMABUF (zero-copy) 取得 → I420 変換 (P5C で HW 化)
- portal authorization の UX (システムダイアログ) を host GUI から起動
- compositor 別 (GNOME/KDE/Sway/Hyprland) smoke test

**Risk (HIGH):** compositor 差で挙動が違う、portal の persistence (session-saved permission) 周り、Wayland multi-monitor 座標系。

### P5C: Linux HW codec (4-6週)

**Why:** Linux でも Windows 同等のパフォーマンス + 電力を達成。

**Tasks:**

- VAAPI encode (Intel iGPU + AMD APU) — `libva` bindgen + dmabuf import
- VAAPI decode (zero-copy to D3D11 / EGL)
- NVENC on Linux (nvidia-headers crate)
- V4L2 M2M (Pi, embedded)
- P5A SelectionPolicy への登録

**Risk (HIGH):** driver 断片化 (Mesa version、nvidia proprietary、AMDGPU profile 差)。Mitigation: backend ごと feature gate + 互換性テーブル + SW 即時 fallback。

### P6: Auth & Connection UX (3-4週、P5 と並行可)

**Why:** 9桁 ID は完成 (Phase 2 W5)、PIN / 履歴 / 許可待ち UX が未着手。RustDesk 体験差を最短で詰める。

**Tasks:**

- **Host PIN**: ユーザー設定固定 PIN (固定パスワード型)
- **一時パスワード**: ランダム生成、有効期限付 (毎回 viewer に共有)
- **接続履歴**: viewer 起動画面に最近の接続リスト + アリアス + オンライン状態 (Parsec 風)
- **許可待ちポップアップ**: host 側で接続要求毎に Allow/Deny + per-permission toggle (画面操作 / クリップボード / file transfer)
- **QR コード共有**: host ID/PIN を QR 化、mobile viewer (P8) と連携
- **Onboarding wizard**: host 初回起動で大 ID 表示 + 無人アクセス toggle + パスワード設定

**Wire change:** signaling-proto に `auth_method: enum { Tofu, Pin, Ephemeral }` 追加、protocol_version bump 検討

### P7A: macOS Viewer (3-4週)

**Why:** macOS Viewer 先行で Metal renderer + VideoToolbox decode + 配布 (Notarization) パイプラインを検証。Host より landmine が少ない。

**Tasks:**

- `metal` crate or `wgpu` で D3D11 同等の YUV→BGRA renderer 実装 (Linux softbuffer から学んだ pattern)
- `screencapturekit-rs` ではなく decode side: VideoToolbox H.265 decoder (`vt-rs` or 自前 binding)
- 入力: `core-graphics` で synthetic mouse/keyboard
- Apple Silicon + Intel 両対応
- 配布: ad-hoc signing + Developer ID for distribution (Phase 5 cert 購入と同期)

### P7B: macOS Host (5-7週)

**Why landmines (Codex):**

- TCC 権限導線 (Screen Recording + Accessibility + Input Monitoring) で初回 UX が壊れやすい — **HIGH**
- ScreenCaptureKit 更新追従 (旧 CGDisplay 系 API は macOS 15 で obsoleted、Firefox 事例あり) — **HIGH**
- Code signing / Hardened Runtime / Notarization で配布工程が複雑 — **HIGH**
- VideoToolbox 動的 reconfigure・キーフレーム制御差異 — MEDIUM
- マルチモニタ / Retina / 色空間 / 可変 refresh でフレーム時刻ズレ — MEDIUM

**Tasks:**

- ScreenCaptureKit (macOS 12.3+) で capture
- VideoToolbox HEVC encode (live reconfigure = L4 同等)
- TCC 権限ガイド UI (信号機型ステータス + Visual Guide スクショ + 設定パネル直接 deep link)
- Notarization 自動化 (CI pipeline + altool/notarytool)
- 配布: DMG + brew tap

### P8A: iOS Viewer MVP (5-7週)

**Architecture:** Rust core + SwiftUI 薄層 (Codex 推奨)。Flutter / RN / Tauri Mobile は低遅延 video + 入力で bridge 負債が大きい。

**Tasks:**

- `cargo-xcode-build` or `uniffi-rs` で Rust core を XCFramework に
- VideoToolbox H.265 decode (macOS と共有)
- Metal renderer (P7A と共有)
- タッチ → mouse mapping (タップ = 左クリック、長押し = 右クリック、二本指スクロール)
- on-screen keyboard activation
- Push notification で接続要求通知 (オプション)

### P8B: Android Viewer MVP (5-7週)

**Architecture:** Rust core + Kotlin/Compose 薄層。

**Tasks:**

- `cargo-ndk` で Rust core を `.so` build
- `MediaCodec` H.265 decode (Surface output zero-copy)
- `SurfaceView` + OpenGL/Vulkan renderer
- タッチ mapping (P8A と同等パターン)
- 対象 SoC を絞った MVP (Snapdragon 8xx / Tensor / Exynos 主流)、デコーダ能力の runtime probe 必須

**Risk (MEDIUM):** Android デバイス差。Mitigation: 対象 SoC 絞った MVP + runtime probe で feature gate

### P9: Hardening / Soak / Telemetry (3-4週)

**Tasks:**

- 実機互換性マトリクス (OS x GPU x codec backend、CI で nightly run)
- 30 分 soak test (既存 `prdt-bench-matrix --duration 30m` を全 OS で)
- 失敗時の自動復帰の網羅: capture / encode / device lost / network drop
- Telemetry: 接続品質 (e2e p95)、fallback 理由 (どの backend が落ちたか)、crash 相関
- Self-hosting guide: signaling-server を Docker 一発で立てる手順

### P10: (Optional 将来)

- **Web viewer**: WebRTC ingest + WASM H.265 decoder (or transcode to VP9/AV1 server-side)。install 不要が魅力だが、別アーキ (リアルタイム低遅延ループ書き直し)。MVP 後に再評価。
- **AV1 codec**: NVENC AV1 (Ada Lovelace+)、VideoToolbox AV1 (M3+)、libdav1d decode。bandwidth 制約強い環境で価値、early adoption は YAGNI
- **Mobile host**: viewer MVP 達成後、需要次第
- **macOS file transfer**: Phase 3c 同等の bidirectional drag-drop

---

## 4. HW Codec Abstraction 設計 (P5A 詳細)

**現状 (`prdt-media-core`):**

```rust
pub trait Encoder { fn encode(&mut self, ...); fn set_target_bitrate(&mut self, bps: u32); }
pub trait Decoder { fn decode(&mut self, ...); }
pub enum EncodeError { DeviceLost, ... }
```

**P5A 拡張案:**

```rust
// 1. Capability probe
pub trait CapabilityProbe {
    fn list_encoders() -> Vec<EncoderCapability>;
    fn list_decoders() -> Vec<DecoderCapability>;
}

pub struct EncoderCapability {
    pub backend: BackendKind,        // Nvenc, Vaapi, Mf, VideoToolbox, MediaCodec, OpenH264
    pub codec: Codec,                 // H264, H265, AV1
    pub max_resolution: (u32, u32),
    pub zero_copy: bool,
    pub priority: i32,                // OS 別固定優先度
}

// 2. Selection policy
pub trait SelectionPolicy {
    fn pick_encoder(&self, candidates: &[EncoderCapability], context: &PolicyContext) -> Option<BackendKind>;
}

pub struct PolicyContext {
    pub target_resolution: (u32, u32),
    pub target_bitrate_bps: u32,
    pub past_failures: HashMap<BackendKind, FailureRecord>,
    pub user_override: Option<BackendKind>,
}

// 3. Health monitor
pub trait HealthMonitor {
    fn record_success(&mut self, backend: BackendKind);
    fn record_failure(&mut self, backend: BackendKind, reason: EncodeError);
    fn should_failover(&self) -> Option<BackendKind>;  // None = stay
}

// 4. Encoder factory (existing trait + 動的構築)
pub trait EncoderFactory {
    fn create(&self, backend: BackendKind, cfg: &EncoderConfig) -> Result<Box<dyn Encoder>, FactoryError>;
}
```

**OS 別候補列の例:**

| OS | 優先順 (高 → 低) |
|---|---|
| Windows | NVENC HEVC → MF HEVC → NVENC H264 → MF H264 → OpenH264 |
| Linux + NVIDIA | NVENC HEVC → NVENC H264 → VAAPI HEVC (if Mesa) → OpenH264 |
| Linux + Intel/AMD | VAAPI HEVC → VAAPI H264 → V4L2 M2M → OpenH264 |
| macOS | VideoToolbox HEVC → VideoToolbox H264 → OpenH264 |
| iOS / Android | (decode only) MediaCodec/VT HEVC → MediaCodec/VT H264 → OpenH264 |

---

## 5. UX 設計指針 (Gemini 合成)

### 接続フロー

- **Main 画面ハイブリッド構成**: 「最近の接続リスト」 + 「ID 入力フォーム」を 50/50 で配置 (Parsec + AnyDesk のいいとこ取り)
- **オンライン状態アイコン**: 履歴の各エントリに green/grey で online/offline 表示 (signaling-server に presence query 追加)

### Permission UX

- **信号機型ステータス**: 「画面記録権限: ❌」「アクセシビリティ: ❌」をリスト化、`[設定を開く]` ボタンを横に
- **Visual Guide**: macOS の権限設定画面のスクリーンショットに自社アイコンを赤枠で囲んだ図解をアプリ内インライン表示
- **UAC 説明**: Windows で「管理者として」要求する理由 (Ctrl+Alt+Del 送信など) を tooltip で

### HW Codec 透明性

- **Status overlay バッジ**: viewer overlay に `🚀 NVENC (HW)` / `💻 OpenH264 (SW)` を表示
- **Auto default**: ユーザーは触らず動く。trouble 時のみ Settings 詳細から `Force SW` toggle
- **Fallback toast**: 初回 SW 落ち時に「HW 失敗、SW で動作中」を一回だけ表示

### Onboarding wizard (Host 側、初回起動)

1. ID 巨大表示 + ステータス (Ready/Offline)
2. 「無人アクセスを有効にするか?」toggle
3. 「パスワードを設定する」ボタン (Pin or Ephemeral 選択)
4. 完了 → tray 常駐モード

---

## 6. 製品ポジショニング

**ターゲット**: Linux を常用するエンジニア + SMB (中小企業) 情シス
**メッセージ**: 「ゲーミングの低遅延を、ビジネスの安定性で。」

**勝ち筋:**

- **AIMD adaptive bitrate + live encoder reconfigure** (L3 + L4): 競合の RustDesk は固定 bitrate で network 劣化に対応できない
- **Linux ネイティブ HW codec (P5C)**: RustDesk は Linux で SW codec のみ、当製品は VAAPI で HW 化
- **Rust メモリ安全性**: セキュリティ要件が厳しい SMB に訴求
- **Self-hosting friendly**: signaling-server を Docker 一発で立てられる (P9 で整備)

---

## 7. Risk Top 5 (合成)

| # | Risk | Score | Mitigation |
|---|---|---|---|
| 1 | Wayland capture + 入力統合 (compositor 差) | HIGH | Portal/PipeWire を P5B で隔離実装、compositor 別 CI 実機テスト (GNOME/KDE/Sway/Hyprland) |
| 2 | Linux HW codec driver 断片化 | HIGH | backend ごと feature gate、互換性テーブル、SW 即時 fallback |
| 3 | macOS TCC + 署名 + Notarization | HIGH | P7A (Viewer) で署名パイプライン早期構築、開発版/配布版で設定分離 |
| 4 | Codec 切替時のセッション不安定 | MEDIUM-HIGH | P5A の HealthMonitor + DeviceLost 試験 + 再接続状態機械の標準化 |
| 5 | Android デバイス差 (特に MediaCodec 互換性) | MEDIUM | 対象 SoC を絞った MVP、デコーダ能力の runtime probe 必須化 |

---

## 8. YAGNI / 見落とし check

**過剰スコープ (Codex):**

- ❌ Web viewer 早期投入 → P10 optional に明記
- ❌ Mobile host → P8 では viewer 限定
- ❌ AV1 早期投入 → P10 optional

**見落としがちな必須項目 (Codex):**

- ✅ 実機互換性マトリクス + 長時間 soak test → P9 で実施
- ✅ 失敗時の自動復帰 (capture/encode/device lost) → P5A HealthMonitor + P9 で網羅
- ✅ 権限未付与時の明確 UX 導線 → P5B (Wayland portal) + P7B (macOS TCC) で個別実装
- ✅ Telemetry (接続品質・fallback 理由・クラッシュ相関) → P9 で実施

---

## 9. 着手手順

`writing-plans` で各 phase ごとに個別 plan 化 (subagent-driven-development で過去 L1-L4 と同じパターン):

1. **P5A** brainstorm → spec → plan → 実装 (今回 user 選択、最初に着手)
2. その後、user に都度 phase 選択を問う

各 phase の完了で git tag (例: `phase-5a-capability-layer-complete`) + STATUS.md 更新 + master へ `--no-ff` merge。

---

## 10. References

- 現状: `docs/superpowers/STATUS.md`
- 過去 spec: `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md` 他
- 過去 plan: `docs/superpowers/plans/`
- L4 (直前完了): `docs/superpowers/specs/2026-05-11-l4-encoder-reconfigure-design.md` + `plans/2026-05-11-l4-encoder-reconfigure.md`
- CCG advisor outputs:
  - Codex: `.omc/artifacts/ask/codex-rust-roadmap-...-2026-05-11T05-06-11-698Z.md`
  - Gemini: `.omc/artifacts/ask/gemini-ux-...-2026-05-11T05-05-19-638Z.md`
- Apple ScreenCaptureKit: <https://developer.apple.com/documentation/ScreenCaptureKit?changes=_8>
- Firefox の macOS 15 SDK 旧 CGDisplay obsoleted 事例: <https://bugzilla.mozilla.org/show_bug.cgi?id=1982470>
- Rust `screencapturekit` crate: <https://docs.rs/screencapturekit/latest/screencapturekit/>
