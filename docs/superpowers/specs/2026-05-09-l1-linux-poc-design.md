# L1 — Linux PoC Design

**Date:** 2026-05-09
**Status:** Draft v2 (post critic review, awaiting user approval)
**Predecessor:** L0 Trait Extraction (`docs/superpowers/plans/2026-05-08-l0-trait-extraction.md`, status `2026-05-08-l0-trait-extraction-status.md`)
**Approach:** Approach 1 — `#[cfg]` gate + Linux 用 `VideoProducer` adapter(`DxgiSwProducer` の Linux 双子)を新設。host main loop 本体ロジックは無改変、ロジックの境界は既に `VideoProducer` trait + `prdt_input_win` の free function 群で確立しており、Linux ではその同形を `media-linux` / `input-linux` 側に揃える

---

## 1. Goal

WSL2 Ubuntu 上で `prdt host` と `prdt connect` が起動し、capture → encode (CPU) → 暗号化転送 → decode (CPU) → softbuffer 表示と、viewer→host 方向の input inject + clipboard sync が end-to-end で動く Linux ビルドを作る。L0 で抽出した `prdt-media-core` / `prdt-input-core` の trait を初めて Linux backend で満たし、Windows 側コードと共存させる。

## 2. Non-goals

- Wayland native 対応(L2)
- HW encode/decode (VAAPI / NVENC / NVDEC) (L2 以降)
- 実機 Linux マシンでのベンチマーク / glass-to-glass / multi-distro 検証 (L2 以降)
- パッケージング (deb / AppImage / Flatpak、Phase 5 相当)
- viewer の D3D11 と softbuffer を統合する大規模リファクタ (L3 候補)
- ホスト側 audio が任意 ALSA/PulseAudio 環境で完全動作することの保証
- autostart `~/.config/autostart/*.desktop` 実装
- クロスコンパイル経路(Windows→Linux 等)。すべて native build (WSL2 上で `x86_64-unknown-linux-gnu` ターゲット)

## 3. Background

L0 で次の trait + skeleton が master に landed 済み:

- `prdt-media-core::{Capturer, Encoder, Decoder}` + `EncodedPacket`(`EncodeError::DeviceLost` も追加済み)
- `prdt-input-core::{InputInjector, ClipboardProvider, VirtualDesktopGeometry}`
- `crates/media-linux/`、`crates/input-linux/` は `#![cfg(target_os = "linux")]` の空 lib として workspace に組み込み済み
- L0 follow-ups (host clippy 8 件 / dirs ベース key path) も別コミットで反映済み

ただし host/viewer のコードは現状すべて `#[cfg(windows)]` 配下、Linux build target で `cargo check` するとリンク段階で機能不足。L1 でその欠片を埋める。

### 既存 host/viewer の wiring 実態(L1 spec の前提)

実装にあたり以下の事実を spec として固定する(critic review v1 で発見):

1. **video pipeline の境界は `prdt_protocol::VideoProducer` (async trait)**。`crates/protocol/src/video_pipeline.rs:33-44`、`async fn next_frame() -> Result<EncodedFrame, ProducerError>` + `request_idr()` + `set_target_bitrate()`。Windows では `DxgiSwProducer`(`crates/host/src/dxgi_sw_producer.rs`) と `DxgiNvencProducer` が impl。L1 Linux は同 trait を impl する `LinuxSwProducer` を追加する
2. **input/clipboard/geometry の境界は free function**。`crates/host/src/lib.rs:20` で `prdt_input_win::{clipboard_sequence_number, read_clipboard_text, virtual_desktop_rect, write_clipboard_text}` 等を直接 import。**L0 trait 経由ではない**。L1 では `prdt_input_linux` が同名の free function を public に提供し、host 側は `#[cfg]` で import 切替
3. **L0 trait 実装 (`core_adapter.rs`) は unit test surface 兼 L2/L3 用予備**。L1 production wiring は free function + `VideoProducer` 経由で、L0 trait に到達する path は production にはない。これは L0 status doc § Not delivered の「Host / viewer code is not rewired through the new traits」とも一致
4. **viewer の input scancode は `winit::platform::scancode::PhysicalKeyExtScancode::to_scancode(key)`**(`crates/viewer/src/lib.rs:87,955`)。返り値は **viewer-OS native scancode** で、Windows なら PS/2 Set 1、Linux なら Linux evdev `KEY_*`。`crates/protocol/src/input.rs:30` の docstring「host-OS-native scancode」とは現状不整合だが、これは L1 では修正しない既知 wire-protocol 課題

cross-platform 想定 crate(`prdt-protocol`、`prdt-transport`、`prdt-crypto`、`prdt-signaling-*`、`prdt-nat-traversal`、`prdt-audio`、`prdt-filetransfer`、`prdt-media-sw`)は L0 までで Windows 側だけ build 検証済みで、Linux build 検証は L1 のスコープ。

## 4. Architecture overview

```
                 ┌────────────────────────────────────────┐
                 │  prdt unified bin (crates/client)      │
                 │  Linux: Cargo.toml に linux dep block  │
                 └────────────────────────────────────────┘
                               │
              ┌────────────────┴────────────────┐
              ▼                                 ▼
     ┌──────────────────┐              ┌──────────────────┐
     │  prdt-host (lib) │              │  prdt-viewer     │
     │  main loop 不変  │              │  (lib)           │
     │  use 切替のみ    │              │  render 切替のみ │
     └──────────────────┘              └──────────────────┘
              │                                 │
   prdt-protocol::VideoProducer (async)         │
   prdt_input_{win,linux} free functions        │
              │                                 │
   ┌──────────┴──────────┐         ┌────────────┴─────────────┐
   ▼                     ▼         ▼                          ▼
prdt-media-linux  prdt-input-linux  prdt-media-sw     softbuffer + winit
 ├─ x11_capture    ├─ uinput_inj.    (Openh264         (CPU framebuffer
 ├─ sw_pipeline    ├─ x11_clipboard   encode/decode    presentation)
 ├─ linux_sw_      ├─ x11_geometry    cross-platform)
 │   producer      ├─ free fns
 │   (impl Video-  │  (host が直接
 │    Producer)   │   呼ぶ surface)
 ├─ i420_to_bgra   └─ core_adapter
 ├─ free fns          (impl L0
 ├─ core_adapter      input traits、
 │   (impl L0         unit-test)
 │    media traits、
 │    unit-test)
   │                     │                  │
   └─── prdt-media-core ─┴──── prdt-input-core ─┘ ← L0 trait surface
                                                   (unit test 兼 L2/L3 予備)
   │
   └── cross-platform: prdt-protocol / prdt-transport / prdt-crypto /
       prdt-audio (cpal) / prdt-filetransfer / prdt-signaling-* /
       prdt-nat-traversal
```

