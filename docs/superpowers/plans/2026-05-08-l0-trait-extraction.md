# L0: Trait Extraction + Windows Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce `prdt-media-core` and `prdt-input-core` crates with cross-platform trait definitions, implement those traits as adapters on the existing Windows backends (`HwHevcEncoder`, `SendInputInjector`, etc.), and add empty `prdt-media-linux` and `prdt-input-linux` skeleton crates for L1. Windows host/viewer behavior is unchanged — this is pure scaffolding with regression-zero on Windows.

**Architecture:** New `crates/media-core` defines `Capturer`, `Encoder`, `Decoder` traits and a minimal `EncodedPacket` type with no OS deps. New `crates/input-core` defines `InputInjector`, `ClipboardProvider`, `VirtualDesktopGeometry` traits. Existing Windows backends gain `impl` blocks for those traits via thin adapter modules (`media-win/src/core_adapter.rs`, `input-win/src/core_adapter.rs`). New `crates/media-linux` and `crates/input-linux` are added as `#![cfg(target_os = "linux")]` skeleton crates that compile to empty on Windows. Host / viewer code is **not** rewired in L0 — that happens in a follow-up plan once the trait surface is validated.

**Tech Stack:** Rust 1.85, Cargo workspace, existing crates `prdt-protocol` (read-only — for `EncodedFrame` reference only), `prdt-media-win`, `prdt-input-win`. No new external dependencies.

**Branch:** `phase-l0-trait-extraction`, branched from `main`.

**Regression bar:** `cargo check --workspace`, `cargo test --workspace --exclude prdt-latency-bench` (bench test is hardware-gated), and `cargo clippy --workspace --all-targets -- -D warnings` MUST stay green on Windows. Existing tests in `crates/media-win/tests/` MUST continue to pass.

---

### Task 0: Branch setup

**Files:** none

- [ ] **Step 1: Verify clean working tree on the current branch (uncommitted source files)**

Run:
```bash
git status --porcelain | grep -v '^??' | head -20
```
Expected: empty output (no tracked-file modifications). Untracked files like `host-key.bin`, `host.err` are fine; the L0 work won't touch them.

If there are tracked modifications, stop and surface them to the user before continuing.

- [ ] **Step 2: Fetch latest main and branch off**

Run:
```bash
git fetch origin
git checkout main
git pull --ff-only origin main
git checkout -b phase-l0-trait-extraction
```
Expected: now on `phase-l0-trait-extraction`, `git status` shows the working tree at main + the same untracked files as before.

- [ ] **Step 3: Smoke-build the existing workspace as a baseline**

Run:
```bash
cargo check --workspace 2>&1 | tail -20
```
Expected: completes without errors. Compile warnings are tolerable; record any new errors as a regression-blocker before continuing.

---

### Task 1: Create `prdt-media-core` crate skeleton

**Files:**
- Create: `crates/media-core/Cargo.toml`
- Create: `crates/media-core/src/lib.rs`
- Modify: `Cargo.toml` (workspace root) — add `crates/media-core` to `members`

- [ ] **Step 1: Create `crates/media-core/Cargo.toml`**

```toml
[package]
name = "prdt-media-core"
version = "0.0.1"
edition.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
thiserror.workspace = true
```

- [ ] **Step 2: Create `crates/media-core/src/lib.rs` with the encoded-packet type and an error type**

```rust
//! Cross-platform media abstractions used by host (capture/encode) and
//! viewer (decode/render). OS-neutral by design — this crate must NOT
//! depend on `prdt-media-win`, `windows`, X11, Wayland, or any GPU SDK.
//!
//! L1+ Linux backends (`prdt-media-linux`) and the existing Windows
//! backend (`prdt-media-win`) implement the traits in this crate via
//! per-OS adapter modules.

#![forbid(unsafe_code)]

pub mod error;
pub mod frame;
pub mod traits;

pub use error::{CaptureError, DecodeError, EncodeError};
pub use frame::EncodedPacket;
pub use traits::{Capturer, Decoder, Encoder};
```

- [ ] **Step 3: Create `crates/media-core/src/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("backend lost (display reset / device removal): {0}")]
    BackendLost(String),
    #[error("capture backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder backend error: {0}")]
    Backend(String),
    #[error("input frame format mismatch: {0}")]
    FormatMismatch(String),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("decoder backend error: {0}")]
    Backend(String),
    #[error("bitstream parse error: {0}")]
    Bitstream(String),
}
```

- [ ] **Step 4: Create `crates/media-core/src/frame.rs`**

