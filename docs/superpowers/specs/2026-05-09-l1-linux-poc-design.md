# L1 — Linux PoC Design

**Date:** 2026-05-09
**Status:** Draft (brainstorming output, awaiting user spec review)
**Predecessor:** L0 Trait Extraction (`docs/superpowers/plans/2026-05-08-l0-trait-extraction.md`, status `2026-05-08-l0-trait-extraction-status.md`)
**Approach:** Approach 1 — `#[cfg]` gate を Windows 同様に使って素直に Linux 実装を増やす

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

cross-platform 想定 crate(`prdt-protocol`、`prdt-transport`、`prdt-crypto`、`prdt-signaling-*`、`prdt-nat-traversal`、`prdt-audio`、`prdt-filetransfer`、`prdt-media-sw`)は L0 までで Windows 側だけ build 検証済みで、Linux build 検証は L1 のスコープ。

## 4. Architecture overview

```
                 ┌────────────────────────────────────────┐
                 │  prdt unified bin (crates/client)      │
                 │  Linux: subcommand dispatch w/ cfg     │
                 └────────────────────────────────────────┘
                               │
              ┌────────────────┴────────────────┐
              ▼                                 ▼
     ┌──────────────────┐              ┌──────────────────┐
     │  prdt-host       │              │  prdt-viewer     │
     │  + platform.rs   │              │  + platform.rs   │
     │   (win | linux)  │              │   (win | linux)  │
     └──────────────────┘              └──────────────────┘
              │                                 │
   ┌──────────┴──────────┐         ┌────────────┴─────────────┐
   ▼                     ▼         ▼                          ▼
prdt-media-linux  prdt-input-linux  prdt-media-sw     softbuffer + winit
(XShm capture +   (uinput inject +  (Openh264 decode  (CPU framebuffer
 sw_pipeline +     scan_code_table +  CPU)             presentation)
 core_adapter)     x11_clipboard +
                   x11_geometry +
                   core_adapter)
   │                     │                  │
   └─── prdt-media-core ─┴──── prdt-input-core ─┘
   │
   └── cross-platform: prdt-protocol / prdt-transport / prdt-crypto /
       prdt-audio (cpal) / prdt-filetransfer / prdt-signaling-* /
       prdt-nat-traversal
```

Windows path は完全に保持(D3D11 + DXGI + NVENC + NVDEC + MF + WASAPI)。Linux path は別系統として並列に存在し、`#[cfg(target_os = "linux")]` で gate される。

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
| `crates/input-linux/Cargo.toml` | `[dependencies]` に `x11rb = { workspace = true }`、`uinput = "0.1"`、`thiserror = { workspace = true }`、`tracing = { workspace = true }` を追加 |
| `crates/host/Cargo.toml` | `[target.'cfg(target_os = "linux")'.dependencies]` block 追加(`prdt-media-linux`、`prdt-input-linux`、`prdt-media-sw`、`prdt-gui-host`、`prdt-gui-common`、`tokio-util`、`async-trait`) |
| `crates/viewer/Cargo.toml` | 同上 + `softbuffer = { workspace = true }`、`prdt-gui-viewer`、`prdt-gui-common`、`prdt-media-sw` |
| `crates/client/Cargo.toml` | 同上(`prdt-host`、`prdt-viewer`、必要なら `prdt-gui-client`) |
| `Cargo.toml` (workspace) | `[workspace.dependencies]` に `x11rb = "0.13"` と `softbuffer = "0.4"` を追加 |

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
| `x11rb` | X11 protocol(XShm + RandR + clipboard) | `0.13` |
| `uinput` | `/dev/uinput` device 作成 + イベント送信 | `0.1` |
| `softbuffer` | viewer の CPU framebuffer presentation | `0.4` |

`x11rb` と `softbuffer` は workspace 共通 dep として宣言。`uinput` は `input-linux` crate-local dep に留める(他 crate からは使わない)。

## 6. Module internals — `crates/media-linux/`

```
src/
  lib.rs              -- pub modules + re-exports (#![cfg(target_os = "linux")])
  error.rs            -- LinuxMediaError + 外向きマッピング helper
  frame.rs            -- BgraFrame { width, height, stride, bgra: Vec<u8>, capture_ts_us: u64 }
  x11_capture.rs      -- XShmGetImage 経路 + plain XGetImage fallback
  sw_pipeline.rs      -- BgraFrame → I420 → Openh264Encoder / Openh264Decoder ラッパ
  i420_to_bgra.rs     -- I420 → BGRA helper (BT.709 limited、~50 行、softbuffer から呼ばれる)
  core_adapter.rs     -- prdt_media_core::{Capturer, Encoder, Decoder} の impl + builder
```

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