Windows path は完全に保持(D3D11 + DXGI + NVENC + NVDEC + MF + WASAPI + `DxgiSwProducer`)。Linux path は **同 `VideoProducer` trait + `prdt_input_*` free function 形を取る並列系統** として `#[cfg(target_os = "linux")]` で gate される。host main loop はどちらの系統でも同形に動く。

## 5. Crate-level changes

### 新規実装が入る既存 crate

| crate | 状態 | 変更概要 |
|---|---|---|
| `crates/media-linux/` | 空 → 実装 | XShm capture + media-sw 経由 encode/decode + L0 trait adapter + i420→bgra helper |
| `crates/input-linux/` | 空 → 実装 | uinput inject (mouse + keyboard) + scan code table + X11 clipboard + X11 RandR geometry + L0 trait adapter |

### Cargo.toml 変更

| crate | 変更 |
|---|---|
| `crates/media-linux/Cargo.toml` | `[dependencies]` に `prdt-media-sw = { path = "../media-sw" }`、`x11rb = { workspace = true }`、`thiserror = { workspace = true }`、`tracing = { workspace = true }` を追加 |
| `crates/input-linux/Cargo.toml` | `[dependencies]` に `x11rb = { workspace = true }`、`nix = { workspace = true, features = ["ioctl", "fs"] }`、`thiserror = { workspace = true }`、`tracing = { workspace = true }`、`once_cell = "1"`(production state 用 OnceCell) を追加 |
| `crates/host/Cargo.toml` | `[target.'cfg(target_os = "linux")'.dependencies]` block 追加(`prdt-media-linux`、`prdt-input-linux`、`prdt-media-sw`、`prdt-gui-host`、`prdt-gui-common`、`tokio-util`、`async-trait`) |
| `crates/viewer/Cargo.toml` | 同上 + `softbuffer = { workspace = true }`、`prdt-gui-viewer`、`prdt-gui-common`、`prdt-media-sw` |
| `crates/client/Cargo.toml` | 同上(`prdt-host`、`prdt-viewer`、必要なら `prdt-gui-client`) |
| `Cargo.toml` (workspace) | `[workspace.dependencies]` に `x11rb = "0.13"`、`softbuffer = "0.4"`、`nix = "0.29"` を追加 |

### ソース変更

| crate | 変更 |
|---|---|
| `crates/host/src/` | `platform/{mod.rs, win.rs, linux.rs}` を新設。`mod.rs` で `#[cfg]` 分岐、`win.rs` は現行 `#[cfg(windows)]` ブロック相当を移動 or 残置(差分最小)、`linux.rs` を新規作成 |
| `crates/viewer/src/` | 同上 + Linux 用 render path モジュール(softbuffer + i420→bgra、~150-200 行) |
| `crates/client/src/main.rs` | サブコマンド dispatch を `#[cfg(any(windows, target_os = "linux"))]` に拡張 |
| 他 | `prdt-audio` / `prdt-filetransfer` / `prdt-gui-*` / `prdt-protocol` / `prdt-transport` / `prdt-crypto` / `prdt-signaling-*` / `prdt-nat-traversal` は **コード変更なし**、Linux build 検証のみ。問題発生時のみ最小修正 |

### workspace dep 追加

| crate | 用途 | 想定バージョン |
|---|---|---|
| `x11rb` | X11 protocol(XShm + RandR + XFixes clipboard watcher) | `0.13` |
| `nix` | `/dev/uinput` ioctl 直接呼び出し(stale `uinput` クレート回避) | `0.29` |
| `softbuffer` | viewer の CPU framebuffer presentation | `0.4` |
| `once_cell` | `input-linux` 内部の lazy global state(uinput device + clipboard watcher) | `1` |

`x11rb`、`softbuffer`、`nix` は workspace 共通 dep として宣言(将来別 crate から使う可能性あり)。`once_cell` は `input-linux` crate-local。

## 6. Module internals — `crates/media-linux/`

```
src/
  lib.rs              -- pub modules + re-exports (#![cfg(target_os = "linux")])
  error.rs            -- LinuxMediaError + 外向きマッピング helper
  frame.rs            -- BgraFrame { width, height, stride, bgra: Vec<u8>, capture_ts_us: u64 }
  x11_capture.rs      -- XShmGetImage 経路 + plain XGetImage fallback
  sw_pipeline.rs      -- BgraFrame → I420 → Openh264Encoder / Openh264Decoder ラッパ
  i420_to_bgra.rs     -- I420 → BGRA helper (BT.709 limited、~50 行、softbuffer から呼ばれる)
  linux_sw_producer.rs -- impl prdt_protocol::VideoProducer (production wiring path、host が直接 import)
  core_adapter.rs     -- prdt_media_core::{Capturer, Encoder, Decoder} の impl + builder
                         (unit test surface 兼 L2/L3 予備、L1 production では未使用)
```

**設計の要点**: production wiring は `linux_sw_producer.rs` の `LinuxSwProducer` を経由する。これは Windows の `crates/host/src/dxgi_sw_producer.rs` と一対一の双子関係。Linux 版は `crates/host/src/` ではなく `crates/media-linux/` 内に置く理由:

- `crates/media-linux/` は既に `#![cfg(target_os = "linux")]` で gate 済み → host/src/ に Linux-only ファイルを増やさない
- Windows の `dxgi_sw_producer` が host/src に住む理由は「media-win が media-sw に依存しないようにする」制約由来(`dxgi_sw_producer.rs:5-7`)で、Linux ではその制約がなく `media-linux` が直接 `prdt-media-sw` に依存可能
- capture + encode + producer wrapper を 1 crate 内で完結できる(L0 trait と producer trait の双方を満たす実装が同じファイル系で書ける)