```rust
/// One encoded video access unit (Annex-B byte stream of NAL units, or
/// equivalent for non-NAL codecs). Pipeline-level metadata (`seq`,
/// `width`, `height`, `codec`) lives on `prdt_protocol::EncodedFrame`
/// — `EncodedPacket` is the codec-output side of the boundary, before
/// the producer wraps it for the wire.
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp_us: u64,
}
```

- [ ] **Step 5: Create `crates/media-core/src/traits.rs`**

```rust
use crate::{CaptureError, DecodeError, EncodeError, EncodedPacket};

/// Pulls one frame from a screen-capture backend. The `Frame` associated
/// type is OS-specific (e.g. `D3d11Texture` on Windows, a DMA-BUF FD or
/// CPU `BgraFrame` on Linux). The producer-level pipeline keeps the
/// concrete type erased behind `prdt_protocol::VideoProducer`; this
/// trait is for the inner capture component.
pub trait Capturer: Send {
    type Frame;

    fn next_frame(&mut self) -> Result<Self::Frame, CaptureError>;
}

/// Encodes one captured frame into one `EncodedPacket`. The `Frame`
/// associated type matches the paired `Capturer::Frame` (Capturer and
/// Encoder are typically constructed together for one backend).
pub trait Encoder: Send {
    type Frame;

    fn encode(
        &mut self,
        frame: &Self::Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError>;

    fn set_target_bitrate(&mut self, bps: u32);
    fn backend_name(&self) -> &'static str;
}

/// Decodes one encoded packet to a backend-specific decoded frame
/// (typically a GPU texture or CPU YUV buffer). Like `Capturer`, the
/// `Frame` type is OS-/backend-specific.
pub trait Decoder: Send {
    type Frame;

    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<Self::Frame>, DecodeError>;
    fn backend_name(&self) -> &'static str;
}
```

- [ ] **Step 6: Add `crates/media-core` to workspace members**

Edit the `members = [...]` array in `Cargo.toml` (workspace root). Insert `"crates/media-core",` immediately after `"crates/protocol",`. Do not change the order of other entries.

The block before:
```toml
members = [
    "crates/protocol",
    "crates/transport",
```
becomes:
```toml
members = [
    "crates/protocol",
    "crates/media-core",
    "crates/transport",
```

- [ ] **Step 7: Verify the new crate builds and types are sound**

Run:
```bash
cargo check -p prdt-media-core 2>&1 | tail -5
```
Expected: `Finished` (or `Checking prdt-media-core` followed by `Finished`). No errors. No warnings about unused imports.

- [ ] **Step 8: Add a unit test for the trait-object compatibility of `Encoder`**

Create `crates/media-core/tests/trait_objects.rs`:

```rust
use prdt_media_core::{EncodeError, EncodedPacket, Encoder};

struct DummyEncoder;

impl Encoder for DummyEncoder {
    type Frame = ();

    fn encode(
        &mut self,
        _frame: &Self::Frame,
        _force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        Ok(EncodedPacket {
            nal_bytes: Vec::new(),
            is_keyframe: false,
            timestamp_us,
        })
    }

    fn set_target_bitrate(&mut self, _bps: u32) {}
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

#[test]
fn dummy_encoder_round_trip() {
    let mut enc = DummyEncoder;
    let p = enc.encode(&(), false, 12_345).expect("encode");
    assert_eq!(p.timestamp_us, 12_345);
    assert!(!p.is_keyframe);
    assert_eq!(enc.backend_name(), "dummy");
}
```

- [ ] **Step 9: Run the test**

Run:
```bash
cargo test -p prdt-media-core 2>&1 | tail -10
```
Expected: `test dummy_encoder_round_trip ... ok`, `test result: ok. 1 passed`.

- [ ] **Step 10: Commit**

```bash
git add crates/media-core Cargo.toml
git commit -m "L0 Task 1: add prdt-media-core crate with Capturer/Encoder/Decoder traits"
```

---

### Task 2: Create `prdt-input-core` crate with input/clipboard/desktop traits

**Files:**
- Create: `crates/input-core/Cargo.toml`
- Create: `crates/input-core/src/lib.rs`
- Create: `crates/input-core/src/error.rs`
- Create: `crates/input-core/src/traits.rs`
- Create: `crates/input-core/tests/trait_objects.rs`
- Modify: `Cargo.toml` (workspace root) — add `crates/input-core` to `members`

- [ ] **Step 1: Create `crates/input-core/Cargo.toml`**

```toml
[package]
name = "prdt-input-core"
version = "0.0.1"
edition.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
thiserror.workspace = true
```

- [ ] **Step 2: Create `crates/input-core/src/lib.rs`**

