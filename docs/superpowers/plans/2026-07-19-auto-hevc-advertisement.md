# Design v2: `--encoder auto` が FFmpeg HEVC を選択可能にする（P5C-lite）

Status: APPROVED-v2 (Fable draft → GPT-5.6 Sol REJECT → shared-resolver 対案採用)
Date: 2026-07-19

## Goal

Linux ホストで `--encoder auto` のとき、HEVC HW エンコード（VAAPI/NVENC）が
コンパイル済み・**ランタイム実証済み**なら H.265 セッションが成立すること。
既存の H.264 クライアントとの互換・byte-stable 規律（NPP/Main10 は auto 不参加）
は不変。

## v1 の欠陥（GPT-5.6 Sol レビューで棄却された点）

1. resolve_auto_encoder は feature + PRDT_PREFER_NVENC でしか選ばず、
   プローブ結果を見ない → 広告と構築が食い違い、構築失敗で host 即死。
2. av_hwdevice_ctx_create のみのプローブは HEVC encode entrypoint 欠如を
   検出できない（誤広告）。
3. --force-sw（handshake 後適用, lib.rs:955）が広告に反映されない。
4. auto 時に Main10 が広告され得る（_negotiation :112-118）が resolver は
   8bit のみ。
5. legacy FFmpeg 経路は X11ShmCapturer 直建て（linux.rs:161）で
   --capture-backend wayland が無視される。

## Decision: 単一の共有 auto-HEVC resolver

### 1. 共有 resolver（新設, crates/host/src/platform/linux.rs）

```rust
/// negotiated-codec H265 の auto 選択と handshake 広告の両方が参照する
/// 単一の決定点。プロセスごとに OnceLock キャッシュ。
fn resolve_hevc_auto_backend() -> Option<&'static str>
    // 戻り値: "ffmpeg-vaapi-hevc" | "ffmpeg-nvenc-hevc" | None
```

- 候補順: 既定 [vaapi, nvenc]、`PRDT_PREFER_NVENC=1|true|yes|on` で反転
  （resolve_auto_encoder の既存規約と同一）。
- 各候補は feature ゲート + **実構築プローブ**: 対応する
  `Hevc{Vaapi,Nvenc}FfmpegEncoder::new` を極小 cfg（320x180@30, 1Mbps,
  PRDT_VAAPI_RENDER_NODE 尊重）で構築→即 drop。frames ctx + avcodec_open2
  まで通るので entrypoint 欠如も検出（GPT 指摘 #2 の解、かつ新規 unsafe ゼロ）。
- 最初にプローブ通過した候補を Some で返しキャッシュ。

### 2. 広告（linux_supported_codecs / _negotiation の auto アーム）

```
"auto" =>
    if !force_sw
       && capture_backend_resolves_to_x11()   // 下記 §4
       && resolve_hevc_auto_backend().is_some()
    { vec![H265, H264] } else { vec![H264] }
```

- force_sw を広告経路に引数で配線（シグネチャ変更: 呼び出し元 lib.rs:897 に
  args.force_sw を渡す）。→ v1 欠陥 #3 解消。
- **auto 時は Main10 を広告しない**: _negotiation の Main10 追加は
  「明示 main10 エンコーダ指定時のみ」にゲート。→ 欠陥 #4 解消。
- H265 先頭 = ホスト優先（Windows auto の [H265, H264] と対称）。

### 3. 構築（resolve_auto_encoder の H265 アーム）

- cfg カスケード決め打ちを廃し、`resolve_hevc_auto_backend()` の
  キャッシュ結果をそのまま返す。広告と構築が同一の決定を共有 → 欠陥 #1 解消。
- キャッシュ済みなので handshake 内の追加コストなし。
- プローブ通過後に本構築が失敗する残余レース: warn ログ
  ("hevc auto probe passed but construction failed") + 正直なセッション失敗。
- H264 アームはバイト単位で不変（byte-stable 規律）。

### 4. capture-backend との整合（欠陥 #5）

- legacy FFmpeg 経路は X11 キャプチャ固定のため、**capture backend が
  X11Shm に解決されるセッションのみ auto-HEVC を有効化**する。
  Wayland portal 解決時は従来どおり H264 policy 経路（portal を尊重する）。
- Wayland で HEVC が欲しいユーザーは明示 `--encoder ffmpeg-vaapi-hevc`
  （既存挙動・キャプチャ制限をドキュメント化）。
- detect_capture_backend は既存関数を再利用し、広告時に1回だけ評価
  （結果は per-session に既存ログと一致すること）。

### 5. 非目標（v1 から不変）

- ScoringPolicy / BackendKind / ProducerConfig 拡張（P5C 本体）
- NPP / Main10 の auto 参加
- Windows 側変更
- codec hot-swap / FFmpeg 経路の Wayland キャプチャ対応（follow-up 起票のみ）

## Acceptance criteria

1. 本機で `--encoder auto --capture-backend x11` →
   `negotiated=H265`、`encoder_ready backend="ffmpeg-vaapi-hevc"`、
   ソークで frames 流通。
2. 同条件 + `PRDT_PREFER_NVENC=1` → `backend="ffmpeg-nvenc-hevc"`。
3. `--encoder auto`（Wayland 解決）→ 広告 [H264]、従来挙動。
4. `--encoder auto --force-sw` → 広告 [H264]、OpenH264。
5. HEVC feature なしビルド → 広告・挙動とも従来とバイト同一。
6. 単体テスト: 広告ロジック（probe/capture/force_sw を注入可能な形で分離）
   + resolve_auto_encoder H265 アームが resolver に委譲することのテスト。
7. 既存テスト全緑、dev-container rustfmt クリーン。

## Risks

- 極小構築プローブの初回コスト（VAAPI 数ms〜、CUDA 初期化 数百ms）。
  OnceLock + handshake 時1回のみで許容。プローブは encoder drop で
  リソース即解放。
- プローブ成功→本構築失敗の残余レースは正直な失敗として許容（warn 付き）。

## Hardware acceptance results (2026-07-19, Ryzen 9 9950X iGPU + RTX 4070 Ti, Arch/ffmpeg7.1)

| AC | Scenario | Result |
|---|---|---|
| 1 | `--encoder auto --capture-backend x11` | PASS — probe `vaapi_probe="ok"` → decision `ffmpeg-vaapi-hevc`, negotiated H265, encoder_ready==1 (probe silent), 781/781 frames |
| 2 | + `PRDT_PREFER_NVENC=1` | PASS — decision `ffmpeg-nvenc-hevc`, `nvenc_probe="ok"` |
| 3 | auto, Wayland-resolved capture | PASS — advertises [H264] only; H265 request rejected exactly as on master. Bonus: resolver correctly skipped failed VAAPI probe → NVENC (the review scenario) |
| 4 | `--encoder auto --force-sw` | PASS — zero probe invocations (lazy gate), negotiated H264, OpenH264 session flows |
| 5 | featureless build | PASS — dev-container default-features check clean, advertisement byte-identical |

Post-review fixes during acceptance: probe-side `encoder_ready` event silenced
via `new_inner(cfg, emit_ready_event)` split (kept smoke assertion count==1);
lazy probe evaluation (force-sw/Wayland sessions never probe).

Follow-up (not in scope, observed during AC1 first attempt): viewer
`--decoder auto` hard-fails when its first pick (VAAPI) cannot init instead of
falling back to nvdec/sw — candidate for a viewer-side retry cascade.