### `x11_capture.rs`

- `x11rb::connection::RustConnection` で X server に接続(default `:0`)
- root window を取得し `Geometry { x, y, width, height }` を読む
- MIT-SHM extension を probe(`Setup` の `extensions` または `xcb_query_extension`)
  - 利用可: `shmget(IPC_PRIVATE, w*h*4, 0o600 | IPC_CREAT)` → `xcb_shm_attach` でサーバ側 attach、フレーム取得は `xcb_shm_get_image`(BGRA、24bpp depth、TZ_PIXMAP format)
  - 利用不可: `xcb_get_image` の plain 経路 + warn ログ。WSL2 で起こりうるが PoC 範囲では性能を妥協
- `Drop` 時に `shm_detach` + `shmctl(IPC_RMID)` + `shmdt` を順番に
- `BgraFrame` を `Vec<u8>` で返す(stride = width * 4 想定、padding は L1 では考慮しない)
- `capture_ts_us` は `Instant::now()` の monotonic マイクロ秒(host の latency 計測用)

### `sw_pipeline.rs`

- `LinuxSwEncoder { inner: Openh264Encoder, scratch_i420: I420Frame }`
  - `encode(&BgraFrame, force_idr, ts) → EncodedPacket`:
    1. `prdt_media_sw::bgra_to_i420(&frame.bgra, w, h, &mut self.scratch_i420)`
    2. `self.inner.encode(&scratch_i420, force_idr, ts)`
- `LinuxSwDecoder { inner: Openh264Decoder }`
  - `decode(&EncodedPacket) → Result<Option<I420Frame>, DecodeError>`(I420 のまま返す、softbuffer 直前で BGRA に変換)

### `i420_to_bgra.rs`

- BT.709 limited-range YUV → BGRA(viewer の windows D3D11 path と一致する color matrix)
- naive impl で十分(L1 PoC、最適化は L2 以降)
- 入力: `&I420Frame`、出力: `&mut [u8]` (softbuffer の buffer)
- 1 ピクセルあたりの計算: `Y + 1.5748*(V-128)` 等の固定小数 / 浮動小数(コードは shortest-readable で良い)

### `linux_sw_producer.rs`(production wiring、`VideoProducer` 実装)

`DxgiSwProducer` を mirror した構造。差分は (a) capture 経路が DXGI Desktop Duplication ではなく XShm、(b) blocking 取得 API がないため明示的な frame pacing が必要。

```rust
pub struct LinuxSwProducer {
    capture: x11_capture::X11ShmCapturer,           // 自前の capture (BgraFrame 出力)
    encoder: Option<prdt_media_sw::Openh264Encoder>, // take/return パターンで spawn_blocking 経由
    bgra_buf: Vec<u8>,                              // capture 用再利用バッファ
    pacer: tokio::time::Interval,                   // 60 fps なら Duration::from_micros(16_667)
    seq: u64,
    idr_pending: bool,
    width: u32,
    height: u32,
}

impl LinuxSwProducer {
    pub fn new(width: u32, height: u32, bitrate_bps: u32, fps: u32) -> anyhow::Result<Self> {
        let capture = x11_capture::X11ShmCapturer::new(width, height)?;
        let encoder = prdt_media_sw::Openh264Encoder::new(/* ... config ... */)?;
        let pacer = {
            let mut i = tokio::time::interval(Duration::from_micros(1_000_000 / fps as u64));
            i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            i
        };
        Ok(Self { capture, encoder: Some(encoder), bgra_buf: vec![0u8; (width * height * 4) as usize],
                  pacer, seq: 0, idr_pending: true, width, height })
    }
}

#[async_trait::async_trait]
impl prdt_protocol::VideoProducer for LinuxSwProducer {
    async fn next_frame(&mut self) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        loop {
            self.pacer.tick().await;  // 60Hz pace; XShm に blocking acquire がないため
            // capture (sync, 高速) — エラー時は Capture(...) variant 返却
            self.capture.grab_into(&mut self.bgra_buf)
                .map_err(|e| prdt_protocol::ProducerError::Capture(e.to_string()))?;
            let bgra = std::mem::take(&mut self.bgra_buf);
            let width = self.width; let height = self.height;
            let force_idr = std::mem::take(&mut self.idr_pending);
            let ts_us = prdt_protocol::now_monotonic_us();
            // encoder を spawn_blocking に move、結果と一緒に戻す(DxgiSwProducer と同形)
            let mut enc = self.encoder.take().expect("encoder taken twice");
            let join = tokio::task::spawn_blocking(move || {
                let i420 = prdt_media_sw::bgra_to_i420(&bgra, width, height, width * 4);
                let result = i420.and_then(|i| enc.encode(&i, force_idr, ts_us));
                (enc, bgra, result)
            }).await
                .map_err(|e| prdt_protocol::ProducerError::Other(format!("spawn_blocking: {e}")))?;
            let (enc_back, bgra_back, encode_result) = join;
            self.encoder = Some(enc_back);
            self.bgra_buf = bgra_back;
            let frame = encode_result.map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            let seq = self.seq; self.seq += 1;
            return Ok(prdt_protocol::EncodedFrame { seq, ..frame });
        }
    }
    fn request_idr(&mut self) { self.idr_pending = true; }
    fn set_target_bitrate(&mut self, bps: u32) {
        if let Some(enc) = self.encoder.as_mut() { enc.set_target_bitrate(bps); }
    }
}
```

### `lib.rs` free functions(host が直接 import する production surface)

`prdt-input-win` が `clipboard_sequence_number` 等を free function として公開しているのに対応する形で、`prdt-media-linux` も builder を free function として公開:

```rust
pub fn build_video_producer(width: u32, height: u32, bitrate_bps: u32, fps: u32)
    -> anyhow::Result<linux_sw_producer::LinuxSwProducer> { ... }

pub fn build_video_decoder()
    -> anyhow::Result<sw_pipeline::LinuxSwDecoder> { ... }
```

host は `#[cfg(target_os = "linux")] use prdt_media_linux::build_video_producer;` で取り込む。