```rust
//! Cross-platform input + clipboard + virtual-desktop abstractions.
//!
//! OS-neutral. The Windows backend (`prdt-input-win`) and the future
//! Linux backend (`prdt-input-linux`) implement these traits via
//! per-OS adapters. Host / viewer code switches to these traits in a
//! follow-up plan; L0 only introduces the abstraction.

#![forbid(unsafe_code)]

pub mod error;
pub mod traits;

pub use error::{ClipboardError, InjectError};
pub use traits::{ClipboardProvider, InputInjector, VirtualDesktopGeometry};
```

- [ ] **Step 3: Create `crates/input-core/src/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("input injection backend error: {0}")]
    Backend(String),
    #[error("permission denied (uinput / portal access not granted): {0}")]
    PermissionDenied(String),
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard backend error: {0}")]
    Backend(String),
    #[error("no text content available")]
    NoText,
    #[error("clipboard payload too large: {0} bytes")]
    TooLarge(usize),
}
```

- [ ] **Step 4: Create `crates/input-core/src/traits.rs`**

```rust
use prdt_protocol::{InputEvent, MonitorRect};

use crate::{ClipboardError, InjectError};

/// Inject one `InputEvent` (mouse move / button / wheel / key) into the
/// host's local input system. Synchronous and best-effort.
pub trait InputInjector: Send {
    fn inject(&self, event: InputEvent) -> Result<(), InjectError>;
    fn backend_name(&self) -> &'static str;
}

/// Read / write the user's primary clipboard text channel. Backends may
/// hold transient state (e.g. a Wayland portal handle) so the trait
/// requires `&mut self` for both calls.
pub trait ClipboardProvider: Send {
    fn read_text(&mut self) -> Result<String, ClipboardError>;
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;

    /// Monotonic counter that bumps each time the user changes the
    /// system clipboard. Used by the host's clipboard-sync poller to
    /// avoid round-tripping unchanged content.
    fn sequence_number(&self) -> u64;

    fn backend_name(&self) -> &'static str;
}

/// Returns the bounding rect of the host's combined virtual desktop in
/// the same coordinate system the host backend's `InputInjector`
/// expects for absolute pointer events.
pub trait VirtualDesktopGeometry: Send {
    fn virtual_desktop_rect(&self) -> MonitorRect;
}
```

- [ ] **Step 5: Add `crates/input-core` to workspace members**

Edit `Cargo.toml` (workspace root) `members = [...]` array. Insert `"crates/input-core",` immediately after the `"crates/media-core",` entry added in Task 1.

- [ ] **Step 6: Verify the new crate builds**

Run:
```bash
cargo check -p prdt-input-core 2>&1 | tail -5
```
Expected: completes without errors.

- [ ] **Step 7: Add a trait-object compatibility test**

Create `crates/input-core/tests/trait_objects.rs`:

```rust
use prdt_input_core::{
    ClipboardError, ClipboardProvider, InjectError, InputInjector, VirtualDesktopGeometry,
};
use prdt_protocol::{InputEvent, MonitorRect};

struct DummyInjector;

impl InputInjector for DummyInjector {
    fn inject(&self, _event: InputEvent) -> Result<(), InjectError> {
        Ok(())
    }
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

struct DummyClipboard {
    seq: u64,
    text: String,
}

impl ClipboardProvider for DummyClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok(self.text.clone())
    }
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.text = text.to_string();
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }
    fn sequence_number(&self) -> u64 {
        self.seq
    }
    fn backend_name(&self) -> &'static str {
        "dummy"
    }
}

struct DummyDesktop;

impl VirtualDesktopGeometry for DummyDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect {
        MonitorRect::new(0, 0, 1920, 1080)
    }
}

#[test]
fn injector_through_dyn() {
    let inj: &dyn InputInjector = &DummyInjector;
    inj.inject(InputEvent::MouseMove {
        x: 0,
        y: 0,
        absolute: true,
    })
    .expect("inject");
    assert_eq!(inj.backend_name(), "dummy");
}

#[test]
fn clipboard_round_trip() {
    let mut cb = DummyClipboard {
        seq: 0,
        text: String::new(),
    };
    cb.write_text("hello").expect("write");
    assert_eq!(cb.read_text().expect("read"), "hello");
    assert_eq!(cb.sequence_number(), 1);
}

#[test]
fn desktop_rect_dimensions() {
    let d = DummyDesktop;
    let r = d.virtual_desktop_rect();
    assert_eq!(r.width(), 1920);
    assert_eq!(r.height(), 1080);
}
```

- [ ] **Step 8: Run the test**

Run:
```bash
cargo test -p prdt-input-core 2>&1 | tail -15
```
Expected: 3 tests passing.

