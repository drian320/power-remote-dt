# power-remote-dt — Project Status & Roadmap

**Last updated:** 2026-04-26
**Latest tag:** `mf-encoder-fallback-complete`
**Branch state:** master (all phase work merged) — **Phase 4 + Plan 4 B1 + B4 + B6 + B7 + B8 完了 + MF エンコーダ fallback 完了 (B3 のみ HW ブロック保留)**
**Test count:** 312 automated Rust tests + 11 Python tests, all passing

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
| `plan4-b1-bench-matrix-complete` | `prdt-bench-matrix` bin(60 構成 sweep:解像度 1080/1440/2160 × bitrate 5/10/20/30/50Mbps × decoder mf/nvdec × fps 60/120)。`run_for_matrix` core 抽出、`MatrixAxes` / `ConfigStats` / `expand_matrix` / `aggregate` / per-frame + summary CSV writer。`scripts/analyze-bench-matrix.py`(per-stage stats、paired NVDEC/MF、stability、outlier、fps-ratio)。実機実測(RTX 3070 Ti、2026-04-26): NVDEC が 29/29 paired 構成で MF より速い(median e2e_p50 ratio 0.83、CV 0.286 vs 0.309、loss 1930 vs 3857 ppm)。viewer の default decoder を `nvdec` に変更。bench-matrix の inter-config delay 250ms で NVENC/NVDEC state leak 解消。NVDEC cfg propagation バグ(latency-bench/build.rs)同時修正(従来 `prdt-latency-bench --consumer nvdec` も decoded=0 だった)。 |
| `plan4-b6-fec-bench-complete` | `prdt-fec-bench` bin(純 CPU FEC アルゴリズム bench、30 構成 sweep: k=8/32/64 × m=2/6 × drop_ppm=0/1%/5%/10%/20%、1000 trials/構成、~30 秒)。`packetize → per-packet drop → FrameAssembler` を直接駆動、transport / GPU / 暗号化なし。`Cfg` / `TrialOutcome { CompleteNoFec, CompleteWithFec, Lost }` / `simulate_one_trial`(xorshift64 RNG で seed-deterministic な drop 判定)/ `aggregate`(recovery_rate_ppm + reconstruct p50/p95)/ `write_summary_csv`(12 列)。8 unit tests。実測結果: drop=0 で 100% 復元、k=8m6 が k=8m2 より drop=20% で +33% の recovery rate(945k vs 610k ppm)、reconstruct latency は k=8 で ~9µs / k=64 で ~270µs。出力は seed-deterministic(timing 列のみ wall-clock ジッター)。`docs/fec-bench.md` に schema + 解釈例。 |
| `plan4-b7-input-load-bench-complete` | `prdt-input-load-bench` bin(input event 配送 lag を並行 video 負荷下で計測、12 構成 sweep: input_rates=[100,500,1000,5000]Hz × video_rates=[0,60,120]fps、5s/構成、~63 秒)。InProcTransport pair で `tokio::spawn` 3 タスク(input sender、optional video sender、receiver)+ CancellationToken cancel-on-deadline + 50ms post-cancel drain。InputEvent には timestamp フィールド無いので `mpsc::unbounded_channel<u64>` で sender→receiver に sent_us を流す。`aggregate`(input_loss_ppm + p50/p95/p99)/ `write_summary_csv`(10 列)。7 unit tests(2 sync + 2 async + 3 aggregate/csv)。実測結果(LoopbackOptions::default = drop_ppm=0): 全 12 構成で loss=0、p50 5-15µs、p95 25-43µs。`docs/input-load-bench.md` に schema + 解釈例 + 限界(real network/host inject/glass-to-glass を含まない)。 |
| `plan4-b4-net-profile-bench-complete` | `prdt-net-profile-bench` bin(network profile sweep、20 構成: latencies_ms=[0,1,10,50,200] × drops_ppm=[0,1000,10000,50000]、5s/構成、~82 秒)。LoopbackOptions の `latency`/`drop_ppm` を populate して InProcTransport で simulated 1-way delay + msg-level drop。input + video 両方の sent/received 計数。`run_one_config`(B7 派生、3 spawn task)/ `aggregate`(input_loss_ppm + video_loss_ppm + input p50/p95/p99)/ `write_summary_csv`(15 列)。7 unit tests。実測結果(RTX 3070 Ti): lat=0/drop=0 baseline で p50=11µs、lat=10ms/drop=0 で p50=15.6ms(10ms 注入 + sleep オーバヘッド)、lat=50ms で sender が ~16 events/sec まで blocking で律速。**既知限界: drop_ppm>0 時の input_p50/p95/p99_us は FIFO-pairing artifact で inflated**(loss 列は正確; docs Caveats に明記 + 修正案あり)。Real LAN/TURN/jitter は2台環境必要で out-of-scope。 |
| `plan4-b8-stability-bench-complete` | 30 分長時間安定性。新 Rust コードなし — 既存 `prdt-bench-matrix --duration 30m --resolutions 1080 --bitrates 30 --decoders nvdec --fps 60` を実機で 1 回実行 + 新 `scripts/analyze-stability.py`(pandas + numpy)で per-frame CSV を分単位 bucket 解析。`bucket_frames` / `percentile_round`(Rust 互換 half-away-from-zero)/ `e2e_p50_slope_us_per_minute`(wall-clock 時間軸の線形回帰)/ `outlier_buckets`(e2e_p99 > 2× median)+ CLI(stdout drift summary)+ 11 unittest tests。実測結果(RTX 3070 Ti、1080p60 30Mbps NVDEC、30分): 89921 sent / 89920 received(loss 11ppm = 0.001%)、e2e_p50=13.3ms / p95=14.6ms / p99=16.9ms、drift slope=-4.42µs/min(基本 flat)、bucket 間 max-min=482µs、outlier なし → **30 分通して安定**確認。`docs/stability-bench.md` に schema + 実測結果 + interpretation。Out of scope: real network drift、OS-level memory leaks、multi-config matrix(時間爆発)、glass-to-glass(M3)。 |
| `mf-encoder-fallback-complete` | Windows MF H.265 エンコーダ fallback 実装。`Hevc265Encoder` trait + `EncodedH265Frame` を新 `encoder_trait.rs` に集約、`NvencEncoder` と新 `MfH265Encoder` が実装。`HwHevcEncoder { Nvenc(Box<NvencEncoder>), Mf(Box<MfH265Encoder>) }` enum で runtime dispatch。`DxgiNvencProducer` が enum 受け取り `with_encoder()` constructor 追加。host bin に `--encoder {auto,nvenc,mf}` + `HostConfig.encoder` serde default。`prdt-bench-matrix` に `--encoders` 軸 + config_id フォーマット変更(`encnvenc`/`encmfenc`、`decmfdec`/`decnvdec`)、summary CSV 18 列。`MfH265Encoder` は `MFTEnumEx(HW flag)` で OS H.265 MFT を検索、async MFT event protocol(`IMFMediaEventGenerator` + `METransformNeedInput/HaveOutput`)、`ICodecAPI` CBR rate-control 設定。BGRA→NV12 は既存 `BgraToNv12` D3D11 VideoProcessor 経由。E2E 動作確認済み(最初の IDR デコード成功)。**既知制限: NVIDIA の MF HEVC MFT は ICodecAPI bitrate hints を無視し IDR が ~470KB に膨張して FEC budget (75KB) を超過するため、NVIDIA では auto-select で NVENC が優先される。AMD/Intel は未検証。詳細は `docs/encoders.md` 参照。**|

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
| `phase4-g6-complete` | i18n(英/日)。`fluent_templates` で `en.ftl` + `ja.ftl` を crate に embed、OS ロケール自動検出(sys-locale)、`Config.gui.locale` で固定可、Settings に Language ドロップダウン。`tr()` / `t!()` マクロ。54 ID。`%APPDATA%\prdt\locale\<lang>\main.ftl` でユーザー override 可(将来拡張)。 |
| `phase4-g2-complete` | viewer in-stream overlay(B1 別プロセス)。`prdt-viewer-overlay`(eframe)を ESC で spawn、ファイル IPC(stats.json 1Hz / control.json polling)で latency p50/p95/p99 表示 + Resume/Disconnect ボタン。`dirs::cache_dir()/prdt/overlay-ipc/<pid>/` で PID 隔離、Drop で child kill + dir 削除。`--headless` で無効。Win/Linux/macOS 同一実装。モバイルは Phase 5+ で viewer 全体再実装時。 |
| `phase4-g3-complete` | tray + 通知 + auto-start(Win)。`tray-icon` 0.14、`notify-rust` 4.x(1s debounce)、`winreg` で HKCU\Run\PrdtHost = "<exe> --headless"。Hide-to-tray、4 項目メニュー(Settings/Stop/Logs/Quit)。プレースホルダ PNG(build.rs 生成、G5 で正式)。10 i18n ID。 |
| `phase4-g4-complete` | MSI インストーラ + 自動アップデート。`wix/main.wxs`(perUser %LOCALAPPDATA%\prdt\bin\、UpgradeCode 固定、Start Menu ショートカット)、`cargo-wix` 0.3 + WiX 3.14。`gui-host::update`(`self_update` 0.41 + GitHub Releases、7 日間隔、Settings に banner + Install)。3 GUI binary に `winres` で マルチ解像度 ICO + version resource。`docs/build-msi.md`。コード署名は G5。 |
| `phase4-g5-complete` | クラッシュレポータ + Authenticode 署名 scaffolding。`prdt_gui_common::crashlog`(`install_panic_hook` → `dirs::cache_dir()/prdt/crashes/<ts>-<bin>-<pid>.json`、`list_pending_crashes`(newest first)、`mark_acknowledged`(→ `acknowledged/`)、`register_tail` で TailHandle のログ行同梱、`truncate_for_display` で UTF-8 安全切詰)。3 GUI binary が `main()` で hook install。gui-host 起動時に pending 読込 → Settings の banner で表示 + Open folder + Acknowledge all。i18n 4 ID(crashlog-*)。`scripts/sign-msi.ps1`(signtool /tr RFC 3161 + SHA-256)、`docs/sign-and-release.md`(EV/OV cert 手順 + release checklist)。cert 購入は Phase 5 公開時。 |