### `core_adapter.rs`

```rust
pub struct LinuxX11ShmCapturer { /* connection + SHM segment + geometry */ }
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

pub fn build_capturer() -> Result<LinuxX11ShmCapturer, prdt_media_core::CaptureError> { ... }
pub fn build_encoder(width: u32, height: u32, bitrate_bps: u32, fps: u32)
    -> Result<LinuxOpenh264Encoder, prdt_media_core::EncodeError> { ... }
pub fn build_decoder() -> Result<LinuxOpenh264Decoder, prdt_media_core::DecodeError> { ... }
```

## 7. Module internals — `crates/input-linux/`

```
src/
  lib.rs              -- pub modules + re-exports (#![cfg(target_os = "linux")])
  error.rs            -- LinuxInputError + 外向きマッピング
  scan_code_table.rs  -- ps2_set1_to_linux_key(scancode: u32) -> Option<u16>
  uinput_injector.rs  -- 2 つの uinput device (mouse / keyboard) + InputEvent → uinput event
  x11_clipboard.rs    -- _CLIPBOARD selection の read/write + 内部 sequence counter
  x11_geometry.rs     -- RandR で virtual desktop rect 取得 + 起動時 cache
  core_adapter.rs     -- prdt_input_core 3 trait の impl + builder
```

### `scan_code_table.rs`

- 関数: `pub fn ps2_set1_to_linux_key(scancode: u32) -> Option<u16>`
  - 入力: Windows PS/2 Set 1 scancode(`InputEvent::Key.scancode`)。0xE0 prefix は upper byte に含まれる(現状の wire format 通り)
  - 出力: Linux `KEY_*` (input-event-codes.h、`u16`)
- 約 120 entry を `match` 式で直書き(alphabet/digits/F-keys/nav/modifier/拡張キー)
  - 主要なもの: `0x1E='A'→KEY_A=30`、`0x1C=Enter→KEY_ENTER=28`、`0x39=Space→KEY_SPACE=57`、`0xE0_5D=Menu→KEY_COMPOSE=127`、矢印 `0xE0_4B/4D/48/50→KEY_LEFT/RIGHT/UP/DOWN`、modifier `0x1D=LCtrl→KEY_LEFTCTRL=29`、`0xE0_1D=RCtrl→KEY_RIGHTCTRL=97`、function `0x3B-0x44=F1-F10→KEY_F1-F10` …
- 未マップは `None` 返却(呼び出し側で warn-log + skip)

### `uinput_injector.rs`

- 2 つの uinput device を起動時に作成:
  - `prdt-virtual-mouse`: `EV_KEY (BTN_LEFT/RIGHT/MIDDLE/SIDE/EXTRA)` + `EV_REL (REL_X/REL_Y/REL_WHEEL/REL_HWHEEL)` + `EV_ABS (ABS_X/ABS_Y、min=0、max=virtual_desktop.{width,height}-1)`
  - `prdt-virtual-keyboard`: `EV_KEY (KEY_A..KEY_F24 ほぼ全部)`、`UI_SET_KEYBIT` ループで設定
- Event 変換:
  - `MouseMove { x, y, absolute: true }` → `EV_ABS ABS_X x; EV_ABS ABS_Y y; EV_SYN`
  - `MouseMove { x, y, absolute: false }` → `EV_REL REL_X x; EV_REL REL_Y y; EV_SYN`
  - `MouseButton { button, pressed }` → `EV_KEY BTN_<...> pressed; EV_SYN`(button enum→BTN_* 変換)
  - `MouseWheel { dx, dy }` → `EV_REL REL_HWHEEL dx; EV_REL REL_WHEEL dy; EV_SYN`
  - `Key { scancode, pressed }` → `ps2_set1_to_linux_key(scancode)?` → `EV_KEY KEY_X pressed; EV_SYN`(`None` なら warn + skip)