- [ ] **Step 9: Commit**

```bash
git add crates/input-core Cargo.toml
git commit -m "L0 Task 2: add prdt-input-core crate with InputInjector / ClipboardProvider / VirtualDesktopGeometry traits"
```

---

### Task 3: Add Windows adapter — `prdt-media-win` implements `prdt-media-core::Encoder`

**Files:**
- Modify: `crates/media-win/Cargo.toml` — add `prdt-media-core` path dep
- Create: `crates/media-win/src/core_adapter.rs`
- Modify: `crates/media-win/src/lib.rs` — register the new module

- [ ] **Step 1: Add the dep to `crates/media-win/Cargo.toml`**

Open `crates/media-win/Cargo.toml`, in the `[dependencies]` section add:
```toml
prdt-media-core = { path = "../media-core" }
```
Place it alphabetically between any existing `prdt-*` deps. Do not remove or reorder other entries.

- [ ] **Step 2: Create `crates/media-win/src/core_adapter.rs`**

```rust
//! Adapter shim: implements `prdt_media_core::Encoder` (cross-platform
//! trait) on top of the existing `Hevc265Encoder` / `HwHevcEncoder`
//! Windows-specific traits.
//!
//! L0 only — host / viewer code is not yet rewired to consume the
//! `prdt_media_core::Encoder` trait. This module exists so the trait
//! surface is exercised on Windows (smoke test below) and so the L1
//! Linux work has a precedent to mirror.

use prdt_media_core::{EncodeError, EncodedPacket, Encoder};

use crate::d3d11::D3d11Texture;
use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder, HwHevcEncoder};
use crate::error::MediaError;

impl Encoder for HwHevcEncoder {
    type Frame = D3d11Texture;

    fn encode(
        &mut self,
        frame: &Self::Frame,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedPacket, EncodeError> {
        <HwHevcEncoder as Hevc265Encoder>::encode(self, frame, force_idr, timestamp_us)
            .map(into_packet)
            .map_err(map_err)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        <HwHevcEncoder as Hevc265Encoder>::set_target_bitrate(self, bps);
    }

    fn backend_name(&self) -> &'static str {
        <HwHevcEncoder as Hevc265Encoder>::backend_name(self)
    }
}

fn into_packet(frame: EncodedH265Frame) -> EncodedPacket {
    EncodedPacket {
        nal_bytes: frame.nal_bytes,
        is_keyframe: frame.is_keyframe,
        timestamp_us: frame.timestamp,
    }
}

fn map_err(err: MediaError) -> EncodeError {
    EncodeError::Backend(err.to_string())
}
```

- [ ] **Step 3: Register the module in `crates/media-win/src/lib.rs`**

Open `crates/media-win/src/lib.rs`. Below the existing `pub mod synthetic;` line, add:
```rust
pub mod core_adapter;
```
Do not modify the existing `pub use ...` lines — the adapter implements a foreign trait, no new symbols need re-exporting.

- [ ] **Step 4: Verify the adapter compiles**

Run:
```bash
cargo check -p prdt-media-win 2>&1 | tail -10
```
Expected: completes without errors. Warnings about unused fields are OK if pre-existing.

- [ ] **Step 5: Add a smoke test that drives `HwHevcEncoder` through `prdt_media_core::Encoder`**

Create `crates/media-win/tests/core_adapter_smoke.rs`:

```rust
//! Verifies that `HwHevcEncoder` is usable through the cross-platform
//! `prdt_media_core::Encoder` trait. This test does NOT exercise the
//! GPU encode path — that's covered by the existing
//! `nvenc_smoke` / `pipeline_smoke` tests. We only check trait-method
//! dispatch (object safety + signature compatibility).

#![cfg(windows)]

use prdt_media_core::Encoder;
use prdt_media_win::{HwHevcEncoder, MfH265Encoder};

/// Compile-time witness that `HwHevcEncoder` implements
/// `prdt_media_core::Encoder<Frame = D3d11Texture>`. If this stops
/// compiling, the adapter signature is no longer compatible.
fn _witness_hwencoder_impls_encoder<E: Encoder>(_e: &mut E) {}

#[test]
fn hwencoder_witness_compiles() {
    fn _f(e: &mut HwHevcEncoder) {
        _witness_hwencoder_impls_encoder(e);
    }
    // No body needed — the witness check is at compile time.
    // Keep this assertion so the test runner reports the test
    // result instead of skipping silently.
    assert_eq!(std::mem::size_of::<&MfH265Encoder>(), std::mem::size_of::<usize>());
}
```

- [ ] **Step 6: Run the test**