### `core_adapter.rs`(unit test surface、L2/L3 予備、production では未使用)

L0 trait を満たす impl は production には不要だが、unit test の安定 surface として、また L2 で trait-object refactor を選んだ際の入口として残す。テスト以外から import されることを想定しない。

```rust
pub struct LinuxX11ShmCapturer { /* X11 connection + SHM segment + geometry */ }
impl prdt_media_core::Capturer for LinuxX11ShmCapturer {
    type Frame = crate::BgraFrame;
    fn next_frame(&mut self) -> Result<Self::Frame, prdt_media_core::CaptureError> { ... }
}

pub struct LinuxOpenh264Encoder { inner: sw_pipeline::LinuxSwEncoder }
impl prdt_media_core::Encoder for LinuxOpenh264Encoder {
    type Frame = crate::BgraFrame;
    fn encode(&mut self, frame: &Self::Frame, force_idr: bool, ts: u64)
        -> Result<prdt_media_core::EncodedPacket, prdt_media_core::EncodeError> { ... }
    fn set_target_bitrate(&mut self, bps: u32) { ... }
    fn backend_name(&self) -> &'static str { "linux-x11shm-openh264" }
}

pub struct LinuxOpenh264Decoder { inner: sw_pipeline::LinuxSwDecoder }
impl prdt_media_core::Decoder for LinuxOpenh264Decoder {
    type Frame = prdt_media_sw::I420Frame;
    fn decode(&mut self, packet: &prdt_media_core::EncodedPacket)
        -> Result<Option<Self::Frame>, prdt_media_core::DecodeError> { ... }
    fn backend_name(&self) -> &'static str { "linux-openh264" }
}
```

## 7. Module internals — `crates/input-linux/`

```
src/
  lib.rs              -- pub modules + re-exports (#![cfg(target_os = "linux")])
                         + production surface free functions (host が直接 import)
  error.rs            -- LinuxInputError + 外向きマッピング
  uinput_injector.rs  -- 2 つの uinput device (mouse / keyboard) + InputEvent → uinput event
                         (free function: pub fn inject_event(event: InputEvent) -> Result<()>)
  x11_clipboard.rs    -- _CLIPBOARD selection の read/write + sequence counter
                         (free functions: clipboard_sequence_number / read_clipboard_text /
                          write_clipboard_text — input-win と同形 API)
  x11_geometry.rs     -- RandR で virtual desktop rect 取得 + 起動時 cache
                         (free function: pub fn virtual_desktop_rect() -> MonitorRect)
  core_adapter.rs     -- prdt_input_core 3 trait の impl + builder
                         (unit test surface 兼 L2/L3 予備、L1 production では未使用)
```

**scancode 翻訳テーブルは L1 では実装しない**(critic review v1):

- viewer 側 `winit::platform::scancode::PhysicalKeyExtScancode::to_scancode` は **viewer-OS-native scancode** を返す。Linux viewer なら Linux evdev `KEY_*` コード(u16 範囲)
- L1 smoke target は Linux↔Linux なので、wire を流れる scancode は Linux `KEY_*` であり、`uinput_injector.rs` でそのまま `EV_KEY <scancode> pressed` に発射可能。翻訳不要
- Cross-OS(Win viewer → Linux host、または Linux viewer → Win host)は **wire-protocol semantic 不整合の既知問題**(`crates/protocol/src/input.rs:30` の docstring「host-OS-native scancode」が impl と乖離)。L2 で別途 normalize layer を入れる
- 受信した scancode を pass-through する際は `u32 → u16` キャストで `KEY_MAX` (0x2FF) を超えるものは warn-log + skip(無効値防御)

### `uinput_injector.rs`

#### crate 選択(critic review v1)

- 既存の `uinput = "0.1"` クレート(crates.io)はメンテ停止 + 古い ioctl flow + libudev 依存
- **代わりに `nix = "0.29"` の `ioctl_*!` マクロで `/dev/uinput` を直接叩く**(~150 行、外部依存最小、現代的 `UI_DEV_SETUP` flow 対応)
- 参考: `linux/uinput.h` の `UI_SET_EVBIT/UI_SET_KEYBIT/UI_SET_RELBIT/UI_SET_ABSBIT/UI_DEV_SETUP/UI_DEV_CREATE/UI_DEV_DESTROY` ioctl 定義

#### device 構成

- 起動時に 2 つの uinput device 作成:
  - `prdt-virtual-mouse`: `EV_KEY (BTN_LEFT/RIGHT/MIDDLE/SIDE/EXTRA)` + `EV_REL (REL_X/REL_Y/REL_WHEEL/REL_HWHEEL)` + `EV_ABS (ABS_X/ABS_Y)`
  - `prdt-virtual-keyboard`: `EV_KEY` で `KEY_RESERVED+1..=KEY_MAX` の範囲を網羅的に setbit(個別列挙より楽)

#### ABS 範囲とコーディネート規約(critic finding #2)

- ABS_X 範囲 = `[0, virtual_desktop_rect.width() - 1]`、ABS_Y = `[0, virtual_desktop_rect.height() - 1]`
- `MouseMove { absolute: true, x, y }` の wire 値は **`MonitorRect`(host virtual desktop space)座標**で、L0 trait doc 通り
- **L1 制約: virtual desktop rect の origin が `(0, 0)` であることを前提とする**。`x11_geometry.rs` で起動時に `rect.left != 0 || rect.top != 0` を assert + warn-log + `(0, 0)`-anchored fallback rect に差し替え
- Multi-monitor で primary が左上端でない構成は L2 へ deferred(`x11_geometry.rs` で「unsupported topology」warn を出す)
- 値は受信時に `[0, ABS_max]` に saturating clamp(範囲外でカーネルが `EINVAL` 返さないよう防御)

#### Event 変換

- `MouseMove { x, y, absolute: true }` → `EV_ABS ABS_X x.clamp(0, max); EV_ABS ABS_Y y.clamp(0, max); EV_SYN`
- `MouseMove { x, y, absolute: false }` → `EV_REL REL_X x; EV_REL REL_Y y; EV_SYN`
- `MouseButton { button, pressed }` → `EV_KEY BTN_<...> pressed; EV_SYN`(button enum→BTN_* 変換)
- `MouseWheel { dx, dy }` → `EV_REL REL_HWHEEL dx; EV_REL REL_WHEEL dy; EV_SYN`
- `Key { scancode, pressed }` → `let key = u16::try_from(scancode).ok().filter(|k| *k <= KEY_MAX)?; EV_KEY key pressed; EV_SYN`(範囲外は warn + skip)