- `Drop` で `UI_DEV_DESTROY` ioctl
- 権限不足(`EACCES` on `open("/dev/uinput")`): `InjectError::BackendUnavailable("/dev/uinput open failed: ... add user to 'input' group or set udev rule")` で具体的メッセージ
- 構造体は `Send`、`InputInjector::inject(&self, event)` 呼出時に内部 `Mutex<UinputDevices>` を取って書き込む(L0 trait は `&self` シグネチャ)

### `x11_clipboard.rs`

- 専用の `RustConnection` を内部に保持(host の他用途と共有しない、code 簡素化のため)
- `write_text(&mut self, t: &str)`:
  1. 透明 1×1 InputOnly window を作成(selection owner 用、初回のみ)
  2. `set_selection_owner(_CLIPBOARD, my_window, CURRENT_TIME)`
  3. background thread で `selection_request` event を受け取り、`UTF8_STRING` target には `t` を返す。それ以外は refuse。**L1 の単純化案**: 起動時に 1 回だけ thread spawn、`Mutex<String>` で current text を持ち、handler はそれを返す。Drop で thread join
- `read_text(&mut self) -> Result<String, ClipboardError>`:
  1. `convert_selection(_CLIPBOARD, UTF8_STRING, target_property, my_window, CURRENT_TIME)`
  2. `selection_notify` event を timeout 1 秒で wait(timeout は `ClipboardError::EmptyOrUnsupported`)
  3. `get_property` で取得、UTF-8 decode 失敗時は `ClipboardError::EmptyOrUnsupported`
- `sequence_number(&mut self) -> u64`:
  - 初期値 0、`read_text` 成功時に内部 last-seen-text とハッシュ比較、変化していれば +1
  - `write_text` 成功時にも +1(他クライアントから読まれる前に host 側自身が変更したケースを区別)

### `x11_geometry.rs`

- `RandR::get_screen_resources_current(root)` → CRTC list
- 各 CRTC の `(x, y, width, height)` を集めて bounding box(L,T = min、R,B = max)を `MonitorRect` に
- 単一モニタなら primary CRTC の rect そのまま
- 起動時 1 回読んで `cached_rect: MonitorRect` に保持、`virtual_desktop_rect()` はそれを clone
- `RandR::get_screen_resources_current` 失敗時 → `warn-log` + 1920×1080@(0,0) を fallback として返す(panic しない)

### `core_adapter.rs`

```rust
pub struct UinputInjector { /* Mutex<UinputDevices> */ }
impl prdt_input_core::InputInjector for UinputInjector { ... }

pub struct X11Clipboard { /* RustConnection + state thread handle + last_seq */ }
impl prdt_input_core::ClipboardProvider for X11Clipboard { ... }

pub struct X11VirtualDesktop { cached_rect: MonitorRect }
impl prdt_input_core::VirtualDesktopGeometry for X11VirtualDesktop { ... }

pub fn build_injector(virtual_rect: MonitorRect)
    -> Result<UinputInjector, prdt_input_core::InjectError> { ... }
pub fn build_clipboard()
    -> Result<X11Clipboard, prdt_input_core::ClipboardError> { ... }
pub fn build_virtual_desktop() -> X11VirtualDesktop { ... }
// RandR 取得失敗時は warn-log + 1920×1080@(0,0) を内部 fallback として保持し、
// 呼び出し側からは常に成功する infallible API として見せる。
```

## 8. Host / Viewer wiring

### `crates/host/src/`

- 新ディレクトリ `platform/` を導入:
  - `mod.rs`: `#[cfg(windows)] mod win; #[cfg(target_os = "linux")] mod linux;` + `pub use` で必要な型を re-export
  - `win.rs`: 現状の `#[cfg(windows)]` ブロック相当(producer 構築、encoder 選択、injector 構築 etc.)を移動 — できる限り **コードのコピー & rename のみ**、ロジック改変なし
  - `linux.rs`: 新規。`prdt_media_linux::core_adapter::build_*` と `prdt_input_linux::core_adapter::build_*` を呼んで host main loop に必要な型を返す
- 既存 `lib.rs` 内の host main loop **本体ロジックは変更しない**。loop に必要な platform 型(`Capturer + Encoder + InputInjector + ClipboardProvider + VirtualDesktopGeometry`)を `platform::*` 経由で取り込むだけで、変更点は `use` 文と initialization (`platform::build_*()`) の差し替えに留める
- producer/encoder の wiring は **既存 Windows 用パイプラインのレイアウトに合わせる**(`DxgiSwProducer` 相当の Linux 版を作るのではなく、より単純な「capture → encode → packet send」ループとして `linux.rs` の中で持つ。Windows 用の複雑な multi-encoder logic は L1 では含めない)

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
| `prdt-gui-client` | 同上 | 同上 |
| tray | 上記 GUI の一部 | 起動失敗で warn のみ、GUI 本体は続行 |