---

## 2. 残タスク(優先順)

### **A. すぐ取れる、影響大、規模小**

#### A1. Plan 4 B1-B8 — 実機 2 台ベンチマーク行列(部分完了)
- **状態**: B1+B2+B5+fps 軸完了(`plan4-b1-bench-matrix-complete`、2026-04-26)。残 B3/B4/B6/B7/B8
- ~~**B1: 解像度マトリクス(1080p / 1440p / 4K)**~~ ✅
- ~~**B2: ビットレートマトリクス(5/10/20/30/50 Mbps)**~~ ✅
- **B3: コーデック比較(H.265 / 将来 AV1)** — NVENC AV1 サポート未実装(Ada Lovelace+ GPU 必要)
- ~~**B4: 経路比較(LAN / loopback / TURN relay)**~~ ✅ software-only 部分(2026-04-26、`plan4-b4-net-profile-bench-complete`、20 構成、`LoopbackOptions` で simulated latency + drop)。真の LAN/TURN は 2 台環境 + 外部 TURN server 必要、保留
- ~~**B5: デコーダ比較(MF / NVDEC)**~~ ✅
- ~~**B6: FEC 効果(k=8 / 32 / 64、m=2 / 6)**~~ ✅(2026-04-26、`plan4-b6-fec-bench-complete`、30 構成、recovery rate + reconstruct latency)
- ~~**B7: input round-trip latency(クリック→画面反映)**~~ ✅ software-only 部分(2026-04-26、`plan4-b7-input-load-bench-complete`、12 構成、send-to-recv lag)。真の click→画面 RTT は M3(カメラ)必要、保留
- ~~**B8: 長時間安定性**~~ ✅ (2026-04-26、`plan4-b8-stability-bench-complete`、`scripts/analyze-stability.py` で 30-min `prdt-bench-matrix` 出力を分単位 bucket 解析、drift / outlier 検出。実測: 1080p60 30Mbps NVDEC で 30 分間ドリフトほぼ 0、loss 0.001%)
- **B1+B2+B5 実機結果(RTX 3070 Ti、2026-04-26)**: bench-results/2026-04-26-final/(60 構成、全成功)
  - NVDEC が 29/29 paired 構成で MF より速い(median e2e_p50 ratio 0.83、17% 高速)
  - NVDEC: lower jitter (CV 0.286 vs 0.309)、lower loss (1930 vs 3857 ppm)
  - 1080p: 6.5ms、1440p: 10.6ms、2160p: 23ms 中央値 e2e_p50(NVDEC)
  - fps を 60→120 にしても e2e ほぼ変わらず(median ratio 0.99、encode 律速)