#### Send/Sync + 公開 API

- `pub fn inject_event(event: InputEvent) -> Result<()>` を `lib.rs` で free function 公開(host から直接呼ばれる、`prdt-input-win` 同様)
- 内部状態は `OnceCell<Mutex<UinputDevices>>` で初期化遅延 + thread-safe
- L0 trait 用 wrapper(`core_adapter.rs::UinputInjector`)も同 OnceCell を共有 — production と test で同 device を使う
- 権限不足(`EACCES` on `open("/dev/uinput")`): `InjectError::BackendUnavailable("/dev/uinput open failed: ... add user to 'input' group or install udev rule")` を返す
- `Drop` ハンドラで `UI_DEV_DESTROY` ioctl + `close()`

### `x11_clipboard.rs`

#### 公開 API(input-win との対称性)

`prdt-input-win` の free function 群と同名で公開し、host が `#[cfg]` で切り替えられるようにする:

```rust
pub fn clipboard_sequence_number() -> u32;
pub fn read_clipboard_text() -> Result<String, ClipboardError>;
pub fn write_clipboard_text(text: &str) -> Result<(), ClipboardError>;
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;  // input-win と同値
```

#### sequence semantic(critic finding #7、L0 trait doc 準拠)

- `clipboard_sequence_number()` は **observed change** をカウントするモノトニック u32(初期 0)
- 内部実装は `OnceCell<Mutex<ClipboardWatcher>>` で起動時に 1 つ spawn された watcher thread が `XFixesSelectionOwnerNotify` event を購読 → owner 変更を観測する度に `AtomicU32::fetch_add(1)`
- **`write_clipboard_text` で counter は bump しない**(自前の write は own selection owner 化なので X11 側からは「他者の変更」と見えうるが、watcher は最後の write 時刻を記録して dedupe する)
- これは Windows の `GetClipboardSequenceNumber()`(OS 提供、自前 write でも bump)と挙動が異なるが、**L0 trait doc「user changes the system clipboard」セマンティクスに合わせる方針**。Windows 側 host watcher は OS 由来の bump を content compare で dedupe しているので、Linux 側の bump-only-on-foreign 動作の方が host watcher にとってはむしろ noise が少ない
- `XFixes` extension が利用不可な場合は polling fallback(500ms 周期で `get_selection_owner(_CLIPBOARD)` の変化を見る + content hash 比較)

#### read / write 実装

- `write_clipboard_text(text)`:
  1. 透明 1×1 InputOnly window を初回作成
  2. `set_selection_owner(_CLIPBOARD, my_window, CURRENT_TIME)`
  3. 起動時 spawn された clipboard thread が `selection_request` event を受け取り、`UTF8_STRING` target に対して current `Mutex<String>` の値を `change_property` で返す。`Mutex<String>` を `text` で更新
  4. 64KB 超なら `ClipboardError::TooLarge(text.len())`
- `read_clipboard_text() -> Result<String, ClipboardError>`:
  1. `convert_selection(_CLIPBOARD, UTF8_STRING, target_property, my_window, CURRENT_TIME)`
  2. `selection_notify` event を timeout 1 秒で wait
  3. timeout / property 不在 / UTF-8 decode 失敗 → `ClipboardError::NoText`
  4. それ以外 X11 protocol error → `ClipboardError::Backend(msg)`

### `x11_geometry.rs`

- `RandR::get_screen_resources_current(root)` → CRTC list
- 各 CRTC の `(x, y, width, height)` を集めて bounding box(L,T = min、R,B = max)を `MonitorRect` に
- 単一モニタなら primary CRTC の rect そのまま
- 起動時 1 回読んで `cached_rect: MonitorRect` に保持、`virtual_desktop_rect()` はそれを clone
- `RandR::get_screen_resources_current` 失敗時 → `warn-log` + 1920×1080@(0,0) を fallback として返す(panic しない)

### `core_adapter.rs`(unit test surface 兼 L2/L3 予備)

L0 trait を満たす thin wrapper。production wiring からは呼ばれない。`uinput_injector.rs` / `x11_clipboard.rs` / `x11_geometry.rs` の内部状態を共有(`OnceCell` + `AtomicU32` 経由)し、test 向けに trait surface を公開する。

```rust
pub struct UinputInjector { /* OnceCell<Mutex<UinputDevices>> 共有 */ }
impl prdt_input_core::InputInjector for UinputInjector {
    fn inject(&self, event: InputEvent) -> Result<(), prdt_input_core::InjectError> { ... }
    fn backend_name(&self) -> &'static str { "linux-uinput" }
}

pub struct X11Clipboard { /* watcher state 共有 */ }
impl prdt_input_core::ClipboardProvider for X11Clipboard {
    fn read_text(&mut self) -> Result<String, prdt_input_core::ClipboardError> { ... }
    fn write_text(&mut self, t: &str) -> Result<(), prdt_input_core::ClipboardError> { ... }
    fn sequence_number(&mut self) -> u64 {
        crate::x11_clipboard::clipboard_sequence_number() as u64
    }
    fn backend_name(&self) -> &'static str { "linux-x11" }
}

pub struct X11VirtualDesktop { cached_rect: MonitorRect }
impl prdt_input_core::VirtualDesktopGeometry for X11VirtualDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect { self.cached_rect }
}

pub fn build_injector() -> Result<UinputInjector, prdt_input_core::InjectError> { ... }
pub fn build_clipboard() -> Result<X11Clipboard, prdt_input_core::ClipboardError> { ... }
pub fn build_virtual_desktop() -> X11VirtualDesktop { ... }
// build_virtual_desktop は infallible — RandR 失敗時は warn-log + 1920×1080@(0,0) fallback。
```
```

## 8. Host / Viewer wiring

### `crates/host/src/lib.rs`

既存コードでは `#[cfg(windows)] use prdt_input_win::{...}` と Windows 専用の producer 構築コード(`DxgiSwProducer`/`DxgiNvencProducer` 切替)が混在している。L1 では:

- L1 で **新たに platform/ ディレクトリは作らない**。既存の `#[cfg(windows)]` パターンに合わせて、Linux も `#[cfg(target_os = "linux")]` の use と initialization ブロックを並列追加する
- 具体的には Windows 用 `use prdt_input_win::{clipboard_sequence_number, read_clipboard_text, virtual_desktop_rect, write_clipboard_text}` の隣に `#[cfg(target_os = "linux")] use prdt_input_linux::{...}` を加える(同名 free function を input-linux が公開する設計のため、import block 以外は touch しない)
- video producer 構築箇所:
  - 現状: `match cli.encoder { Auto/Nvenc/Mf/Openh264 => DxgiNvencProducer or DxgiSwProducer }` で Windows-specific
  - L1: `#[cfg(target_os = "linux")]` ブロックで `prdt_media_linux::build_video_producer(width, height, bitrate, fps)?` を呼んで `LinuxSwProducer` を構築 → `Box<dyn VideoProducer>` として既存 main loop に渡す
  - encoder CLI flag は `--encoder openh264` 固定(L1 ではそれ以外無効、`--encoder auto` も openh264 にマップ)
- input dispatch / clipboard watcher / geometry 共有 path はすべて **同名 free function** に解決されるので main loop 本体は変更不要

### `crates/viewer/src/lib.rs`

viewer は winit + media-win の D3D11 path が現行。Linux では:

- 既存 D3D11/DualPlaneRenderer/NVDEC/MF 配線は `#[cfg(windows)]` block 内
- 新規 `#[cfg(target_os = "linux")]` block で:
  - `softbuffer::Surface::new(&context, &window)` で CPU framebuffer surface 作成
  - 受信した `EncodedFrame` を `prdt_media_linux::build_video_decoder()` の `LinuxSwDecoder` に投入 → `Option<I420Frame>`
  - `prdt_media_linux::i420_to_bgra(&i420, &mut buffer)` で softbuffer の `&mut [u32]` バッファに直接書き込み
  - `buffer.present()`
- 入力 event(viewer→host)は winit cross-platform path を共有、scancode は前述の通り L1 では Linux 環境では Linux KEY_* がそのまま wire に乗る

### `crates/client/src/main.rs`

- サブコマンド dispatch を `#[cfg(any(windows, target_os = "linux"))]` に拡張(`Cmd::Host`、`Cmd::Connect`)
- `Cmd::Gui` も `#[cfg(any(windows, target_os = "linux"))]` だが、Linux で gui-client が build できない場合は `#[cfg(windows)]` に後退(後述 §9 の検証で確定)
- `crates/client/Cargo.toml` の `[target.'cfg(target_os = "linux")'.dependencies]` block で `prdt-host`, `prdt-viewer`(必要に応じ `prdt-gui-client`)を引き入れる

### `crates/viewer/src/`

- 同様に `platform/` ディレクトリ:
  - `win.rs`: 既存の D3D11 + DualPlaneRenderer + NVDEC/MF 配線
  - `linux.rs`: 新規。winit window 作成 + softbuffer surface 構築 + decoder loop
- Linux render path:
  ```
  loop {
      let packet = recv_video_packet().await;
      if let Some(i420) = decoder.decode(&packet)? {
          let mut buffer = surface.buffer_mut()?;
          prdt_media_linux::i420_to_bgra(&i420, &mut buffer);
          buffer.present()?;
      }
  }
  ```
- 入力 event: winit の WindowEvent → 既存 viewer の event mapper(cross-platform 部分)→ transport 経由で host へ送信(コード変更なし)

### `crates/client/src/main.rs`

- `Cmd::Host` / `Cmd::Connect` の dispatch を `#[cfg(any(windows, target_os = "linux"))]` に
- `Cmd::Gui` は `#[cfg(any(windows, target_os = "linux"))]`(Linux で gui-client が build できれば、できない場合は windows のみに後退、smoke 検証で判断)

## 9. Audio / GUI / Filetransfer / Tray verification

`prdt-audio`、`prdt-filetransfer`、`prdt-gui-*`、tray は **コード変更ゼロを目標**、検証のみ。問題発生時のみ最小修正 (`#[cfg(target_os = "linux")]` で機能 disable や warn fallback)。

| 対象 | 検証 | 想定リスク + 対応 |
|---|---|---|
| `prdt-audio` | `cargo check + test --target linux`、WSLg で host を default audio で起動して device 列挙ログ確認 | WSLg で audio device 不在 → host CLI に Linux で `--audio off` を default にする小修正(該当時のみ) |
| `prdt-filetransfer` | `cargo check + test --target linux` | 期待: 問題なし(tokio fs は POSIX 透過) |
| `prdt-gui-host` / `prdt-gui-viewer` | `cargo check --target linux`、WSLg で `prdt gui` 起動 → window 描画目視 | tray-icon 起動失敗 → warn fallback の `#[cfg(target_os = "linux")]` 分岐を最小限追加 |
| `prdt-gui-client` | 同上、ただし現状 `[target.'cfg(windows)'.dependencies]` で `prdt-host`/`prdt-viewer`/`prdt-gui-host`/`prdt-crypto`/`clap` を Windows のみに gate しており Linux build では機能ゼロ。L1 で `[target.'cfg(target_os = "linux")']` 同等 block を追加して Linux でも実機能ありで build 可能化(必要なら) | dep 追加で transitive build issue が出た場合は `Cmd::Gui` を Linux 側で `#[cfg(windows)]` のままに後退させ open question 化 |
| tray | 上記 GUI の一部 | 起動失敗で warn のみ、GUI 本体は続行 |

`prdt-gui-host`(`tray-icon`、`notify-rust`、`self_update`、`image`、`ureq`、`ico`、`semver`、`tempfile`)はすべて cross-platform crate を unconditional dep として宣言しており、`winreg` のみ正しく Windows-gated 済み。よって **gui-host は workspace MSRV(1.85)以降の Rust toolchain で Linux build 可能と推定**。実 cargo check で transitive dep に引っかかった場合(例: master 検証時に `zmij 1.0.21` の `core::hint::select_unpredictable` 不足が観測された)→ `rust-toolchain.toml` で nightly pin / Rust 更新指示を smoke checklist に追加。これは spec の修正対象ではなく env 整備課題