Run:
```bash
cargo test -p prdt-media-win --test core_adapter_smoke 2>&1 | tail -10
```
Expected: `test hwencoder_witness_compiles ... ok`. If the `_witness_*` function fails to compile, the adapter signature has drifted from `prdt_media_core::Encoder`.

- [ ] **Step 7: Run the existing media-win test suite to verify no regression**

Run:
```bash
cargo test -p prdt-media-win 2>&1 | tail -20
```
Expected: same set of tests pass as before plus the new one. No tests change from `pass` to `fail`.

- [ ] **Step 8: Commit**

```bash
git add crates/media-win/Cargo.toml crates/media-win/src/lib.rs crates/media-win/src/core_adapter.rs crates/media-win/tests/core_adapter_smoke.rs
git commit -m "L0 Task 3: media-win impls prdt-media-core::Encoder for HwHevcEncoder"
```

---

### Task 4: Add Windows adapter — `prdt-input-win` implements all three `prdt-input-core` traits

**Files:**
- Modify: `crates/input-win/Cargo.toml` — add `prdt-input-core` path dep
- Create: `crates/input-win/src/core_adapter.rs`
- Modify: `crates/input-win/src/lib.rs` — register the new module + export adapter types

- [ ] **Step 1: Add the dep to `crates/input-win/Cargo.toml`**

In `[dependencies]`, add:
```toml
prdt-input-core = { path = "../input-core" }
```

- [ ] **Step 2: Create `crates/input-win/src/core_adapter.rs`**

```rust
//! Adapter shim: implements `prdt_input_core` traits on the existing
//! `SendInputInjector` (input injection), the function-style clipboard
//! API (wrapped in a stateful struct), and `virtual_desktop_rect()`.

use prdt_input_core::{
    ClipboardError as CoreClipboardError, ClipboardProvider, InjectError as CoreInjectError,
    InputInjector, VirtualDesktopGeometry,
};
use prdt_protocol::{InputEvent, MonitorRect};

use crate::clipboard::{
    clipboard_sequence_number, read_clipboard_text, write_clipboard_text, ClipboardError,
};
use crate::desktop::virtual_desktop_rect;
use crate::injector::{InjectError as WinInjectError, SendInputInjector};

impl InputInjector for SendInputInjector {
    fn inject(&self, event: InputEvent) -> Result<(), CoreInjectError> {
        SendInputInjector::inject(self, event).map_err(map_inject_err)
    }

    fn backend_name(&self) -> &'static str {
        "send-input"
    }
}

fn map_inject_err(err: WinInjectError) -> CoreInjectError {
    match err {
        WinInjectError::SendInput(s) => CoreInjectError::Backend(s),
    }
}

/// Stateful adapter around the function-style clipboard API. Holds the
/// last-observed sequence number so polling consumers can use a single
/// owner instead of calling the free function directly.
#[derive(Default)]
pub struct Win32Clipboard {
    last_seq: u64,
}

impl Win32Clipboard {
    pub fn new() -> Self {
        Self {
            last_seq: clipboard_sequence_number(),
        }
    }
}

impl ClipboardProvider for Win32Clipboard {
    fn read_text(&mut self) -> Result<String, CoreClipboardError> {
        self.last_seq = clipboard_sequence_number();
        read_clipboard_text().map_err(map_clipboard_err)
    }

    fn write_text(&mut self, text: &str) -> Result<(), CoreClipboardError> {
        write_clipboard_text(text).map_err(map_clipboard_err)?;
        self.last_seq = clipboard_sequence_number();
        Ok(())
    }

    fn sequence_number(&self) -> u64 {
        // Always read fresh — the underlying Win32 counter is monotonic
        // per-session, so we don't need to cache.
        clipboard_sequence_number()
    }

    fn backend_name(&self) -> &'static str {
        "win32-clipboard"
    }
}

fn map_clipboard_err(err: ClipboardError) -> CoreClipboardError {
    match err {
        ClipboardError::OpenFailed => {
            CoreClipboardError::Backend("OpenClipboard failed after retries".into())
        }
        ClipboardError::Windows(s) => CoreClipboardError::Backend(s),
        ClipboardError::TooLarge(n) => CoreClipboardError::TooLarge(n),
        ClipboardError::NoText => CoreClipboardError::NoText,
    }
}

#[derive(Default)]
pub struct Win32VirtualDesktop;

impl Win32VirtualDesktop {
    pub fn new() -> Self {
        Self
    }
}

impl VirtualDesktopGeometry for Win32VirtualDesktop {
    fn virtual_desktop_rect(&self) -> MonitorRect {
        virtual_desktop_rect()
    }
}
```

