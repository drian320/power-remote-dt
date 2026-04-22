# Phase 0 — Plan 3 of 4: Input + Host/Viewer Binary Wiring

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** 2 つの Windows マシン(または 1 台 localhost loopback)で実際にリモートデスクトップとして画面表示 + 入力操作ができる最小動作品を作る。Plan 1〜2c で作った全コンポーネントを host / viewer バイナリに結線する。

**Architecture:**
- `input-win`: `SendInputInjector`(host 側)、`RawInputCapturer`(viewer 側)
- viewer の描画: D3D11 swapchain に NV12→BGRA シェーダで present(`MfD3d11Consumer` を拡張して ID3D11Texture2D 出力対応)
- `host` バイナリ: CLI で bind アドレス、モニタ index、ビットレート受け取り、producer ループ + UDP transport + input injector
- `viewer` バイナリ: CLI で host アドレス、winit window、swapchain、consumer、input capturer

**Tech Stack:** Plan 2c までの全レイヤ + `winit` 0.30+、`windows` crate 0.58(Win32_UI_Input*、Win32_UI_WindowsAndMessaging、D3D11 shader compile など)。

**Spec reference:** §§ 2.7(binary skeleton)、3.2(Host thread layout)、3.3(Viewer thread layout)、4.1-4.7、6.1 F13-F15。

---

## File Structure(Plan 3 完了時)

```
crates/
├── input-win/
│   ├── Cargo.toml                  [modify] add windows crate w/ Input features
│   └── src/
│       ├── lib.rs                  [modify]
│       ├── injector.rs             [new] SendInputInjector
│       └── capturer.rs             [new] RawInputCapturer (winit integration)
├── media-win/
│   └── src/
│       ├── pipeline/consumer.rs    [modify] extract ID3D11Texture2D output
│       └── d3d11/
│           ├── mod.rs              [modify] export swapchain + renderer
│           ├── swapchain.rs        [new] SwapChain wrapper
│           └── nv12_renderer.rs    [new] NV12→BGRA shader + present
├── host/
│   ├── Cargo.toml                  [modify] add deps
│   └── src/main.rs                 [rewrite]
└── viewer/
    ├── Cargo.toml                  [modify] add deps (winit)
    └── src/main.rs                 [rewrite]
```

---

## Task List(7 tasks)

- Task 1: `input-win::SendInputInjector` + `RawInputCapturer`
- Task 2: `MfD3d11Consumer` D3D11 texture 出力化(`take_latest_texture`)
- Task 3: `D3d11SwapChain` + NV12→BGRA renderer
- Task 4: `host` バイナリ実装
- Task 5: `viewer` バイナリ実装(winit + swapchain 結線)
- Task 6: 手動スモーク手順書 + `cargo build --release` 動作確認
- Task 7: README + `phase0-plan3-complete` タグ

---

## Task 1: input-win (Injector + Capturer)

**Files:**
- Modify: `crates/input-win/Cargo.toml`
- Create: `crates/input-win/src/injector.rs`
- Create: `crates/input-win/src/capturer.rs`
- Modify: `crates/input-win/src/lib.rs`

### Cargo.toml

```toml
[package]
name = "prdt-input-win"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-protocol = { path = "../protocol" }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync"] }
async-trait = "0.1"
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_UI_Input",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_UI_WindowsAndMessaging",
] }
```

### injector.rs