`prdt-gui-host` などの Linux build で deps の解決失敗があった場合は、その crate の `Cargo.toml` 内の `[target.'cfg(target_os = "linux")']` block を最小限編集する(例: `winreg` の Windows-only 化が抜けていれば修正)。

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
    #[error("scancode {0:#x} has no Linux key mapping")] UnmappedScancode(u32),
    #[error("X11 connection failed: {0}")] X11Connect(String),
    #[error("clipboard selection request timed out")] ClipboardTimeout,
    #[error("clipboard returned non-UTF-8 bytes")] ClipboardNonUtf8,
    #[error("RandR returned no CRTCs")] NoCrtcs,
}
```

外向きマッピング:

| 内部 variant | `InjectError` | `ClipboardError` |
|---|---|---|
| `UinputOpenDenied` | `BackendUnavailable(msg+hint)` | — |
| `UinputIoctl` | `Backend(msg)` | — |
| `UnmappedScancode` | warn-log + skip(`Err` にしない) | — |
| `X11Connect` (clipboard) | — | `Backend(msg)` |
| `ClipboardTimeout` | — | `EmptyOrUnsupported` |
| `ClipboardNonUtf8` | — | `EmptyOrUnsupported` |
| `NoCrtcs` | — | — (geometry: warn + 1920×1080@(0,0) fallback) |

## 11. Testing

### Unit tests(CI で常時走る、~13 新規)

| crate | テスト | 目的 |
|---|---|---|
| `media-linux` | `bgra_frame_size_matches_geometry` | capture が root window と同じ寸法を返す(mock geometry) |
| `media-linux` | `shm_unavailable_falls_back_to_get_image` | extension probe 失敗で fallback path に行く |
| `media-linux` | `encoder_round_trip_via_media_sw` | `bgra_to_i420 → encode → decode → i420` で寸法/フォーマット一致 |
| `media-linux` | `decoder_emits_some_after_idr` | IDR を食わせると `Some(I420Frame)` が出る |
| `media-linux` | `i420_to_bgra_known_pixels` | 既知 YUV 入力(Y=128, U=V=128 のグレー)で BGRA が灰色になる |
| `input-linux` | `ps2_set1_alphabet_maps` | 0x1E..='A'..='Z' 全部マップされる |
| `input-linux` | `ps2_set1_extended_e0_maps` | 0xE0_5D=Menu / 矢印 / RCtrl / RAlt 等が正しく KEY_* に |
| `input-linux` | `unmapped_scancode_returns_none` | 0xFF_FF などは `None` |
| `input-linux` | `uinput_open_eacces_yields_backend_unavailable` | `/dev/uinput` を mock io error で塞ぎ、`InjectError::BackendUnavailable` が返る |
| `input-linux` | `clipboard_sequence_increments_on_change` | mock で write→seq+1、同じ text 再 read で seq 不変 |
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

- `x11rb` の MIT-SHM 経路で SysV SHM segment と posix-mq SHM のどちらを使うか(crate API による)。クレート docs を確認の上、最も sample-rich な path を採用
- viewer の winit + softbuffer init は X11 と Wayland(WSLg)両方で動作する想定だが、WSLg で `WAYLAND_DISPLAY` がセットされている場合の挙動を smoke で確認(`WINIT_UNIX_BACKEND=x11` 環境変数強制が必要なら manual smoke checklist に記載)
- `prdt-gui-*` の Linux build で eframe の wayland feature が要求する system lib(`libwayland-client`, `libxkbcommon` 等)が無い場合の対応 — 必要なら `apt install` 手順を smoke checklist に
- `audiopus` が Linux で `libopus` system 依存を要求するか(`audiopus-sys` のビルド設定確認)。要求する場合は smoke checklist に `apt install libopus-dev`

これらは plan 化の段階で task を切る前に発見/解決される類の問題で、spec レベルでは未確定で構わない。

---

**Status:** Brainstorming complete. Ready for spec self-review and user approval before invoking `superpowers:writing-plans`.