- [ ] **Step 3: Register the module + export adapter types in `crates/input-win/src/lib.rs`**

Modify `crates/input-win/src/lib.rs`. After the existing `pub mod injector;` line, add:
```rust
pub mod core_adapter;
```
Then below the existing `pub use injector::...` line, append:
```rust
pub use core_adapter::{Win32Clipboard, Win32VirtualDesktop};
```

- [ ] **Step 4: Verify the adapter compiles**

Run:
```bash
cargo check -p prdt-input-win 2>&1 | tail -10
```
Expected: completes without errors.

- [ ] **Step 5: Add a smoke test exercising all three traits via dyn dispatch**

Create `crates/input-win/tests/core_adapter_smoke.rs`:

```rust
//! Verifies that all three `prdt_input_core` traits are usable via
//! `dyn` references on the Windows adapter types. Does not exercise
//! actual SendInput / clipboard / monitor enumeration — those are
//! covered by `injector_constructs` and the desktop module's tests.

#![cfg(windows)]

use prdt_input_core::{ClipboardProvider, InputInjector, VirtualDesktopGeometry};
use prdt_input_win::{SendInputInjector, Win32Clipboard, Win32VirtualDesktop};

#[test]
fn injector_dyn_dispatch() {
    let injector = SendInputInjector::new();
    let dyn_inj: &dyn InputInjector = &injector;
    assert_eq!(dyn_inj.backend_name(), "send-input");
}

#[test]
fn clipboard_dyn_dispatch() {
    let mut cb = Win32Clipboard::new();
    let dyn_cb: &mut dyn ClipboardProvider = &mut cb;
    assert_eq!(dyn_cb.backend_name(), "win32-clipboard");
    // sequence_number() should not panic on a fresh session, even if
    // CI has no actual clipboard contents.
    let _ = dyn_cb.sequence_number();
}

#[test]
fn desktop_dyn_dispatch() {
    let d = Win32VirtualDesktop::new();
    let dyn_d: &dyn VirtualDesktopGeometry = &d;
    let r = dyn_d.virtual_desktop_rect();
    assert!(r.width() >= 0);
    assert!(r.height() >= 0);
}
```

- [ ] **Step 6: Run the test**

Run:
```bash
cargo test -p prdt-input-win --test core_adapter_smoke 2>&1 | tail -10
```
Expected: 3 tests passing.

- [ ] **Step 7: Run the full input-win suite to verify no regression**

Run:
```bash
cargo test -p prdt-input-win 2>&1 | tail -20
```
Expected: existing tests (`injector_constructs`, `virtual_desktop_has_positive_area`, etc.) still pass.

- [ ] **Step 8: Commit**

```bash
git add crates/input-win/Cargo.toml crates/input-win/src/lib.rs crates/input-win/src/core_adapter.rs crates/input-win/tests/core_adapter_smoke.rs
git commit -m "L0 Task 4: input-win impls prdt-input-core traits (InputInjector / ClipboardProvider / VirtualDesktopGeometry)"
```

---

### Task 5: Create `prdt-media-linux` skeleton crate

**Files:**
- Create: `crates/media-linux/Cargo.toml`
- Create: `crates/media-linux/src/lib.rs`
- Modify: `Cargo.toml` (workspace root) — add `crates/media-linux`

- [ ] **Step 1: Create `crates/media-linux/Cargo.toml`**

```toml
[package]
name = "prdt-media-linux"
version = "0.0.1"
edition.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
prdt-media-core = { path = "../media-core" }
prdt-protocol = { path = "../protocol" }
```

- [ ] **Step 2: Create `crates/media-linux/src/lib.rs`**

```rust
//! Linux media backend — empty skeleton for L1.
//!
//! This crate compiles to an empty library on non-Linux targets. On
//! Linux it will provide screen-capture (X11 / xdg-desktop-portal +
//! PipeWire) and encode/decode (VAAPI / NVENC Linux / software)
//! implementations of `prdt_media_core` traits in L1+.
//!
//! L0 deliverable: crate exists and is wired into the workspace so
//! the L1 implementer has a place to write code without restructuring
//! the workspace mid-flight.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

// Intentionally empty in L0. L1 will add:
//   pub mod x11_capture;
//   pub mod portal_capture;
//   pub mod vaapi_encode;
//   pub mod nvenc_linux;
//   pub mod ffmpeg_decode;
//   pub mod core_adapter;  // impls of prdt_media_core traits
```

- [ ] **Step 3: Add to workspace members**

Edit workspace `Cargo.toml`. After `"crates/media-win",` insert:
```toml
    "crates/media-linux",
```