```rust
//! Injects InputEvents on the host via `SendInput`.

use std::mem;

use prdt_protocol::{InputEvent, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("SendInput: {0}")]
    SendInput(String),
}

pub struct SendInputInjector;

impl SendInputInjector {
    pub fn new() -> Self { Self }

    pub fn inject(&self, ev: InputEvent) -> Result<(), InjectError> {
        unsafe {
            match ev {
                InputEvent::MouseMove { x, y, absolute } => {
                    let mut flags = MOUSEEVENTF_MOVE;
                    if absolute {
                        flags |= MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
                    }
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: x,
                                dy: y,
                                mouseData: 0,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 {
                        return Err(InjectError::SendInput("MouseMove sent 0".into()));
                    }
                }
                InputEvent::MouseButton { button, pressed } => {
                    let flags = match (button, pressed) {
                        (MouseButton::Left, true)  => MOUSEEVENTF_LEFTDOWN,
                        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                        (MouseButton::Right, true)  => MOUSEEVENTF_RIGHTDOWN,
                        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                        (MouseButton::Middle, true)  => MOUSEEVENTF_MIDDLEDOWN,
                        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                        (MouseButton::X1, true)  => MOUSEEVENTF_XDOWN,
                        (MouseButton::X1, false) => MOUSEEVENTF_XUP,
                        (MouseButton::X2, true)  => MOUSEEVENTF_XDOWN,
                        (MouseButton::X2, false) => MOUSEEVENTF_XUP,
                    };
                    let x_data = match button {
                        MouseButton::X1 => 1u32,
                        MouseButton::X2 => 2u32,
                        _ => 0,
                    };
                    let input = INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0, dy: 0,
                                mouseData: x_data,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 { return Err(InjectError::SendInput("MouseButton".into())); }
                }
                InputEvent::MouseWheel { dx, dy } => {
                    // Vertical wheel
                    if dy != 0 {
                        let input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 { mi: MOUSEINPUT {
                                dx: 0, dy: 0,
                                mouseData: dy as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                time: 0,
                                dwExtraInfo: 0,
                            }},
                        };
                        SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    }
                    if dx != 0 {
                        let input = INPUT {
                            r#type: INPUT_MOUSE,
                            Anonymous: INPUT_0 { mi: MOUSEINPUT {
                                dx: 0, dy: 0,
                                mouseData: dx as u32,
                                dwFlags: MOUSEEVENTF_HWHEEL,
                                time: 0,
                                dwExtraInfo: 0,
                            }},
                        };
                        SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    }
                }
                InputEvent::Key { scancode, pressed } => {
                    let mut flags = KEYEVENTF_SCANCODE;
                    if !pressed { flags |= KEYEVENTF_KEYUP; }
                    // Extended keys (arrow keys, etc.) use 0xE0 prefix in scancode.
                    if scancode & 0xFF00 == 0xE000 {
                        flags |= KEYEVENTF_EXTENDEDKEY;
                    }
                    let input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 { ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: (scancode & 0xFF) as u16,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        }},
                    };
                    let sent = SendInput(&[input], mem::size_of::<INPUT>() as i32);
                    if sent == 0 { return Err(InjectError::SendInput("Key".into())); }
                }
            }
        }
        Ok(())
    }
}
```

### capturer.rs

```rust
//! Capture InputEvents on the viewer. Integrates with winit WindowEvent stream.
//! Callers push events from their winit event loop into the capturer, and
//! consume them via an mpsc channel.

use prdt_protocol::{InputEvent, MouseButton};
use tokio::sync::mpsc;

pub struct RawInputCapturer {
    tx: mpsc::UnboundedSender<InputEvent>,
}

impl RawInputCapturer {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InputEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn emit(&self, ev: InputEvent) {
        let _ = self.tx.send(ev);
    }

    /// Convert a winit MouseButton to protocol MouseButton.
    pub fn map_winit_mouse_button(b: winit::event::MouseButton) -> Option<MouseButton> {
        use winit::event::MouseButton as W;
        Some(match b {
            W::Left => MouseButton::Left,
            W::Right => MouseButton::Right,
            W::Middle => MouseButton::Middle,
            W::Back => MouseButton::X1,
            W::Forward => MouseButton::X2,
            W::Other(_) => return None,
        })
    }
}
```

Note: capturer.rs requires winit as a dep. Add to input-win Cargo.toml:
```toml
winit = "0.30"
```

### lib.rs

```rust
//! Windows input capture (RawInput via winit) and injection (SendInput).

#![cfg(windows)]

pub mod capturer;
pub mod injector;

pub use capturer::RawInputCapturer;
pub use injector::{InjectError, SendInputInjector};
```

### Inline tests