## 10. Error model

### `media-linux::error::LinuxMediaError`(crate-internal)

```rust
#[derive(Debug, thiserror::Error)]
pub enum LinuxMediaError {
    #[error("X11 connection failed: {0}")] X11Connect(String),
    #[error("MIT-SHM extension unavailable")] ShmUnavailable,
    #[error("XGetImage failed for root window")] XGetImageFailed,
    #[error("openh264 backend error: {0}")]
    Openh264(#[from] prdt_media_sw::MediaSwError),
    #[error("invalid frame dimensions: {0}x{1}")] InvalidDimensions(u32, u32),
}
```

外向きマッピング:

| 内部 variant | `CaptureError` | `EncodeError` | `DecodeError` |
|---|---|---|---|
| `X11Connect` | `Backend(...)` | — | — |
| `ShmUnavailable` | warn-log し plain `XGetImage` に fallback (`Err` にしない) | — | — |
| `XGetImageFailed` | `Backend(...)` | — | — |
| `Openh264` (encode side) | — | `Backend(msg)`; format mismatch のみ `FormatMismatch` | — |
| `Openh264` (decode side) | — | — | `Backend(msg)` |
| `InvalidDimensions` | `Backend(...)` | `FormatMismatch` | `FormatMismatch` |

`EncodeError::DeviceLost` は Linux SW path では emit しない(GPU device が無いため、L0 で追加された variant の semantic に該当しない)。

### `input-linux::error::LinuxInputError`(crate-internal)

```rust
#[derive(Debug, thiserror::Error)]
pub enum LinuxInputError {
    #[error("/dev/uinput open failed: {0} (hint: add user to 'input' group or check udev rule)")]
    UinputOpenDenied(std::io::Error),
    #[error("uinput ioctl failed: {0}")] UinputIoctl(std::io::Error),
    #[error("scancode {0:#x} out of Linux KEY_* range (max=0x2FF)")] ScancodeOutOfRange(u32),
    #[error("X11 connection failed: {0}")] X11Connect(String),
    #[error("clipboard selection request timed out")] ClipboardTimeout,
    #[error("clipboard returned non-UTF-8 bytes")] ClipboardNonUtf8,
    #[error("clipboard payload too large: {0} bytes")] ClipboardTooLarge(usize),
    #[error("RandR returned no CRTCs")] NoCrtcs,
}
```

外向きマッピング:

| 内部 variant | `InjectError` | `ClipboardError` |
|---|---|---|
| `UinputOpenDenied` | `BackendUnavailable(msg+hint)` | — |
| `UinputIoctl` | `Backend(msg)` | — |
| `ScancodeOutOfRange` | warn-log + skip(`Err` にしない、UX 連続性優先) | — |
| `X11Connect` (clipboard) | — | `Backend(msg)` |
| `ClipboardTimeout` | — | `NoText` |
| `ClipboardNonUtf8` | — | `NoText` |
| `ClipboardTooLarge(usize)` | — | `TooLarge(n)` |
| `NoCrtcs` | — | — (geometry: warn + 1920×1080@(0,0) fallback) |

`prdt_input_core::ClipboardError` の variant は `Backend(String) / NoText / TooLarge(usize)` の 3 つのみ(`crates/input-core/src/error.rs` で確認)。spec v1 の `EmptyOrUnsupported` は存在しないので `NoText` に統合。`UnmappedScancode` は scancode テーブルを L1 で持たない方針(§7)に伴い `ScancodeOutOfRange`(u16 / KEY_MAX 範囲外チェック)に rename。

## 11. Testing

### Unit tests(CI で常時走る、~13 新規)

| crate | テスト | 目的 |
|---|---|---|
| `media-linux` | `bgra_frame_size_matches_geometry` | capture が root window と同じ寸法を返す(mock geometry) |
| `media-linux` | `shm_unavailable_falls_back_to_get_image` | extension probe 失敗で fallback path に行く |
| `media-linux` | `encoder_round_trip_via_media_sw` | `bgra_to_i420 → encode → decode → i420` で寸法/フォーマット一致 |
| `media-linux` | `decoder_emits_some_after_idr` | IDR を食わせると `Some(I420Frame)` が出る |
| `media-linux` | `i420_to_bgra_known_pixels` | 既知 YUV 入力(Y=128, U=V=128 のグレー)で BGRA が灰色になる |
| `media-linux` | `linux_sw_producer_pacer_60fps` | `tokio::time::pause()` で 60Hz interval が next_frame を ~16.67ms 周期で叩く事を検証 |
| `input-linux` | `scancode_passthrough_in_range` | u16 範囲内 + ≤ KEY_MAX(0x2FF) は inject へ通る |
| `input-linux` | `scancode_out_of_range_skipped` | 0x10000+ や KEY_MAX 超過は warn-log + skip(`Err` 返さない) |
| `input-linux` | `uinput_open_eacces_yields_backend_unavailable` | `/dev/uinput` を mock io error で塞ぎ、`InjectError::BackendUnavailable` が返る |
| `input-linux` | `clipboard_sequence_increments_only_on_external_change` | own write は bump しない、selection_owner 変化のみ bump |
| `input-linux` | `virtual_desktop_rect_unions_crtcs` | mock RandR データで union が正しい |
| `prdt-host` (Linux) | `linux_platform_module_compiles` | spec 通り build できる(integration test) |
| `prdt-viewer` (Linux) | `softbuffer_init_smoke` | softbuffer surface が作成できる(headless 不可なら `#[ignore]`) |

### Integration / smoke tests(`#[ignore]`、手動)

| テスト | 内容 |
|---|---|
| `xshm_capture_one_frame` | 実 X11 接続 + 1 frame capture + 寸法検証 |
| `uinput_inject_mouse_move` | uinput device 作成、`MouseMove(0,0)` 注入、device 作成成功で OK 扱い |
| `x11_clipboard_set_then_get` | `write_text("hello") → read_text() == "hello"` |