- [ ] **Step 4: Verify it builds on Windows (must compile to empty)**

Run:
```bash
cargo check -p prdt-media-linux 2>&1 | tail -10
```
Expected: `Finished`. Because of `#![cfg(target_os = "linux")]` the lib body is excluded on Windows, and the crate produces an empty rlib. No errors.

- [ ] **Step 5: Commit**

```bash
git add crates/media-linux Cargo.toml
git commit -m "L0 Task 5: add prdt-media-linux skeleton crate (empty on non-Linux)"
```

---

### Task 6: Create `prdt-input-linux` skeleton crate

**Files:**
- Create: `crates/input-linux/Cargo.toml`
- Create: `crates/input-linux/src/lib.rs`
- Modify: `Cargo.toml` (workspace root) — add `crates/input-linux`

- [ ] **Step 1: Create `crates/input-linux/Cargo.toml`**

```toml
[package]
name = "prdt-input-linux"
version = "0.0.1"
edition.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
prdt-input-core = { path = "../input-core" }
prdt-protocol = { path = "../protocol" }
```

- [ ] **Step 2: Create `crates/input-linux/src/lib.rs`**

```rust
//! Linux input backend — empty skeleton for L1.
//!
//! This crate compiles to an empty library on non-Linux targets. On
//! Linux it will provide input injection (uinput, libei,
//! xdg-desktop-portal RemoteDesktop), clipboard sync (wl-clipboard /
//! arboard / portal Clipboard), and virtual-desktop geometry queries.
//!
//! L0 deliverable: crate exists and is wired into the workspace.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

// Intentionally empty in L0. L1+ will add:
//   pub mod uinput_injector;
//   pub mod libei_injector;
//   pub mod xtest_injector;
//   pub mod wl_clipboard;
//   pub mod x11_clipboard;
//   pub mod core_adapter;  // impls of prdt_input_core traits
```

- [ ] **Step 3: Add to workspace members**

Edit workspace `Cargo.toml`. After `"crates/input-win",` insert:
```toml
    "crates/input-linux",
```

- [ ] **Step 4: Verify it builds on Windows**

Run:
```bash
cargo check -p prdt-input-linux 2>&1 | tail -10
```
Expected: `Finished`. Empty rlib produced on Windows.

- [ ] **Step 5: Commit**

```bash
git add crates/input-linux Cargo.toml
git commit -m "L0 Task 6: add prdt-input-linux skeleton crate (empty on non-Linux)"
```

---

### Task 7: Workspace-wide verification — check, test, clippy

**Files:** none (verification only — no source changes expected)

- [ ] **Step 1: Full workspace check**

Run:
```bash
cargo check --workspace 2>&1 | tail -10
```
Expected: `Finished`. Total error count: 0. New warnings should be 0 — investigate any that appear.

- [ ] **Step 2: Full workspace clippy with errors-on-warning**

Run:
```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
```
Expected: `Finished`. If new clippy warnings show up in the new crates, fix them before continuing — the goal is regression-zero on lint.

- [ ] **Step 3: Run the new crates' own tests**

Run:
```bash
cargo test -p prdt-media-core -p prdt-input-core 2>&1 | tail -15
```
Expected: 4 tests passing total (1 from media-core, 3 from input-core).

- [ ] **Step 4: Run the adapter smoke tests**

Run:
```bash
cargo test -p prdt-media-win --test core_adapter_smoke -p prdt-input-win --test core_adapter_smoke 2>&1 | tail -15
```
Expected: 4 tests passing total (1 from media-win adapter + 3 from input-win adapter).

- [ ] **Step 5: Run the full media-win existing test suite**

Run:
```bash
cargo test -p prdt-media-win 2>&1 | tail -25
```
Expected: same passing-test count as before this plan started, plus the new adapter smoke. No previously-passing test should now fail or be skipped.

- [ ] **Step 6: Run the full input-win existing test suite**

Run:
```bash
cargo test -p prdt-input-win 2>&1 | tail -15
```
Expected: same as Step 5 — no regression.

- [ ] **Step 7: Run the workspace test suite, excluding the hardware-gated bench crate**

Run:
```bash
cargo test --workspace --exclude prdt-latency-bench 2>&1 | tail -20
```
Expected: `test result: ok` for every crate that has tests. If any crate prints `FAILED` summary, halt and inspect.

- [ ] **Step 8: If any verification step changed source (e.g. a clippy fix), commit it**

If `git status` shows new tracked-file modifications:
```bash
git add -u crates
git commit -m "L0 Task 7: clippy / format fixups from workspace verification"
```
Otherwise skip this step — no commit needed.

---