Add minimal tests to each file that exercise construction (actual SendInput will succeed — we don't test it in unit tests because it sends to the actual desktop).

```rust
// At bottom of injector.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injector_constructs() {
        let _inj = SendInputInjector::new();
    }
}

// At bottom of capturer.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturer_channel_roundtrip() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (cap, mut rx) = RawInputCapturer::new();
            cap.emit(InputEvent::Key { scancode: 0x1E, pressed: true });
            let ev = rx.recv().await.unwrap();
            assert!(matches!(ev, InputEvent::Key { scancode: 0x1E, pressed: true }));
        });
    }
}
```

Commit: `feat(input-win): add SendInputInjector and RawInputCapturer`

---

## Task 2: MfD3d11Consumer D3D11 texture output

**Files:**
- Modify: `crates/media-win/src/mf/decoder.rs`
- Modify: `crates/media-win/src/pipeline/consumer.rs`

Add to `H265Decoder`:
```rust
/// Like `process_output` but returns an ID3D11Texture2D directly (zero-copy)
/// via IMFDXGIBuffer. The texture is NV12 format at the decoder's configured
/// width/height, possibly with extra alignment.
pub fn process_output_texture(&mut self) -> Result<Option<crate::d3d11::D3d11Texture>> {
    // Similar to process_output but instead of ConvertToContiguousBuffer + Lock:
    // - Call sample.GetBufferByIndex(0) to get IMFMediaBuffer
    // - Cast to IMFDXGIBuffer via QueryInterface
    // - GetResource::<ID3D11Texture2D>() returns the NV12 texture
    // - Wrap via D3d11Texture::from_raw with NV12 format
    //
    // Handle MF_E_TRANSFORM_NEED_MORE_INPUT and MF_E_TRANSFORM_STREAM_CHANGE
    // identically to process_output.
    todo!("implement using IMFDXGIBuffer::GetResource")
}
```

The subagent should implement this using the actual windows crate APIs. Reference: https://learn.microsoft.com/en-us/windows/win32/api/mfobjects/nn-mfobjects-imfdxgibuffer

Add `take_latest_texture()` to `MfD3d11Consumer` as the new preferred path. Keep `take_latest_frame()` as a bytes fallback for tests.

Commit: `feat(media-win): expose decoded NV12 as ID3D11Texture2D (zero-copy)`

---

## Task 3: D3D11 SwapChain + NV12→BGRA renderer

**Files:**
- Create: `crates/media-win/src/d3d11/swapchain.rs`
- Create: `crates/media-win/src/d3d11/nv12_renderer.rs`
- Modify: `crates/media-win/src/d3d11/mod.rs`

### swapchain.rs

```rust
//! D3D11 swapchain wrapping an HWND. Creates a DXGI flip-model swapchain
//! and exposes a backbuffer ID3D11RenderTargetView that `nv12_renderer`
//! writes into.
//
// API sketch:
//   pub struct SwapChain {
//       dev: D3d11Device,
//       swap: IDXGISwapChain1,
//       rtv: ID3D11RenderTargetView,
//       width: u32,
//       height: u32,
//   }
//
//   impl SwapChain {
//       pub fn new_for_hwnd(dev: &D3d11Device, hwnd: HWND, width: u32, height: u32) -> Result<Self>;
//       pub fn rtv(&self) -> &ID3D11RenderTargetView;
//       pub fn present(&self, vsync: bool) -> Result<()>;
//   }
```

Use `IDXGIFactory2::CreateSwapChainForHwnd` with:
- `DXGI_SWAP_EFFECT_FLIP_DISCARD`
- `BufferCount = 2`
- `Format = DXGI_FORMAT_B8G8R8A8_UNORM`
- `SampleDesc = { 1, 0 }`
- `AlphaMode = DXGI_ALPHA_MODE_IGNORE`
- `Flags = DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING` (for --no-vsync mode)

### nv12_renderer.rs

Render pass that takes an NV12 D3D11 texture and draws to the swapchain RTV via a pixel shader.

Strategy: full-screen triangle vertex shader + NV12-sampling pixel shader.

```hlsl
// vs: output clip-space full-screen triangle from vertex id
// ps: sample Y from texture slot 0 (R8_UNORM view of NV12 plane 0)
//     sample UV from texture slot 1 (R8G8_UNORM view of NV12 plane 1)
//     combine with BT.709 full-range conversion matrix
//     output RGBA
```

Build the shaders with `D3DCompile` at runtime (or pre-compile to DXBC and embed). For Phase 0 simplicity, use runtime compile via windows crate `D3DCompile`.

Helpers needed:
- Create shader resource views (SRVs) on the NV12 texture: one R8 view for Y (plane index 0), one R8G8 view for UV (plane index 1)
- Bind the two SRVs to pixel shader slots 0 and 1
- `OMSetRenderTargets(swapchain_rtv)` + `DrawInstanced(3, 1, 0, 0)` for the fullscreen tri

**IMPORTANT:** This is about 300-500 lines of D3D11 code. The implementer should follow Microsoft's DirectX-Graphics-Samples patterns. An alternative is using `ID3D11VideoProcessor` which does the conversion internally — that's 200 fewer lines but more API surface. The subagent should pick whichever they find less error-prone and document the choice.

Commit:
- `feat(media-win): add D3D11 swapchain wrapper`
- `feat(media-win): add NV12→BGRA shader renderer`

---

## Task 4: `host` binary

**Files:**
- Modify: `crates/host/Cargo.toml`
- Rewrite: `crates/host/src/main.rs`

### Cargo.toml additions

```toml
[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = "1"

[target.'cfg(windows)'.dependencies]
prdt-media-win = { path = "../media-win" }
prdt-input-win = { path = "../input-win" }
```

### main.rs

```rust
#![cfg(windows)]

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use prdt_media_win::{
    dxgi::enumerate_outputs_for_adapter, pick_default_adapter, D3d11Device, DxgiNvencProducer,
};
use prdt_input_win::SendInputInjector;
use prdt_protocol::{VideoProducer, ControlMessage};
use prdt_transport::{
    host_handshake, now_monotonic_us, CustomUdpTransport, ReceivedMessage, Transport,
    UdpTransportConfig,
};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "prdt-host")]
struct Args {
    /// Local bind address, e.g. 0.0.0.0:9000.
    #[arg(long, default_value = "0.0.0.0:9000")]
    bind: SocketAddr,

    /// Monitor output index (from enumerate_outputs).
    #[arg(long, default_value_t = 0u32)]
    monitor: u32,

    /// Target bitrate in Mbps (e.g., 30 for 30 Mbps).
    #[arg(long, default_value_t = 30u32)]
    bitrate_mbps: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into())
        )
        .init();

    let args = Args::parse();
    let adapter = pick_default_adapter().context("no GPU adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;
    let outputs = enumerate_outputs_for_adapter(&adapter).context("outputs")?;
    let output = outputs.get(args.monitor as usize)
        .context(format!("no output at index {}", args.monitor))?
        .clone();

    info!(
        monitor = args.monitor,
        device_name = %output.device_name,
        bitrate_mbps = args.bitrate_mbps,
        "host starting"
    );

    // Bind UDP first; wait for viewer to say Hello.
    let cfg = UdpTransportConfig {
        session_id: 0, // client picks
        ..Default::default()
    };
    let transport = Arc::new(
        CustomUdpTransport::bind(args.bind, cfg).await.context("UDP bind")?,
    );
    info!(local = ?transport.local_addr()?, "listening");

    // Wait for Hello, send HelloAck.
    let session_id: u64 = 0xDEADBEEF; // stable ID for Phase 0; randomize in Plan 4
    let req = host_handshake(
        &*transport,
        session_id,
        now_monotonic_us(),
        args.bitrate_mbps * 1_000_000,
        Duration::from_secs(60),
    )
    .await
    .context("handshake")?;
    info!(?req, "handshake complete");

    // Build producer.
    let mut producer = DxgiNvencProducer::new(&dev, &output, args.bitrate_mbps * 1_000_000)
        .context("producer")?;

    // Spawn video loop.
    let tx_clone = Arc::clone(&transport);
    let video = tokio::spawn(async move {
        loop {
            match producer.next_frame().await {
                Ok(frame) => {
                    if let Err(e) = tx_clone.send_video(frame).await {
                        warn!(?e, "send_video error; continuing");
                    }
                }
                Err(e) => {
                    warn!(?e, "producer error; continuing");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    });

    // Spawn input injection loop.
    let inject_tx = Arc::clone(&transport);
    let injector = SendInputInjector::new();
    let input = tokio::spawn(async move {
        loop {
            match inject_tx.recv().await {
                Ok(ReceivedMessage::Input(ev)) => {
                    if let Err(e) = injector.inject(ev) {
                        warn!(?e, "inject error");
                    }
                }
                Ok(ReceivedMessage::Control(ControlMessage::Bye)) => {
                    info!("peer sent Bye");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(?e, "recv error");
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = video => info!("video task ended"),
        _ = input => info!("input task ended"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received"),
    }
    Ok(())
}
```

Commit: `feat(host): implement host binary with capture+encode loop + input injection`

---

## Task 5: `viewer` binary

**Files:**
- Modify: `crates/viewer/Cargo.toml`
- Rewrite: `crates/viewer/src/main.rs`

### Cargo.toml additions

```toml
winit = "0.30"
raw-window-handle = "0.6"
```

### main.rs (simplified — see below for structure)

The viewer binary is substantial because it needs a winit event loop that:
1. Creates a window
2. Initializes D3D11 swapchain for that window's HWND
3. Initializes `MfD3d11Consumer`
4. Runs 3 concurrent tasks:
   - tokio: UDP recv → consumer.submit
   - tokio: drain input_event_rx → send_input on transport
   - main thread: winit event loop handles WindowEvent (input + redraw)

The tricky bit: winit's event loop MUST be on the main thread. tokio tasks run on worker threads. Communication between them is via channels.

The implementer should structure it like:
- `EventLoop::run_app` with a custom `ApplicationHandler`
- `ApplicationHandler::window_event` pushes InputEvents to the capturer mpsc
- A separate `std::thread::spawn` hosts the tokio runtime with recv/send loops
- Shared state: `Arc<Mutex<Option<D3d11Texture>>>` for the latest decoded frame, read by the redraw handler in the main thread

This is ~300-500 lines of glue code. Provide the skeleton and let the implementer fill in.

Commit: `feat(viewer): implement viewer binary with winit + swapchain + decode`

---

## Task 6: Smoke test procedure

**Files:**
- Create: `docs/superpowers/plan3-manual-smoke.md`

Manual test procedure:

### Single-machine loopback
```powershell
# Terminal 1
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo run -p prdt-host --release -- --bind 127.0.0.1:9000 --monitor 0 --bitrate-mbps 20

# Terminal 2
cargo run -p prdt-viewer --release -- --host 127.0.0.1:9000
```
Expected: viewer window opens, shows the host's desktop. Moving the mouse in the viewer window moves the cursor on the host.

### Two-machine LAN
Same as above but Host uses `--bind 0.0.0.0:9000` and Viewer uses `--host <host-ip>:9000`.

### Known limitations
- HDR displays may render incorrectly (spec defers to Phase 1)
- Alt-Tab / UAC / full-screen games may cause DXGI AccessLost (Plan 4 adds F6 recovery)
- Session_id is hardcoded 0xDEADBEEF (no real sessions/auth; Plan 5)

Commit: `docs: add Plan 3 manual smoke test procedure`

---

## Task 7: Final README + tag

Update README.md to check Plan 3 complete. Commit + tag `phase0-plan3-complete`.

---

## Plan 3 Exit Criteria

- [ ] `cargo build --release --workspace` succeeds
- [ ] Host binary starts, binds UDP, logs "listening"
- [ ] Viewer binary starts, opens a window
- [ ] Handshake completes (Hello + HelloAck)
- [ ] Video frames flow end-to-end (viewer window displays host's desktop)
- [ ] Mouse movement in viewer window is reflected in host's cursor
- [ ] Keyboard input in viewer window is injected on host
- [ ] Clippy clean
- [ ] Tag `phase0-plan3-complete`

---

## Known Risks

1. **Mouse coordinate mapping**: viewer window mouse coords are window-local, but host `SendInput` with ABSOLUTE expects screen-space. Need to scale viewer (mouse_x/mouse_y) → (abs_x, abs_y) based on the captured output's resolution. The `MouseMove { absolute: true }` path in injector.rs assumes coords are already in the 0..65535 absolute range; the viewer must do the scaling.
2. **Keyboard scancode round-trip**: viewer and host might have different keyboard layouts. We use scancodes (physical keys) which avoids layout translation. Virtual keys are not used.
3. **Window resize**: if the viewer window is resized, the D3D11 swapchain must be resized too. winit emits `WindowEvent::Resized`.
4. **NV12 texture size padding**: MF decoder may return NV12 textures with width padded to 16/32 pixels. The shader should sample based on stream dimensions (stored in `EncodedFrame.width/height`), not texture dimensions.
5. **UAC prompt**: DXGI can't capture UAC prompts; those appear as black frames. Expected, not a bug.

---

*End of Phase 0 — Plan 3 of 4.*