### Manual smoke checklist

別ファイル `docs/superpowers/plans/2026-05-09-l1-linux-poc-manual-smoke.md` に以下を記述:

1. `cargo build --release --target x86_64-unknown-linux-gnu --workspace` グリーン
2. `groups | grep input` で `input` group メンバーシップ確認、なければ `sudo usermod -aG input $USER` → 再ログイン(or 一時的 `sudo chmod 666 /dev/uinput`)
3. `target/release/prdt host` 起動 → tracing log:
   - `X11 connected, MIT-SHM available`(or `extension unavailable, falling back`)
   - `uinput device created`
   - `host listening on UDP <port>`
4. 別ターミナルで `target/release/prdt connect <host_id>` → handshake 成功 + winit window 開く + frame 受信
5. viewer window 内で mouse 動作 → host 側 WSLg desktop で pointer 動作確認
6. viewer 上で keypress → host 側で同じキー入力(`xev` で確認)
7. viewer の clipboard text を copy → host で `xsel -b` などで同テキスト
8. (best-effort) `prdt gui` 起動 → window 描画目視。失敗時は warn のみ確認、修正は L1 範囲外
9. (best-effort) 30 秒間 session 保持、tracing log の e2e_p99 値を記録(数値は acceptance 外、参考値)

### Regression posture

- 既存 Windows テスト 0 件 fail(現 master ~356 tests 全 green を維持)
- `cargo check --workspace --target x86_64-unknown-linux-gnu` グリーン
- `cargo clippy --workspace --target x86_64-unknown-linux-gnu -- -D warnings` グリーン

## 12. Definition of Done

L1 完了条件:

1. **Unit tests グリーン**: 新規 ~13 + 既存全部
2. **Cargo check + clippy グリーン**(両 target、警告ゼロ in `-D warnings` mode)
3. **Manual smoke checklist 1-7 ステップ全パス**
4. **Step 8 (GUI)** は warn 許容、panic しないことのみ確認
5. **Step 9 (latency 数値)** は記録のみ、数値は acceptance に含めない
6. ドキュメント: `docs/superpowers/plans/2026-05-09-l1-linux-poc-manual-smoke.md` 完備、`STATUS.md` の B2 セクションを「L1 完了」に更新

## 13. Out of Scope

| 項目 | 後回し先 |
|---|---|
| Wayland portal capture (`org.freedesktop.portal.ScreenCast`) | L2 |
| libei + RemoteDesktop portal による input inject | L2 |
| wl-clipboard / portal Clipboard | L2 |
| VAAPI (Intel/AMD) HW encode | L2 |
| NVENC / NVDEC on Linux | L3(NVIDIA 環境必須) |
| Linux glass-to-glass / multi-config benchmark | L2 以降 |
| 30 分以上の stability bench on Linux | L2 以降 |
| 複数 distro (Fedora/Arch/RHEL) 検証 | L2 以降、実機提供あり次第 |
| deb / AppImage / Flatpak packaging | Phase 5 相当 |
| autostart `~/.config/autostart/*.desktop` | L2 |
| viewer の wgpu 統一(D3D11/softbuffer 二系統解消)| L3 |
| ホスト側 audio device の任意環境動作保証 | L2 |
| input remapping / dead key / IME | 別計画 |
| クロスコンパイル | 不採用、native build only |

## 14. Open questions for the implementer

これらは plan 化の段階で task を切る前に解決される類の問題で、spec レベルでは未確定で構わない:

- **Rust toolchain version on dev env**: master を `cargo check --target x86_64-unknown-linux-gnu --workspace` で試した時 `zmij 1.0.21`(transitive dep)が `core::hint::select_unpredictable`(unstable intrinsic)を呼んでいるため fail。`rustup update stable` または `rust-toolchain.toml` で nightly pin が必要かどうかを plan 着手時に確認、smoke checklist の前段に追記
- `x11rb` の MIT-SHM 経路で SysV SHM segment と posix-mq SHM のどちらを使うか(crate API による)。クレート docs を確認の上、最も sample-rich な path を採用
- viewer の winit + softbuffer init は X11 と Wayland(WSLg)両方で動作する想定だが、WSLg で `WAYLAND_DISPLAY` がセットされている場合の挙動を smoke で確認(`WINIT_UNIX_BACKEND=x11` 環境変数強制が必要なら manual smoke checklist に記載)
- `prdt-gui-*` の Linux build で eframe の wayland feature が要求する system lib(`libwayland-client`, `libxkbcommon` 等)が無い場合の対応 — 必要なら `apt install` 手順を smoke checklist に
- `audiopus` が Linux で `libopus` system 依存を要求するか(`audiopus-sys` のビルド設定確認)。要求する場合は smoke checklist に `apt install libopus-dev`
- `nix` クレートの `ioctl_*!` マクロで `UI_DEV_SETUP` (`_IOW(UINPUT_IOCTL_BASE, 3, struct uinput_setup)`) を正しく宣言できるか — 標準サンプル(`evdev` クレート source 等)を参考に組む

### L1 では touch しないが将来課題として記録

- **Cross-OS scancode normalization (wire-protocol semantic 不整合)**: `crates/protocol/src/input.rs:30` docstring「host-OS-native scancode」と viewer 実装(`PhysicalKeyExtScancode::to_scancode` = viewer-OS-native)の乖離。L1 同 OS path では実害なし。L2 以降で (a) viewer 側に host-OS aware 変換 layer 追加、(b) wire を USB HID Usage ID に normalize、のいずれかを採用予定
- **Multi-monitor non-zero-origin**: `x11_geometry.rs` で primary が左上端でない構成 (`rect.left != 0 || rect.top != 0`) は L1 で warn + (0,0) fallback 対応のみ。完全対応は L2 で uinput ABS の origin offset サポート追加と合わせて実施
- **Cursor capture**: XShm は root window pixels のみ取得、X11 hardware cursor は capture されない(Windows DXGI も同様で、Windows 側は `prdt-media-win::dxgi::Cursor` で別 channel 合成)。Linux 版の cursor 合成は L2

---

**Status:** Brainstorming complete. Ready for spec self-review and user approval before invoking `superpowers:writing-plans`.