### Task 8: Documentation — drop a brief plan-completion note

**Files:**
- Create: `docs/superpowers/plans/2026-05-08-l0-trait-extraction-status.md`

- [ ] **Step 1: Write the status note**

Create `docs/superpowers/plans/2026-05-08-l0-trait-extraction-status.md`:

```markdown
# L0 Trait Extraction — Status

**Branch:** `phase-l0-trait-extraction`
**Plan:** `docs/superpowers/plans/2026-05-08-l0-trait-extraction.md`

## Delivered

- New `crates/media-core` (Capturer / Encoder / Decoder traits + `EncodedPacket`).
- New `crates/input-core` (InputInjector / ClipboardProvider / VirtualDesktopGeometry traits).
- `prdt-media-win::HwHevcEncoder` now `impl prdt_media_core::Encoder<Frame = D3d11Texture>`.
- `prdt-input-win::SendInputInjector` impls `prdt_input_core::InputInjector`.
- `prdt-input-win::Win32Clipboard` impls `prdt_input_core::ClipboardProvider`.
- `prdt-input-win::Win32VirtualDesktop` impls `prdt_input_core::VirtualDesktopGeometry`.
- Empty skeleton crates `crates/media-linux` and `crates/input-linux` wired into the workspace, gated by `#![cfg(target_os = "linux")]`.

## Not delivered (deferred)

- Host / viewer code is **not** rewired through the new traits. Trait surface is exercised only by the unit + smoke tests.
- No Linux implementation. `media-linux` / `input-linux` are empty.
- No `directories`-based key-path migration (that's a separate cross-platform PR).
- No README rewrite or platform-support matrix.

## Regression posture

- `cargo check --workspace` green on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` green on Windows.
- All previously-passing tests in `prdt-media-win` and `prdt-input-win` still pass.
- 8 new tests added across the new crates and adapter smoke suites; all green.

## Next plan

L1 — Linux PoC: X11 capture + media-sw encode + uinput inject. Plan to be written separately. Requires a Linux dev environment; do not start without one.
```

- [ ] **Step 2: Commit the status note**

```bash
git add docs/superpowers/plans/2026-05-08-l0-trait-extraction-status.md
git commit -m "L0 Task 8: status doc for the trait-extraction plan"
```

---

### Task 9: Final review and sign-off

**Files:** none

- [ ] **Step 1: Confirm branch state**

Run:
```bash
git log --oneline main..HEAD
```
Expected: 8 commits (one per Task 1–8 that produced changes; Task 0 had none, Task 7 may have had none).

- [ ] **Step 2: Confirm tree cleanliness**

Run:
```bash
git status --porcelain | grep -v '^??'
```
Expected: empty output. If anything is staged/modified, commit or revert before sign-off.

- [ ] **Step 3: Run the full verification one more time as a final smoke**

Run:
```bash
cargo check --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude prdt-latency-bench
```
Expected: all three commands exit 0. If any fails, fix forward (do **not** force-merge).

- [ ] **Step 4: Hand back to the user with a summary**

Print to the user:
```text
L0 trait extraction complete on branch phase-l0-trait-extraction.
- 8 commits, regression-zero on Windows test/clippy.
- New crates: prdt-media-core, prdt-input-core, prdt-media-linux (skel), prdt-input-linux (skel).
- Windows adapters: prdt-media-win impls Encoder, prdt-input-win impls InputInjector / ClipboardProvider / VirtualDesktopGeometry.
- Host / viewer untouched. No behavior change.

Next: L1 plan (Linux PoC) — needs a Linux dev environment, not started.
```

Ask the user how they want to land the branch (PR vs merge vs hold) before doing anything destructive.

---

## Self-Review Checklist (already applied)

- **Spec coverage:** L0 scope per CCG synthesis was "trait extraction + Windows adapter". Tasks 1–4 cover both halves. Tasks 5–6 prepare L1 ground without overstepping. Task 7 enforces regression-zero. Tasks 8–9 close out documentation and sign-off.
- **Placeholders:** none — every code block is complete and runnable, every command shows the expected outcome.
- **Type consistency:** `EncodedPacket` (media-core) / `EncodedH265Frame` (media-win) / `EncodedFrame` (protocol) are three distinct types — the adapter in Task 3 explicitly translates between the first two; the third is unrelated and unchanged. `MonitorRect` is reused from `prdt_protocol`. `InputEvent` is reused from `prdt_protocol`. No method-name drift across tasks.
- **Risk gates:** Every implementation task ends with a green-test command; the workspace verification (Task 7) is mandatory before sign-off; if anything fails, the plan halts rather than skipping ahead.
