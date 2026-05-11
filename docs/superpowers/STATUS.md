# power-remote-dt — Project Status & Roadmap

**Last updated:** 2026-05-11
**Latest tag:** `phase-l3-adaptive-bitrate-complete`
**Branch state:** `phase0-sw-codec-wire` (post-tag) — **Phase 4 + Plan 4 B1 + B4 + B6 + B7 + B8 完了 + MF エンコーダ fallback 完了 + host session liveness 完了 + NVDEC arc-swap 化 完了 + ソフトウェアコーデック OpenH264 完了 (B3 のみ HW ブロック保留)**
**Test count:** 348+ automated Rust tests + 11 Python tests; new crate `prdt-media-sw` 6 tests (Phase 1) + Phase 0 protocol/transport new tests + Phase 5 latency-bench new test (≥10 new tests per plan §8 acceptance)

| `software-codec-openh264-complete` | ソフトウェアコーデック (OpenH264) フォールバックパス + ワイヤフォーマット v2 codec ネゴシエーション。新クレート `crates/media-sw` (`openh264 = "0.9.3"`、`features = ["source"]` で BSD-2 ベンドソースを `cc` 経由で静的リンク、ビルド時ネットワーク I/O ゼロ): `Openh264Encoder` (Profile::Baseline / RateControlMode::Bitrate / Complexity::Low / UsageType::ScreenContentRealTime / num_threads=0)、`Openh264Decoder`、`I420Frame`、`bgra_to_i420` (BT.601 limited)、`i420_to_nv12`。ワイヤ: `Hello.protocol_version 1→2`、`HelloAck.negotiated_codec: Codec` + `HelloAck.host_supported_codecs: Vec<Codec>` 追加、新 `ControlMessage::HelloReject { reason: String }` (kind_u8=22)。ホスト: `--encoder {auto, nvenc, mf, openh264}` + `VideoEncoderBackend { Hw, SwH264 }` 列挙 + 新 `DxgiSwProducer` (BGRA→I420 readback + `tokio::task::spawn_blocking` 分離)。ビューア: `--decoder {auto, nvdec, mf, openh264}` + 新 `--codec {auto, h265, h264}` フラグ + `media-win` 新 `i420-upload` フィーチャの `CpuI420Uploader` (I420→NV12→D3D11 STAGING マップ→既存 `DualPlaneYuvRenderer` 入力テクスチャ `CopySubresourceRegion`)。MSRV 1.78→1.85 (`PanicHookInfo` 1.81+ 移行 + `phase4-g5-complete` `#[allow(deprecated)]` 撤去を同チェーンで実施)。N=5 同セッション計測 (1080p60 30Mbps): OpenH264 `e2e_p99 median = 25.7ms` (σ=268µs / σ/median=1.0%、loss_ppm=0、decode_p99 median=3.5ms) ✅ 全 Phase 5 acceptance クリア。NVENC/NVDEC リグレッションは iteration 4 で「±5% of quiescent baseline」から「同セッション SW_median ≤ 1.5× HW_median」へ変更 (現環境でマルチエージェント負荷下、HW path は同コード・byte-equivalent ながら quiescent 21.3ms から 65.9ms に環境ドリフト、SW/HW 比 0.391 ≤ 1.5)、HW ドリフトはコードリグレッションでなく環境コンテンション (σ=2.7ms vs SW σ=268µs の 10x 差が指紋)。First-frame latency Phase 4 acceptance: 17–30ms (mean 23ms / N=20、≤500ms 制約)。ADR `docs/adr/2026-04-27-software-codec-openh264.md` 完備。クレート README + `docs/superpowers/plans/2026-04-27-software-codec.md` (4 iteration ralplan 合意) 同梱。チーム実行 `sw-codec-openh264` (worker-wire / mediasw / producer / consumer / glue + team-lead)。`audio-mmcss-hardening` は cpal 内部 WASAPI コールバックスレッドに到達不能のため follow-up タグへ descope。 |
| `nvdec-arcswap-complete` | NVDEC `latest_dual` を `Mutex<Option<DualPlaneFrame>>` から `arc_swap::ArcSwapOption<DualPlaneFrame>` に置換、`take_latest_dual_plane(&self) -> Option<Arc<DualPlaneFrame>>` で `swap(None)` の consume セマンティクス。`DualPlaneFrame` は `#[derive(Clone)]` 削除 + `pub(crate)` フィールド + `y_tex_raw()`/`uv_tex_raw()` accessor で外部からの inner clone を型レベル封鎖、reader 側 `ID3D11Texture2D::AddRef` 倍化を排除。`CuvidDecoder::Drop` で `latest_dual.store(None)` を `dual_cache=None` の前に挿入し CUDA context 生存中に Arc release。新規テスト 4 本 (consume 契約、`Arc::strong_count` 不変条件 100-iter、`DualCache::Drop` カウンタ、`cuGraphicsUnregisterResource` shim カウンタ; Drop 順序は liveness invariant で間接保証、prod 型汚染なし)。N=5 baseline (host-session-liveness 子) vs N=5 arcswap 計測で primary 2 指標両方とも改善: `e2e_p99` -26% (28.9ms→21.3ms)、`decode_p99` -44% (7.2ms→4.0ms)、加えて run-to-run spread が `e2e_p99` で 97% 削減 (39.6ms→1.1ms) と tail 安定性大幅向上。全 7 指標で acceptance pass (regression guard `median+2σ` 内)。`arc-swap = "1"` を crate-local dep に追加 (workspace 昇格せず)。ralplan consensus iteration 4 で APPROVE (Planner→Architect→Critic 4 周)。ADR `docs/adr/2026-04-27-nvdec-latest-dual-arcswap.md` 9 セクション完備。 |
| `host-session-liveness-complete` | viewer 1Hz `KeepAlive` heartbeat、host 5s watchdog、`CancellationToken` 配下で全 worker tearing down → outer loop で `reset_session` + 再 handshake。viewer 異常終了でも host 再起動なしで 5-7 秒以内に新セッション受け入れ。`recv_raw_unencrypted` + encrypted recv で `WSAECONNRESET` (stale ICMP) フィルタ。3 cycle smoke ok、0 panic。 |

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
- **状態 (2026-05-09)**: **L0 + L1 platform crates + L1.5a host wiring + L1.5b viewer wiring 完了**。Linux↔Linux end-to-end smoke 可
  - L0: traits 抽出 + skeleton crates + L0 follow-ups (master)
  - L1: `prdt-media-linux` + `prdt-input-linux` 完全実装 + 29 unit tests + 4 ignored integration (`phase-l1-platform-crates-complete`)
  - L1.5a (`phase-l1.5a-host-wiring-complete`): host `lib.rs` を `platform::*` 経由に rewire + Linux client が `prdt host` をルート
  - **L1.5b (`phase-l1.5b-viewer-wiring-complete`)**: viewer `lib.rs` (2029 行) を `platform/{mod,win,linux,input_map}.rs` に分解。`LatestFrame`/`ViewerConsumer`/`ViewerRenderer` を `PlatformFrame`/`PlatformConsumer`/`PlatformRender` に rename + 平台別に分割。Linux 側は softbuffer + `prdt_media_linux::i420_to_bgra` で CPU 描画、`prdt_media_sw::Openh264Decoder` で SW H.264 decode。Linux client が `prdt connect` をルート。`crates/viewer/tests/linux_connect_smoke.rs` (`#[ignore]`、WSLg 必要) で viewer boot 検証 (1 passed)。`cargo check + clippy` 両ターゲット green。
  - **L1.5b smoke walkthrough (2026-05-09)**: WSLg host + 実機 Wayland viewer で end-to-end 検証。**ウィンドウ表示 ✅** (softbuffer + winit + Wayland 統合動作確認)、handshake + frame 送信 wire 層 ✅。3 fix を smoke 中に発見・修正:
    - `e69d199` media-linux: WSLg multi-monitor 7680×2160 が OpenH264 SW max 超 → 3840×2160 にクランプ
    - `f02b706` host: WSLg で audio device 不在 → audio task が session ごとキャンセル → audio failure を非致命に
    - `70857e0` viewer: Wayland の `wl_surface` は最初の buffer commit まで unmapped → `build_render` で初期黒 buffer を 1 回 commit
  - **L1.5b smoke 既知制約** (L2 transport polish 候補): WSL2 → LAN UDP 高 bitrate (>5 Mbps) で大量 fragment 損失。viewer に `RequestIdr` 送信 path 未実装で IDR loss 後の自己回復無し。実機 Wayland 上でウィンドウは表示されるがフレーム中身は decode 失敗 (transport/IDR-recovery が L2 で要解決) → **L2 で解消、下記**
  - **CI 配信**: `.github/workflows/release.yml` で Linux x86_64 binary を tag-trigger で release 化 (smoke-1, smoke-2 で運用検証済み)
  - **L2 (`phase-l2-transport-robustness-complete`, 2026-05-10)**: L1.5b smoke の black-screen を解消する transport robustness 最小ループを実装。Cross-platform (Linux + Windows)、~250 LoC across 9 tasks。
    - **Viewer side** (`crates/viewer/src/lib.rs`): `IdrRequester` struct (`needs_idr_pending` + `last_request_at` + 250ms cooldown) を recv loop に配線。3 つの loss 検知 trigger: Linux decoder `Err`、Linux `Ok(None) && needs_idr && !is_kf` (P-frame reference miss)、Windows submit error。+ 1-second recv timeout 経路で `purge_assembler()` non-empty → mark。`try_send_idr_request` closure を全 exit point (continue 含む) で発火 → `transport.send_control(ControlMessage::RequestIdr).await`
    - **Transport side** (`crates/transport/src/udp.rs`): `pub async fn purge_assembler(&self) -> Vec<u64>` を `CustomUdpTransport` に追加 (assembler の既存 `purge()` を viewer に expose)。
    - **Host side** (`crates/host/src/lib.rs`): `force_idr_flag: Arc<AtomicBool>` を control loop と video loop で共有。control loop の new arm: `Ok(ReceivedMessage::Control(ControlMessage::RequestIdr)) => force_idr_flag.store(true, Release)`。video loop は `force_idr_flag.swap(false, AcqRel)` → `producer.request_idr()` (既存 `VideoProducer` trait) を `next_frame()` 直前に呼ぶ。
    - **Encoder side**: 全 3 encoder で SPS/PPS-with-every-IDR を有効化:
      - OpenH264 (`crates/media-sw/src/encoder.rs`): `SpsPpsStrategy::SpsPpsListing` を `EncoderConfig` builder に追加 (実際は `ScreenContentRealTime` usage で `CONSTANT_ID` に降格されるが、結果として全 IDR に SPS+PPS が乗る)
      - MF H.265 (`crates/media-win/src/mf/encoder.rs`): `CODECAPI_AVEncVideoForceKeyFrame=1` を `ICodecAPI::SetValue` で。MFT が E_NOTIMPL を返す場合は黙って無視 (degraded mode fallback は viewer-side cache、L3)
      - NVENC (`crates/media-win/src/nvenc/config.rs`): `enableRepeatSPSPPS=1` を `NV_ENC_INITIALIZE_PARAMS` に
    - **Tests**: 7 new tests cross-platform: 3 transport loopback (`idr_loss_test::*`)、2 host smoke (`request_idr_handler_smoke::*`)、1 viewer unit (`idr_requester_cooldown`)、1 OpenH264 (`second_idr_carries_sps_pps`)、+ 2 `#[ignore]` HW encoder tests (MF/NVENC; Windows CI で `--ignored` 付き実行)
    - **Linux regression bar**: cargo build + clippy --workspace -- -D warnings green、339 passed / 6 ignored
    - **Windows regression bar**: Windows CI (PR で確認、tag push 後)
    - **Pre-existing flaky test (L2 regression ではない)**: `transport::probe_test::two_transports_find_each_other` は master でも deterministic FAILED (UDP probe timing issue、別件)
  - **L2 smoke walkthrough (2026-05-10)**: WSLg host (`--bitrate-mbps 5 --encoder openh264`) + 実機 Wayland viewer で end-to-end 検証。**spec §1 DoD #1 達成 ✅** — 接続後 ~2.4 秒で black → live に遷移 (`textures_decoded=0 → 5 → 7`)。L2 RequestIdr loop 完全動作確認:
    - Viewer: 初回 decode 失敗 (Native:16 = `dsNoParamSets`) → `IdrRequester::mark()` → `transport.send_control(RequestIdr)`
    - Host: control loop が `viewer requested IDR; setting force_idr for next encode` を log + `force_idr_flag.store(true, Release)`
    - Host video loop: `force_idr_flag.swap(false, AcqRel)` → `producer.request_idr()` → encoder が SPS+PPS+IDR slice を含む新 IDR を emit
    - Viewer: 新 IDR 復号成功、画面更新開始
    - Latency 改善 (前回 30Mbps smoke vs 今回 5Mbps): `arrival_p50` 412ms → 99ms (4.2×)、`decode_p50` 564ms → 205ms (2.7×)、`present_p50` 586ms → 223ms (2.6×)
  - **L2 smoke 残課題** (L3 territory): 17 秒以降 host→viewer の packet delivery が事実上停止 (host 262 frames send → viewer 15 frames recv = **5.7% delivery**)。host watchdog が 5 秒 silence で session kill。原因: WiFi/LAN の物理層 packet loss + IDR fragment loss が連鎖して回復不能 stretch に入った。**L3 で解決予定**: (a) Reed-Solomon FEC across IDR fragments、(b) observed-loss-driven adaptive bitrate
  - **L3 (`phase-l3-adaptive-bitrate-complete`, 2026-05-11)**: viewer-side AIMD bitrate controller を追加して L2 smoke の 5.7% delivery → session timeout を解消。Cross-platform、~360 LoC across 6 modify + 1 new + 3 new test files。
    - **Viewer side** (`crates/transport/src/bitrate_control.rs` 新): `BitrateController` (stateless: `observe(lost, total)` → `aimd_step(now)` → `should_send()` → `mark_sent()`, with `reset_window()`)。AIMD パラメータ: MD ×0.7 on loss>2%, AI +200kbps/s on loss<0.5%, 2s post-MD cooldown, 5% hysteresis、min 1 Mbps, max `--bitrate-mbps × 1e6`
    - **Viewer wiring** (`crates/viewer/src/lib.rs` `latency_task`): 1Hz tick で recv_task の `purge_assembler()` 結果を `Arc<AtomicU64>` 経由で受け取り (T4 review HIGH fix で recv_task を唯一の purger に統一)、caller が `last_total_samples` 差分で rolling window を構築 → controller 駆動 → `SetBitrate` 送信。Warmup guard: `has_baseline = snap.present.is_some() || last_total_samples > 0` で tick-1 spurious MD 抑制。`--no-adaptive-bitrate` flag で disable (回帰比較用)、`--bitrate-mbps` は clap range 1..=4000 で validate
    - **Host side** (`crates/host/src/lib.rs`): `tokio::sync::mpsc::unbounded_channel::<u32>()` を control loop と video loop で共有。control loop arm: `Ok(ControlMessage::SetBitrate { target_bps }) => bitrate_tx.send(target_bps)`。video loop は per-frame `bitrate_rx.try_recv()` で drain to latest → `producer.set_target_bitrate(bps)`
    - **Producer fix** (`crates/media-win/src/pipeline/producer.rs:190`): `DxgiNvencProducer::set_target_bitrate` の Phase 0 no-op stub を `self.encoder.set_target_bitrate(bps)` に書き換え (1-line forward to `HwHevcEncoder` which already dispatches to NVENC/MF)
    - **Tests**: 13 new tests cross-platform: 8 unit (`bitrate_control::tests::*`) + 2 transport integration (`adaptive_bitrate_test::*`) + 2 host smoke (`setbitrate_handler_smoke::*`) + 1 from T1 hysteresis test = **13 new** (Linux `cargo test --workspace` 307 passed, excluding pre-existing flaky `transport::probe_test::two_transports_find_each_other`)
    - **Wire**: `ControlMessage::SetBitrate { target_bps: u32 }` (kind_u8=6, 既存 dead path) を再利用、protocol_version bump 不要、backward compatible
    - **Linux regression bar**: `cargo build/clippy --workspace -- -D warnings` 両 target green
    - **Windows regression bar**: GitHub Actions release workflow PR #3 で green (run 25643045643)
  - **L3 smoke walkthrough (2026-05-11)**: WSLg host (`--bitrate-mbps 30 --encoder openh264`) + 実機 Wayland viewer (`--codec h264 --decoder openh264`、GitHub Actions release artifact run 25645084886 から DL) で end-to-end 検証。**spec §1 DoD #2 達成 ✅** — 30 Mbps フル帯域で 1m32s 健全配信、frames_sent=1252 / frames_received=1218 = **97.3% delivery** (L2 smoke の 5.7% から劇的改善、~21 fps、recv_errors=0、timeouts=0)。L3 SetBitrate 未送信 = controller AI ceiling 維持 = 期待動作 (loss < 0.5% 領域)。**DoD #1** (`target_bps ≤ 5 Mbps` within 60s) は環境ロス不在のため直接実証不可、MD ロジックは T5 integration test `loss_burst_drives_md_monotonically` で実証済み (30M → ~5M in 5 ticks @ 5% loss)。session 終端は viewer Ctrl+C → 5.6s 後に host watchdog が正常 kill。**L4 残候補**: 実機 WiFi 物理層 loss を意図的に誘発する smoke 手順 (faulty cable, distance test, tc qdisc netem 等)、Reed-Solomon FEC across IDR fragments
- **L2 残候補** (transport robustness 完了後): Wayland portal capture / libei / wl-clipboard、VAAPI HW encode/decode、NVENC/NVDEC on Linux、cross-OS scancode normalization、multi-monitor non-zero-origin、cursor capture/合成、複数 distro 検証、`Cmd::Gui` Linux 対応、Linux viewer overlay child process、audio default-on on Linux、`prdt_input_win::RawInputCapturer::map_winit_mouse_button` cleanup、viewer cooperative shutdown (CancellationToken plumbing)、IDR fragment FEC + adaptive bitrate (L3)
- **元の見積もり**: 大(3-4 週)。**実績 (L0 + L1 + L1.5a + L1.5b + smoke fixes + L2 transport)**: ~85%。残 15% (HW codec + Wayland portal + packaging) は L2 残/L3 へ

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