- **ブロッカー**: M3(カメラ実測)未着手のため真の glass-to-glass は取れず、`e2e = decode_done - capture` 近似値
- **見積もり**: 残 B4 (~3d、2 台 LAN)、B6 (~1d)、B7 (~2d、M3 と組み合わせ)、B8 (~1d)

#### ~~A2. Plan 2d optimization — NVDEC 真ゼロコピー~~ — **完了 (2026-04-25, `plan2d-zerocopy-complete`)**
- ~~CPU バウンス排除~~ → dual R8 + R8G8 D3D11 textures + CUDA-D3D11 interop 経由で達成
- ~~色変換~~ → 自前 HLSL pixel shader (BT.709 limited-range YUV→BGRA)
- 残り(将来 Plan 4 等で): DualCache のダブルバッファ化、HDR/10bit (P010)、BT.601 自動切替

### **B. 中規模、優先度中**

#### ~~B1. Phase 4 GUI(本格)~~ — **完了 (2026-04-25, `phase4-g5-complete`)**
- ~~spec~~ → `docs/superpowers/specs/2026-04-23-phase4-gui-design.md`(全体)+ G1〜G6 各 spec
- ~~G1: egui 基盤 + host GUI + viewer launcher~~ ✅
- ~~G2: viewer in-stream overlay (B1 別プロセス)~~ ✅
- ~~G3: tray + 通知 + auto-start~~ ✅
- ~~G4: MSI インストーラ + 自動アップデート~~ ✅
- ~~G5: crash reporter + Authenticode signing scaffolding~~ ✅(cert 購入は Phase 5)
- ~~G6: i18n (英/日)~~ ✅
- 合計 ~8 週分の作業を完了、Phase 4 完了

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
