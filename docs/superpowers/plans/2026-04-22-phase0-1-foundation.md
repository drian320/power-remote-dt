# Phase 0 — Plan 1 of 4: Foundation (protocol + transport)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OS 非依存の `protocol` クレート(型とワイヤー形式)と `transport` クレート(UDP + FEC + reassembler)を TDD で構築し、UDP loopback 上で `EncodedFrame` / `InputEvent` / `ControlMessage` が往復できる状態を達成する。GPU 依存コードはこの plan には一切含まない。

**Architecture:** Cargo workspace 構成、6 クレート + 1 ベンチクレート。Plan 1 では OS 非依存の 2 クレート(`protocol` / `transport`)を完成させ、残り 5 クレートは空スケルトンのみ作る。FEC は Reed-Solomon(k=8, m=2)、パケットは固定 16B ヘッダ + type-specific payload、`bincode` で内側ペイロードをシリアライズ。

**Tech Stack:** Rust stable、Cargo workspace、`tokio`(UDP)、`bincode`、`reed-solomon-erasure`、`bytes`、`tracing`、`thiserror`、`proptest`(dev)。Windows 11 開発、`windows-latest` CI。

**Spec reference:** `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`(セクション 1.2、2.2、4.1〜4.7、5 全体、7.2〜7.3 に対応)

---

## File Structure (Plan 1 で作成/変更するファイル)

```
power-remote-dt/
├── .gitignore                                  [新規]
├── Cargo.toml                                  [新規] workspace ルート
├── rustfmt.toml                                [新規]
├── .github/workflows/ci.yml                    [新規] CI(軽量テストのみ)
├── docs/superpowers/plans/
│   └── 2026-04-22-phase0-1-foundation.md       [このファイル]
└── crates/
    ├── protocol/
    │   ├── Cargo.toml                          [新規]
    │   ├── src/
    │   │   ├── lib.rs                          [新規] 公開 API 束ね
    │   │   ├── error.rs                        [新規] ProtocolError
    │   │   ├── frame.rs                        [新規] EncodedFrame
    │   │   ├── input.rs                        [新規] InputEvent, MouseButton
    │   │   ├── control.rs                      [新規] ControlMessage
    │   │   ├── wire.rs                         [新規] WirePacket, PacketHeader, packet_type
    │   │   └── ser.rs                          [新規] encode/decode ヘルパ
    │   └── tests/
    │       └── roundtrip.rs                    [新規] proptest 含む
    ├── transport/
    │   ├── Cargo.toml                          [新規]
    │   ├── src/
    │   │   ├── lib.rs                          [新規]
    │   │   ├── error.rs                        [新規] TransportError
    │   │   ├── transport_trait.rs              [新規] Transport trait
    │   │   ├── fec.rs                          [新規] Reed-Solomon ラッパ
    │   │   ├── assembler.rs                    [新規] FrameAssembler
    │   │   ├── packetize.rs                    [新規] EncodedFrame → Vec<VideoPacket>
    │   │   ├── udp.rs                          [新規] CustomUdpTransport
    │   │   ├── loopback.rs                     [新規] InProcTransport(test 用)
    │   │   └── handshake.rs                    [新規] Hello/HelloAck
    │   └── tests/
    │       └── loopback_test.rs                [新規] 人工損失/遅延注入テスト
    ├── media-win/
    │   ├── Cargo.toml                          [新規、空スケルトン]
    │   └── src/lib.rs                          [新規、空スケルトン]
    ├── input-win/
    │   ├── Cargo.toml                          [新規、空スケルトン]
    │   └── src/lib.rs                          [新規、空スケルトン]
    ├── host/
    │   ├── Cargo.toml                          [新規、空 bin]
    │   └── src/main.rs                         [新規、Hello World]
    ├── viewer/
    │   ├── Cargo.toml                          [新規、空 bin]
    │   └── src/main.rs                         [新規、Hello World]
    └── latency-bench/
        ├── Cargo.toml                          [新規、空 bin]
        └── src/main.rs                         [新規、Hello World]
```

**重要**: 各ファイルは単一責務。`protocol` は **OS 依存ゼロ**を厳守(Windows API / ネット I/O を使わない)。`transport` は tokio(UDP)を使うが OS 固有 API は使わない。

---

## Tasks

### Task 1: ワークスペース初期化と Git

**Files:**
- Create: `E:\project\rust-desktop\power-remote-dt\Cargo.toml`
- Create: `E:\project\rust-desktop\power-remote-dt\.gitignore`
- Create: `E:\project\rust-desktop\power-remote-dt\rustfmt.toml`

- [ ] **Step 1: git 初期化(既に `.git` があればスキップ)**

Run:
```bash
cd "E:/project/rust-desktop/power-remote-dt"
git init
```
Expected: `Initialized empty Git repository in E:/project/rust-desktop/power-remote-dt/.git/`

- [ ] **Step 2: `.gitignore` を作成**

File: `.gitignore`
```
/target
**/*.rs.bk
Cargo.lock
*.pdb
.vscode/
.idea/
*.log
*.jsonl
/tmp/
/.omc/state/
```

- [ ] **Step 3: `rustfmt.toml` を作成**

File: `rustfmt.toml`
```
max_width = 100
edition = "2021"
imports_granularity = "Module"
group_imports = "StdExternalCrate"
```

- [ ] **Step 4: workspace `Cargo.toml` を作成**

File: `Cargo.toml`
```toml
[workspace]
resolver = "2"
members = [
    "crates/protocol",
    "crates/transport",
    "crates/media-win",
    "crates/input-win",
    "crates/host",
    "crates/viewer",
    "crates/latency-bench",
]

[workspace.package]
edition = "2021"
rust-version = "1.78"
license = "Apache-2.0 OR MIT"
repository = "https://github.com/your-org/power-remote-dt"

[workspace.dependencies]
# Async / IO
tokio = { version = "1.40", features = ["rt-multi-thread", "net", "macros", "sync", "time"] }

# Serialization
bincode = "1.3"
bytes = "1.7"
serde = { version = "1.0", features = ["derive"] }

# FEC
reed-solomon-erasure = "6.0"

# Error & logging
thiserror = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# CLI
clap = { version = "4.5", features = ["derive"] }

# Testing
proptest = "1.5"

[profile.release]
lto = "thin"
codegen-units = 1
panic = "abort"

[profile.dev]
opt-level = 1       # テストが 4K60 相当のバッファ処理で時間かかりすぎないように
panic = "abort"
```

- [ ] **Step 5: 動作確認(空ビルド)**

Run:
```bash
cargo check --workspace
```
Expected: 最初は `members` に指定したクレートがまだ存在しないため失敗する想定。後続 Task で空スケルトンを作るまで赤のまま。`error: failed to load manifest for workspace member ...` が出ることを確認して次へ。

- [ ] **Step 6: 初回コミット**

Run:
```bash
git add -A
git commit -m "chore: initialize workspace scaffolding"
```

---

### Task 2: 全クレートの空スケルトンを作る(workspace を緑にする)

**Files:**
- Create: `crates/protocol/Cargo.toml`、`crates/protocol/src/lib.rs`
- Create: `crates/transport/Cargo.toml`、`crates/transport/src/lib.rs`
- Create: `crates/media-win/Cargo.toml`、`crates/media-win/src/lib.rs`
- Create: `crates/input-win/Cargo.toml`、`crates/input-win/src/lib.rs`
- Create: `crates/host/Cargo.toml`、`crates/host/src/main.rs`
- Create: `crates/viewer/Cargo.toml`、`crates/viewer/src/main.rs`
- Create: `crates/latency-bench/Cargo.toml`、`crates/latency-bench/src/main.rs`

- [ ] **Step 1: `protocol` クレートを作る**

File: `crates/protocol/Cargo.toml`
```toml
[package]
name = "prdt-protocol"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
bincode.workspace = true
bytes.workspace = true
serde.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
```

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.
```

- [ ] **Step 2: `transport` クレートを作る**

File: `crates/transport/Cargo.toml`
```toml
[package]
name = "prdt-transport"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
prdt-protocol = { path = "../protocol" }
tokio.workspace = true
bincode.workspace = true
bytes.workspace = true
serde.workspace = true
thiserror.workspace = true
tracing.workspace = true
reed-solomon-erasure.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "net", "macros", "sync", "time", "test-util"] }
proptest.workspace = true
```

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.
```

- [ ] **Step 3: `media-win` の空スケルトン**

File: `crates/media-win/Cargo.toml`
```toml
[package]
name = "prdt-media-win"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-protocol = { path = "../protocol" }
```

File: `crates/media-win/src/lib.rs`
```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented in Phase 0 Plan 2.

#![cfg(windows)]
```

- [ ] **Step 4: `input-win` の空スケルトン**

File: `crates/input-win/Cargo.toml`
```toml
[package]
name = "prdt-input-win"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-protocol = { path = "../protocol" }
```

File: `crates/input-win/src/lib.rs`
```rust
//! Windows input capture (RawInput) and injection (SendInput).
//! Implemented in Phase 0 Plan 3.

#![cfg(windows)]
```

- [ ] **Step 5: `host` / `viewer` / `latency-bench` の空 bin**

File: `crates/host/Cargo.toml`
```toml
[package]
name = "prdt-host"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "prdt-host"
path = "src/main.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-media-win = { path = "../media-win" }
prdt-input-win = { path = "../input-win" }
```

File: `crates/host/src/main.rs`
```rust
fn main() {
    println!("prdt-host placeholder (Phase 0 Plan 3 で実装)");
}
```

File: `crates/viewer/Cargo.toml`
```toml
[package]
name = "prdt-viewer"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "prdt-viewer"
path = "src/main.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-media-win = { path = "../media-win" }
prdt-input-win = { path = "../input-win" }
```

File: `crates/viewer/src/main.rs`
```rust
fn main() {
    println!("prdt-viewer placeholder (Phase 0 Plan 3 で実装)");
}
```

File: `crates/latency-bench/Cargo.toml`
```toml
[package]
name = "prdt-latency-bench"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "prdt-latency-bench"
path = "src/main.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

File: `crates/latency-bench/src/main.rs`
```rust
fn main() {
    println!("prdt-latency-bench placeholder (Phase 0 Plan 4 で本実装)");
}
```

- [ ] **Step 6: ビルド確認**

Run:
```bash
cargo check --workspace
```
Expected: すべてのクレートがビルド通過、警告ゼロ。

- [ ] **Step 7: コミット**

Run:
```bash
git add -A
git commit -m "chore: add empty scaffolding for all 7 crates"
```

---

### Task 3: CI スケルトン(軽量テストのみ、GPU 不要)

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: CI 設定を追加**

File: `.github/workflows/ci.yml`
```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: rustfmt
        run: cargo fmt --all -- --check
      - name: clippy (protocol, transport のみ)
        run: cargo clippy -p prdt-protocol -p prdt-transport --all-targets -- -D warnings
      - name: test (protocol, transport)
        run: cargo test -p prdt-protocol -p prdt-transport --all-targets
      - name: build
        run: cargo build --workspace
```

- [ ] **Step 2: CI 動作確認(ローカル代替チェック)**

Run:
```bash
cargo fmt --all -- --check
cargo clippy -p prdt-protocol -p prdt-transport --all-targets -- -D warnings
cargo test -p prdt-protocol -p prdt-transport --all-targets
```
Expected: すべて成功(クレートはまだ空なのでテスト 0 件、clippy/fmt パス)。

- [ ] **Step 3: コミット**

Run:
```bash
git add -A
git commit -m "ci: add minimal Windows CI pipeline (fmt, clippy, test, build)"
```

---

### Task 4: `protocol::error::ProtocolError` を定義

**Files:**
- Create: `crates/protocol/src/error.rs`
- Modify: `crates/protocol/src/lib.rs`

- [ ] **Step 1: テストを書く**

File: `crates/protocol/src/error.rs`
```rust
use std::fmt;

/// Protocol-level error surface. Intentionally small - the caller can
/// downcast `Other` for edge cases.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("packet too short: need >= {expected}, got {actual}")]
    PacketTooShort { expected: usize, actual: usize },

    #[error("bad magic: expected 0x{expected:02x}, got 0x{actual:02x}")]
    BadMagic { expected: u8, actual: u8 },

    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),

    #[error("unknown packet type: {0}")]
    UnknownPacketType(u8),

    #[error("unknown control kind: {0}")]
    UnknownControlKind(u8),

    #[error("unknown event kind: {0}")]
    UnknownEventKind(u8),

    #[error("payload length mismatch: header={header}, actual={actual}")]
    PayloadLengthMismatch { header: u32, actual: usize },

    #[error("bincode: {0}")]
    Bincode(#[from] bincode::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_stable() {
        let e = ProtocolError::BadMagic { expected: 0x52, actual: 0xAA };
        assert_eq!(e.to_string(), "bad magic: expected 0x52, got 0xaa");

        let e = ProtocolError::PacketTooShort { expected: 16, actual: 3 };
        assert_eq!(e.to_string(), "packet too short: need >= 16, got 3");

        let _: ProtocolError = bincode::Error::from(
            Box::new(bincode::ErrorKind::SizeLimit)
        ).into();
    }

    #[test]
    fn error_impls_std_error() {
        fn assert_is_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_is_error::<ProtocolError>();
        let _ = fmt::Debug::fmt;
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod error;

pub use error::ProtocolError;
```

- [ ] **Step 3: テスト実行して通ることを確認**

Run:
```bash
cargo test -p prdt-protocol error::tests
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add ProtocolError enum"
```

---

### Task 5: `EncodedFrame` と `FrameFlags` の型定義

**Files:**
- Create: `crates/protocol/src/frame.rs`
- Modify: `crates/protocol/src/lib.rs`

**Spec ref:** セクション 2.2、4.1、5.3。

- [ ] **Step 1: 失敗するテストを書く**

File: `crates/protocol/src/frame.rs`
```rust
use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Codec discriminator. For Phase 0 we only support H.265, but keep it
/// open so Phase 3+ can slot in AV1 without a protocol-breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Codec {
    H265 = 0,
    H264 = 1,
    Av1  = 2,
}

impl Codec {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::H265),
            1 => Some(Self::H264),
            2 => Some(Self::Av1),
            _ => None,
        }
    }
}

/// A single encoded video frame - one or more NAL units concatenated.
/// Zero-copy: `nal_units` is `Bytes` so the producer can retain ownership
/// of an underlying encoder buffer if it wants.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub seq: u64,
    pub timestamp_host_us: u64,
    pub is_keyframe: bool,
    pub nal_units: Bytes,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
}

impl EncodedFrame {
    pub fn new_h265(
        seq: u64,
        timestamp_host_us: u64,
        is_keyframe: bool,
        nal_units: Bytes,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            seq,
            timestamp_host_us,
            is_keyframe,
            nal_units,
            width,
            height,
            codec: Codec::H265,
        }
    }

    pub fn byte_len(&self) -> usize {
        self.nal_units.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trip() {
        for v in 0u8..=2 {
            let c = Codec::from_u8(v).unwrap();
            assert_eq!(c as u8, v);
        }
        assert!(Codec::from_u8(42).is_none());
    }

    #[test]
    fn encoded_frame_construction() {
        let f = EncodedFrame::new_h265(
            1,
            12345,
            true,
            Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x40, 0x01]),
            3840,
            2160,
        );
        assert_eq!(f.seq, 1);
        assert_eq!(f.timestamp_host_us, 12345);
        assert!(f.is_keyframe);
        assert_eq!(f.width, 3840);
        assert_eq!(f.height, 2160);
        assert_eq!(f.codec, Codec::H265);
        assert_eq!(f.byte_len(), 6);
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod error;
pub mod frame;

pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-protocol frame::tests
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add EncodedFrame and Codec types"
```

---

### Task 6: `InputEvent` と `MouseButton`

**Files:**
- Create: `crates/protocol/src/input.rs`
- Modify: `crates/protocol/src/lib.rs`

**Spec ref:** セクション 2.2、4.2、5.4。

- [ ] **Step 1: 型定義とテスト**

File: `crates/protocol/src/input.rs`
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MouseButton {
    Left   = 0,
    Right  = 1,
    Middle = 2,
    X1     = 3,
    X2     = 4,
}

impl MouseButton {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            3 => Some(Self::X1),
            4 => Some(Self::X2),
            _ => None,
        }
    }
}

/// Input event sent from the viewer to the host.
///
/// - Mouse coordinates: `absolute=true` means host-screen-space pixels;
///   `absolute=false` means a delta from the previous position.
/// - Scancode: host-OS-native scancode (we do NOT translate virtual keys
///   between viewer and host - passthrough avoids layout mismatches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove { x: i32, y: i32, absolute: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    Key { scancode: u32, pressed: bool },
}

/// Discriminant byte used in the wire format (InputPacket.event_kind).
impl InputEvent {
    pub fn kind_u8(&self) -> u8 {
        match self {
            Self::MouseMove { .. }   => 0,
            Self::MouseButton { .. } => 1,
            Self::MouseWheel { .. }  => 2,
            Self::Key { .. }         => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_button_round_trip() {
        for v in 0u8..=4 {
            let b = MouseButton::from_u8(v).unwrap();
            assert_eq!(b as u8, v);
        }
        assert!(MouseButton::from_u8(99).is_none());
    }

    #[test]
    fn event_kinds_are_stable() {
        assert_eq!(InputEvent::MouseMove { x: 0, y: 0, absolute: true }.kind_u8(), 0);
        assert_eq!(
            InputEvent::MouseButton { button: MouseButton::Left, pressed: true }.kind_u8(),
            1,
        );
        assert_eq!(InputEvent::MouseWheel { dx: 0, dy: 1 }.kind_u8(), 2);
        assert_eq!(InputEvent::Key { scancode: 0x1E, pressed: true }.kind_u8(), 3);
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod error;
pub mod frame;
pub mod input;

pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
pub use input::{InputEvent, MouseButton};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-protocol input::tests
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add InputEvent and MouseButton types"
```

---

### Task 7: `ControlMessage`

**Files:**
- Create: `crates/protocol/src/control.rs`
- Modify: `crates/protocol/src/lib.rs`

**Spec ref:** 4.4、4.5、4.6、5.5。

- [ ] **Step 1: 型定義とテスト**

File: `crates/protocol/src/control.rs`
```rust
use crate::frame::Codec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Viewer → Host.
    Hello {
        protocol_version: u8,
        req_width: u32,
        req_height: u32,
        req_fps: u32,
        codec: Codec,
    },
    /// Host → Viewer.
    HelloAck {
        session_id: u64,
        host_monotonic_base_us: u64,
        neg_width: u32,
        neg_height: u32,
        neg_fps: u32,
        neg_bitrate_bps: u32,
    },
    /// Bidirectional.
    Bye,
    /// Viewer → Host.
    Ping { ping_seq: u64, viewer_ts_us: u64 },
    /// Host → Viewer.
    Pong { ping_seq: u64, viewer_ts_us: u64, host_ts_us: u64 },
    /// Viewer → Host.
    RequestIdr,
    /// Bidirectional (viewer suggests, host confirms).
    SetBitrate { target_bps: u32 },
    /// Bidirectional debug channel; optional, Phase 0 not required.
    Stats {
        loss_rate_ppm: u32,     // parts per million
        fps_millis: u32,        // fps * 1000
        bitrate_bps: u32,
    },
}

impl ControlMessage {
    /// Discriminant byte used in wire format (ControlPacket.control_kind).
    pub fn kind_u8(&self) -> u8 {
        match self {
            Self::Hello        { .. } => 0,
            Self::HelloAck     { .. } => 1,
            Self::Bye                 => 2,
            Self::Ping         { .. } => 3,
            Self::Pong         { .. } => 4,
            Self::RequestIdr          => 5,
            Self::SetBitrate   { .. } => 6,
            Self::Stats        { .. } => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_kinds_are_stable() {
        let hello = ControlMessage::Hello {
            protocol_version: 1,
            req_width: 3840,
            req_height: 2160,
            req_fps: 60,
            codec: Codec::H265,
        };
        assert_eq!(hello.kind_u8(), 0);
        assert_eq!(ControlMessage::Bye.kind_u8(), 2);
        assert_eq!(ControlMessage::RequestIdr.kind_u8(), 5);
    }

    #[test]
    fn ping_pong_fields() {
        let p = ControlMessage::Ping { ping_seq: 7, viewer_ts_us: 1_000_000 };
        assert_eq!(p.kind_u8(), 3);
        if let ControlMessage::Ping { ping_seq, viewer_ts_us } = p {
            assert_eq!(ping_seq, 7);
            assert_eq!(viewer_ts_us, 1_000_000);
        }
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod control;
pub mod error;
pub mod frame;
pub mod input;

pub use control::ControlMessage;
pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
pub use input::{InputEvent, MouseButton};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-protocol control::tests
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add ControlMessage enum"
```

---

### Task 8: Common packet header (16B) のシリアライズ

**Files:**
- Create: `crates/protocol/src/wire.rs`
- Modify: `crates/protocol/src/lib.rs`

**Spec ref:** 5.2。

- [ ] **Step 1: ヘッダ定数と型定義**

File: `crates/protocol/src/wire.rs`
```rust
use crate::error::ProtocolError;

/// Magic byte identifying our protocol.
pub const MAGIC: u8 = 0x52; // 'R'

/// Current protocol version. Incremented on any breaking wire change.
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Length of the common header in bytes.
pub const HEADER_LEN: usize = 16;

/// Upper bound on a single chunk payload we send over UDP.
/// Derived: IPv4 MTU 1500 - IP 20 - UDP 8 - base header 16 - video header 26 = 1430.
/// We round down to 1200 for safety over tunneled paths.
pub const DEFAULT_CHUNK_PAYLOAD_LEN: usize = 1200;

/// Wire-level packet type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Video   = 0,
    Input   = 1,
    Control = 2,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Video),
            1 => Some(Self::Input),
            2 => Some(Self::Control),
            _ => None,
        }
    }
}

/// Fixed 16-byte header prefixed onto every UDP packet.
///
/// Layout (little-endian):
/// ```text
/// offset | size | field
/// 0      | 1    | magic (0x52)
/// 1      | 1    | version (0x01)
/// 2      | 1    | packet_type
/// 3      | 1    | flags
/// 4      | 8    | session_id (u64 LE)
/// 12     | 4    | payload_len (u32 LE)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub packet_type: PacketType,
    pub flags: u8,
    pub session_id: u64,
    pub payload_len: u32,
}

impl PacketHeader {
    /// Serialize the header into a 16-byte array.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = MAGIC;
        out[1] = PROTOCOL_VERSION;
        out[2] = self.packet_type as u8;
        out[3] = self.flags;
        out[4..12].copy_from_slice(&self.session_id.to_le_bytes());
        out[12..16].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }

    /// Parse the header from a raw UDP datagram. Validates magic and version.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: HEADER_LEN,
                actual: buf.len(),
            });
        }
        if buf[0] != MAGIC {
            return Err(ProtocolError::BadMagic { expected: MAGIC, actual: buf[0] });
        }
        if buf[1] != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(buf[1]));
        }
        let packet_type = PacketType::from_u8(buf[2])
            .ok_or(ProtocolError::UnknownPacketType(buf[2]))?;
        let flags = buf[3];
        let mut sid = [0u8; 8];
        sid.copy_from_slice(&buf[4..12]);
        let session_id = u64::from_le_bytes(sid);
        let mut plen = [0u8; 4];
        plen.copy_from_slice(&buf[12..16]);
        let payload_len = u32::from_le_bytes(plen);
        Ok(Self { packet_type, flags, session_id, payload_len })
    }
}
```

- [ ] **Step 2: テストを追加(同ファイル末尾)**

追記 to `crates/protocol/src/wire.rs`:
```rust

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip_video() {
        let h = PacketHeader {
            packet_type: PacketType::Video,
            flags: 0b0000_0001,
            session_id: 0xDEADBEEF_CAFEBABE,
            payload_len: 1200,
        };
        let buf = h.encode();
        assert_eq!(buf[0], MAGIC);
        assert_eq!(buf[1], PROTOCOL_VERSION);
        let parsed = PacketHeader::decode(&buf).expect("decode ok");
        assert_eq!(parsed, h);
    }

    #[test]
    fn header_round_trip_all_types() {
        for t in [PacketType::Video, PacketType::Input, PacketType::Control] {
            let h = PacketHeader {
                packet_type: t,
                flags: 0,
                session_id: 1,
                payload_len: 10,
            };
            let buf = h.encode();
            assert_eq!(PacketHeader::decode(&buf).unwrap(), h);
        }
    }

    #[test]
    fn header_rejects_short_buffer() {
        let buf = [MAGIC, PROTOCOL_VERSION, 0];
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(
            err,
            ProtocolError::PacketTooShort { expected: 16, actual: 3 }
        ));
    }

    #[test]
    fn header_rejects_bad_magic() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = 0xAA;
        buf[1] = PROTOCOL_VERSION;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtocolError::BadMagic { expected: 0x52, actual: 0xAA }));
    }

    #[test]
    fn header_rejects_unsupported_version() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = MAGIC;
        buf[1] = 0xFF;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtocolError::UnsupportedVersion(0xFF)));
    }

    #[test]
    fn header_rejects_unknown_packet_type() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = MAGIC;
        buf[1] = PROTOCOL_VERSION;
        buf[2] = 0xAB;
        let err = PacketHeader::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownPacketType(0xAB)));
    }
}
```

- [ ] **Step 3: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
//! Wire types and serialization for power-remote-dt.
//! OS-independent. No network or OS calls here.

pub mod control;
pub mod error;
pub mod frame;
pub mod input;
pub mod wire;

pub use control::ControlMessage;
pub use error::ProtocolError;
pub use frame::{Codec, EncodedFrame};
pub use input::{InputEvent, MouseButton};
pub use wire::{PacketHeader, PacketType, DEFAULT_CHUNK_PAYLOAD_LEN, HEADER_LEN, MAGIC, PROTOCOL_VERSION};
```

- [ ] **Step 4: テスト実行**

Run:
```bash
cargo test -p prdt-protocol wire::tests
```
Expected: 6 tests passed

- [ ] **Step 5: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add PacketHeader with encode/decode (16B fixed)"
```

---

### Task 9: VideoPacket ペイロードの encode/decode

**Files:**
- Modify: `crates/protocol/src/wire.rs`

**Spec ref:** 5.3。

- [ ] **Step 1: VideoPacket 型と payload ヘッダ(26B)**

Append to `crates/protocol/src/wire.rs`:
```rust

/// VideoPacket payload header length (before chunk data).
pub const VIDEO_PAYLOAD_HDR_LEN: usize = 26;

/// Flags packed into VideoPacket.video_flags byte.
pub mod video_flags {
    pub const IS_KEYFRAME: u8 = 0b0000_0001;
    pub const IS_PARITY:   u8 = 0b0000_0010;
}

/// A single video chunk on the wire. For a given frame_seq, the receiver
/// collects all chunks with `chunk_idx in [0, source_chunks + parity_chunks)`
/// and reconstructs the frame via FEC if necessary.
///
/// Payload layout (little-endian, starts at byte 16 of the UDP datagram):
/// ```text
/// offset | size | field
/// 0      | 8    | frame_seq
/// 8      | 8    | timestamp_host_us
/// 16     | 2    | chunk_idx
/// 18     | 2    | source_chunks (k)
/// 20     | 2    | parity_chunks (m)
/// 22     | 1    | video_flags
/// 23     | 1    | reserved
/// 24     | 2    | payload_bytes (valid bytes inside this chunk)
/// 26     | N    | chunk_payload (up to DEFAULT_CHUNK_PAYLOAD_LEN)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket {
    pub frame_seq: u64,
    pub timestamp_host_us: u64,
    pub chunk_idx: u16,
    pub source_chunks: u16,
    pub parity_chunks: u16,
    pub video_flags: u8,
    pub payload_bytes: u16,
    pub chunk_payload: Vec<u8>,
}

impl VideoPacket {
    pub fn is_keyframe(&self) -> bool {
        self.video_flags & video_flags::IS_KEYFRAME != 0
    }

    pub fn is_parity(&self) -> bool {
        self.video_flags & video_flags::IS_PARITY != 0
    }

    /// Serialize into a buffer (caller must prepend PacketHeader separately).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(VIDEO_PAYLOAD_HDR_LEN + self.chunk_payload.len());
        out.extend_from_slice(&self.frame_seq.to_le_bytes());
        out.extend_from_slice(&self.timestamp_host_us.to_le_bytes());
        out.extend_from_slice(&self.chunk_idx.to_le_bytes());
        out.extend_from_slice(&self.source_chunks.to_le_bytes());
        out.extend_from_slice(&self.parity_chunks.to_le_bytes());
        out.push(self.video_flags);
        out.push(0); // reserved
        out.extend_from_slice(&self.payload_bytes.to_le_bytes());
        out.extend_from_slice(&self.chunk_payload);
        out
    }

    /// Parse from a payload slice (body-only, not including common 16B header).
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < VIDEO_PAYLOAD_HDR_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: VIDEO_PAYLOAD_HDR_LEN,
                actual: buf.len(),
            });
        }
        let frame_seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let timestamp_host_us = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let chunk_idx = u16::from_le_bytes(buf[16..18].try_into().unwrap());
        let source_chunks = u16::from_le_bytes(buf[18..20].try_into().unwrap());
        let parity_chunks = u16::from_le_bytes(buf[20..22].try_into().unwrap());
        let video_flags = buf[22];
        let _reserved = buf[23];
        let payload_bytes = u16::from_le_bytes(buf[24..26].try_into().unwrap());

        let expected_payload_end = VIDEO_PAYLOAD_HDR_LEN + payload_bytes as usize;
        if buf.len() < expected_payload_end {
            return Err(ProtocolError::PayloadLengthMismatch {
                header: payload_bytes as u32,
                actual: buf.len() - VIDEO_PAYLOAD_HDR_LEN,
            });
        }
        let chunk_payload = buf[VIDEO_PAYLOAD_HDR_LEN..expected_payload_end].to_vec();
        Ok(Self {
            frame_seq,
            timestamp_host_us,
            chunk_idx,
            source_chunks,
            parity_chunks,
            video_flags,
            payload_bytes,
            chunk_payload,
        })
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;

    #[test]
    fn video_packet_round_trip() {
        let pkt = VideoPacket {
            frame_seq: 42,
            timestamp_host_us: 1_234_567,
            chunk_idx: 3,
            source_chunks: 8,
            parity_chunks: 2,
            video_flags: video_flags::IS_KEYFRAME,
            payload_bytes: 5,
            chunk_payload: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        };
        let buf = pkt.encode();
        assert_eq!(buf.len(), VIDEO_PAYLOAD_HDR_LEN + 5);
        let back = VideoPacket::decode(&buf).unwrap();
        assert_eq!(back, pkt);
        assert!(back.is_keyframe());
        assert!(!back.is_parity());
    }

    #[test]
    fn video_packet_parity_flag() {
        let pkt = VideoPacket {
            frame_seq: 1,
            timestamp_host_us: 0,
            chunk_idx: 9,
            source_chunks: 8,
            parity_chunks: 2,
            video_flags: video_flags::IS_PARITY,
            payload_bytes: 0,
            chunk_payload: vec![],
        };
        let buf = pkt.encode();
        let back = VideoPacket::decode(&buf).unwrap();
        assert!(back.is_parity());
        assert!(!back.is_keyframe());
    }

    #[test]
    fn video_packet_rejects_short() {
        let buf = [0u8; 4];
        assert!(VideoPacket::decode(&buf).is_err());
    }

    #[test]
    fn video_packet_rejects_length_mismatch() {
        // Header says payload_bytes = 99 but only 3 bytes of payload present.
        let mut buf = vec![0u8; VIDEO_PAYLOAD_HDR_LEN];
        buf[24..26].copy_from_slice(&99u16.to_le_bytes());
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert!(matches!(
            VideoPacket::decode(&buf).unwrap_err(),
            ProtocolError::PayloadLengthMismatch { header: 99, actual: 3 }
        ));
    }
}
```

- [ ] **Step 2: テスト実行**

Run:
```bash
cargo test -p prdt-protocol wire::video_tests
```
Expected: 4 tests passed

- [ ] **Step 3: 公開を `lib.rs` に追加**

File: `crates/protocol/src/lib.rs`(`pub use wire::...` 行を更新)
```rust
pub use wire::{
    video_flags, PacketHeader, PacketType, VideoPacket,
    DEFAULT_CHUNK_PAYLOAD_LEN, HEADER_LEN, MAGIC, PROTOCOL_VERSION,
    VIDEO_PAYLOAD_HDR_LEN,
};
```

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add VideoPacket encode/decode"
```

---

### Task 10: InputPacket ペイロードの encode/decode

**Files:**
- Modify: `crates/protocol/src/wire.rs`

**Spec ref:** 5.4。

- [ ] **Step 1: InputPacket 型**

Append to `crates/protocol/src/wire.rs`:
```rust

use crate::input::{InputEvent, MouseButton};

/// InputPacket fixed-prefix length (before event-specific body).
pub const INPUT_PAYLOAD_HDR_LEN: usize = 17;

/// Wire representation of a single input event.
///
/// Layout (little-endian, after 16B common header):
/// ```text
/// offset | size | field
/// 0      | 8    | input_seq
/// 8      | 8    | timestamp_viewer_us
/// 16     | 1    | event_kind
/// 17     | N    | event_body  (kind-specific)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPacket {
    pub input_seq: u64,
    pub timestamp_viewer_us: u64,
    pub event: InputEvent,
}

impl InputPacket {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(INPUT_PAYLOAD_HDR_LEN + 9);
        out.extend_from_slice(&self.input_seq.to_le_bytes());
        out.extend_from_slice(&self.timestamp_viewer_us.to_le_bytes());
        out.push(self.event.kind_u8());
        match self.event {
            InputEvent::MouseMove { x, y, absolute } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.push(absolute as u8);
            }
            InputEvent::MouseButton { button, pressed } => {
                out.push(button as u8);
                out.push(pressed as u8);
            }
            InputEvent::MouseWheel { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            InputEvent::Key { scancode, pressed } => {
                out.extend_from_slice(&scancode.to_le_bytes());
                out.push(pressed as u8);
            }
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < INPUT_PAYLOAD_HDR_LEN {
            return Err(ProtocolError::PacketTooShort {
                expected: INPUT_PAYLOAD_HDR_LEN,
                actual: buf.len(),
            });
        }
        let input_seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let timestamp_viewer_us = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let event_kind = buf[16];
        let body = &buf[17..];
        let event = match event_kind {
            0 => {
                if body.len() < 9 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 9,
                        actual: buf.len(),
                    });
                }
                InputEvent::MouseMove {
                    x: i32::from_le_bytes(body[0..4].try_into().unwrap()),
                    y: i32::from_le_bytes(body[4..8].try_into().unwrap()),
                    absolute: body[8] != 0,
                }
            }
            1 => {
                if body.len() < 2 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 2,
                        actual: buf.len(),
                    });
                }
                let button = MouseButton::from_u8(body[0])
                    .ok_or(ProtocolError::UnknownEventKind(body[0]))?;
                InputEvent::MouseButton { button, pressed: body[1] != 0 }
            }
            2 => {
                if body.len() < 8 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 8,
                        actual: buf.len(),
                    });
                }
                InputEvent::MouseWheel {
                    dx: i32::from_le_bytes(body[0..4].try_into().unwrap()),
                    dy: i32::from_le_bytes(body[4..8].try_into().unwrap()),
                }
            }
            3 => {
                if body.len() < 5 {
                    return Err(ProtocolError::PacketTooShort {
                        expected: INPUT_PAYLOAD_HDR_LEN + 5,
                        actual: buf.len(),
                    });
                }
                InputEvent::Key {
                    scancode: u32::from_le_bytes(body[0..4].try_into().unwrap()),
                    pressed: body[4] != 0,
                }
            }
            other => return Err(ProtocolError::UnknownEventKind(other)),
        };
        Ok(Self { input_seq, timestamp_viewer_us, event })
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn input_packet_all_kinds_round_trip() {
        let cases = [
            InputEvent::MouseMove { x: 100, y: -50, absolute: true },
            InputEvent::MouseMove { x: -1, y: 1, absolute: false },
            InputEvent::MouseButton { button: MouseButton::Left, pressed: true },
            InputEvent::MouseButton { button: MouseButton::X2, pressed: false },
            InputEvent::MouseWheel { dx: 0, dy: 120 },
            InputEvent::Key { scancode: 0x1E, pressed: true },
            InputEvent::Key { scancode: 0xE0_5D, pressed: false },
        ];
        for (i, ev) in cases.iter().enumerate() {
            let p = InputPacket {
                input_seq: i as u64,
                timestamp_viewer_us: 100 + i as u64,
                event: *ev,
            };
            let buf = p.encode();
            let back = InputPacket::decode(&buf).unwrap();
            assert_eq!(back, p, "round trip failed for {:?}", ev);
        }
    }

    #[test]
    fn input_packet_rejects_unknown_kind() {
        let mut buf = vec![0u8; INPUT_PAYLOAD_HDR_LEN + 4];
        buf[16] = 0x42;
        assert!(matches!(
            InputPacket::decode(&buf).unwrap_err(),
            ProtocolError::UnknownEventKind(0x42),
        ));
    }
}
```

- [ ] **Step 2: テスト実行**

Run:
```bash
cargo test -p prdt-protocol wire::input_tests
```
Expected: 2 tests passed

- [ ] **Step 3: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`(`pub use wire::...` 行を更新)
```rust
pub use wire::{
    video_flags, InputPacket, PacketHeader, PacketType, VideoPacket,
    DEFAULT_CHUNK_PAYLOAD_LEN, HEADER_LEN, INPUT_PAYLOAD_HDR_LEN, MAGIC,
    PROTOCOL_VERSION, VIDEO_PAYLOAD_HDR_LEN,
};
```

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add InputPacket encode/decode"
```

---

### Task 11: ControlPacket (bincode) の encode/decode

**Files:**
- Modify: `crates/protocol/src/wire.rs`

**Spec ref:** 5.5、5.6。

- [ ] **Step 1: ControlPacket ヘルパ(1B kind + bincode body)**

Append to `crates/protocol/src/wire.rs`:
```rust

use crate::control::ControlMessage;

/// Serialize a ControlMessage as: [1B kind][bincode body].
pub fn encode_control(msg: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    let kind = msg.kind_u8();
    let mut out = Vec::with_capacity(32);
    out.push(kind);
    bincode::serialize_into(&mut out, msg)?;
    Ok(out)
}

/// Deserialize a ControlMessage from the same layout.
pub fn decode_control(buf: &[u8]) -> Result<ControlMessage, ProtocolError> {
    if buf.is_empty() {
        return Err(ProtocolError::PacketTooShort { expected: 1, actual: 0 });
    }
    let kind = buf[0];
    // We don't trust `kind` blindly; bincode will decode the whole tagged enum.
    // We keep the leading byte as a fast-path dispatch hint for future optimization.
    if kind > 7 {
        return Err(ProtocolError::UnknownControlKind(kind));
    }
    let msg: ControlMessage = bincode::deserialize(&buf[1..])?;
    Ok(msg)
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use crate::frame::Codec;

    #[test]
    fn control_hello_round_trip() {
        let msg = ControlMessage::Hello {
            protocol_version: 1,
            req_width: 3840,
            req_height: 2160,
            req_fps: 60,
            codec: Codec::H265,
        };
        let buf = encode_control(&msg).unwrap();
        assert_eq!(buf[0], msg.kind_u8());
        let back = decode_control(&buf).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn control_all_kinds_round_trip() {
        let cases = [
            ControlMessage::Bye,
            ControlMessage::RequestIdr,
            ControlMessage::Ping { ping_seq: 1, viewer_ts_us: 2 },
            ControlMessage::Pong { ping_seq: 1, viewer_ts_us: 2, host_ts_us: 3 },
            ControlMessage::SetBitrate { target_bps: 50_000_000 },
            ControlMessage::Stats { loss_rate_ppm: 500, fps_millis: 59_940, bitrate_bps: 50_000_000 },
        ];
        for msg in cases {
            let buf = encode_control(&msg).unwrap();
            let back = decode_control(&buf).unwrap();
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn control_rejects_unknown_kind() {
        let buf = vec![0xFF];
        assert!(matches!(
            decode_control(&buf).unwrap_err(),
            ProtocolError::UnknownControlKind(0xFF),
        ));
    }
}
```

- [ ] **Step 2: テスト実行**

Run:
```bash
cargo test -p prdt-protocol wire::control_tests
```
Expected: 3 tests passed

- [ ] **Step 3: `lib.rs` から公開**

File: `crates/protocol/src/lib.rs`
```rust
pub use wire::{
    decode_control, encode_control, video_flags, InputPacket, PacketHeader, PacketType, VideoPacket,
    DEFAULT_CHUNK_PAYLOAD_LEN, HEADER_LEN, INPUT_PAYLOAD_HDR_LEN, MAGIC, PROTOCOL_VERSION,
    VIDEO_PAYLOAD_HDR_LEN,
};
```

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(protocol): add ControlMessage serialization (1B kind + bincode)"
```

---

### Task 12: integration 統合テスト(UDP パケットの完全 round-trip)

**Files:**
- Create: `crates/protocol/tests/roundtrip.rs`

- [ ] **Step 1: end-to-end UDP datagram round-trip テスト**

File: `crates/protocol/tests/roundtrip.rs`
```rust
//! End-to-end tests treating the protocol as an opaque byte-stream.

use prdt_protocol::{
    control::ControlMessage, decode_control, encode_control, frame::Codec, input::MouseButton,
    wire::{self, video_flags, InputPacket, PacketHeader, PacketType, VideoPacket, HEADER_LEN},
    InputEvent, ProtocolError,
};

fn build_video_datagram(session_id: u64, pkt: &VideoPacket) -> Vec<u8> {
    let body = pkt.encode();
    let hdr = PacketHeader {
        packet_type: PacketType::Video,
        flags: 0,
        session_id,
        payload_len: body.len() as u32,
    };
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&hdr.encode());
    out.extend_from_slice(&body);
    out
}

#[test]
fn full_video_datagram_round_trip() {
    let chunk = VideoPacket {
        frame_seq: 100,
        timestamp_host_us: 9_999_999,
        chunk_idx: 0,
        source_chunks: 8,
        parity_chunks: 2,
        video_flags: video_flags::IS_KEYFRAME,
        payload_bytes: 4,
        chunk_payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let datagram = build_video_datagram(0xDEAD_BEEF_CAFE_BABE, &chunk);
    assert_eq!(datagram.len(), HEADER_LEN + wire::VIDEO_PAYLOAD_HDR_LEN + 4);

    let hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(hdr.packet_type, PacketType::Video);
    assert_eq!(hdr.session_id, 0xDEAD_BEEF_CAFE_BABE);
    assert_eq!(hdr.payload_len as usize, wire::VIDEO_PAYLOAD_HDR_LEN + 4);

    let body = &datagram[HEADER_LEN..HEADER_LEN + hdr.payload_len as usize];
    let back = VideoPacket::decode(body).unwrap();
    assert_eq!(back, chunk);
}

#[test]
fn full_input_datagram_round_trip() {
    let pkt = InputPacket {
        input_seq: 7,
        timestamp_viewer_us: 1_000,
        event: InputEvent::MouseButton { button: MouseButton::Right, pressed: true },
    };
    let body = pkt.encode();
    let hdr = PacketHeader {
        packet_type: PacketType::Input,
        flags: 0,
        session_id: 42,
        payload_len: body.len() as u32,
    };
    let mut datagram = Vec::from(hdr.encode());
    datagram.extend_from_slice(&body);

    let parsed_hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(parsed_hdr.packet_type, PacketType::Input);
    let back = InputPacket::decode(
        &datagram[HEADER_LEN..HEADER_LEN + parsed_hdr.payload_len as usize],
    )
    .unwrap();
    assert_eq!(back, pkt);
}

#[test]
fn full_control_datagram_round_trip() {
    let msg = ControlMessage::HelloAck {
        session_id: 1,
        host_monotonic_base_us: 2,
        neg_width: 3840,
        neg_height: 2160,
        neg_fps: 60,
        neg_bitrate_bps: 50_000_000,
    };
    let body = encode_control(&msg).unwrap();
    let hdr = PacketHeader {
        packet_type: PacketType::Control,
        flags: 0,
        session_id: 1,
        payload_len: body.len() as u32,
    };
    let mut datagram = Vec::from(hdr.encode());
    datagram.extend_from_slice(&body);

    let parsed_hdr = PacketHeader::decode(&datagram).unwrap();
    assert_eq!(parsed_hdr.packet_type, PacketType::Control);
    let back = decode_control(
        &datagram[HEADER_LEN..HEADER_LEN + parsed_hdr.payload_len as usize],
    )
    .unwrap();
    assert_eq!(back, msg);
}

#[test]
fn datagram_rejects_corruption() {
    let mut buf = Vec::from(
        PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: 1,
            payload_len: 0,
        }
        .encode(),
    );
    buf[0] = 0xFF;
    let err = PacketHeader::decode(&buf).unwrap_err();
    assert!(matches!(err, ProtocolError::BadMagic { .. }));
}
```

- [ ] **Step 2: テスト実行**

Run:
```bash
cargo test -p prdt-protocol --test roundtrip
```
Expected: 4 tests passed

- [ ] **Step 3: proptest ベースの encoded-frame 的網羅(オプション、次 Step で追加)**

Append to `crates/protocol/tests/roundtrip.rs`:
```rust

use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_video_packet_round_trip(
        frame_seq in 0u64..u64::MAX,
        ts in 0u64..u64::MAX,
        chunk_idx in 0u16..1024,
        source_chunks in 1u16..32,
        parity_chunks in 0u16..8,
        is_kf in any::<bool>(),
        payload in prop::collection::vec(any::<u8>(), 0..=1200),
    ) {
        let flags = if is_kf { video_flags::IS_KEYFRAME } else { 0 };
        let pkt = VideoPacket {
            frame_seq,
            timestamp_host_us: ts,
            chunk_idx,
            source_chunks,
            parity_chunks,
            video_flags: flags,
            payload_bytes: payload.len() as u16,
            chunk_payload: payload.clone(),
        };
        let buf = pkt.encode();
        let back = VideoPacket::decode(&buf).unwrap();
        prop_assert_eq!(back.frame_seq, frame_seq);
        prop_assert_eq!(back.timestamp_host_us, ts);
        prop_assert_eq!(back.chunk_idx, chunk_idx);
        prop_assert_eq!(back.source_chunks, source_chunks);
        prop_assert_eq!(back.parity_chunks, parity_chunks);
        prop_assert_eq!(back.video_flags, flags);
        prop_assert_eq!(back.chunk_payload, payload);
    }
}
```

- [ ] **Step 4: テスト再実行**

Run:
```bash
cargo test -p prdt-protocol --test roundtrip
```
Expected: 5 tests passed(proptest は 256 件の自動生成を含む)

- [ ] **Step 5: コミット**

Run:
```bash
git add -A
git commit -m "test(protocol): end-to-end datagram round-trip + proptest for VideoPacket"
```

---

### Task 13: `transport::error::TransportError` と `Transport` trait

**Files:**
- Create: `crates/transport/src/error.rs`
- Create: `crates/transport/src/transport_trait.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 2.3、5.13。

- [ ] **Step 1: Error 型**

File: `crates/transport/src/error.rs`
```rust
use prdt_protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),

    #[error("session_id mismatch: expected {expected}, got {actual}")]
    SessionIdMismatch { expected: u64, actual: u64 },

    #[error("handshake timeout")]
    HandshakeTimeout,

    #[error("peer sent Bye")]
    PeerClosed,

    #[error("frame assembler timed out for seq {frame_seq}")]
    FrameTimeout { frame_seq: u64 },

    #[error("FEC recovery failed for seq {frame_seq}: have {have}, need {need}")]
    FecFailed { frame_seq: u64, have: usize, need: usize },

    #[error("encoded frame too large: {bytes} bytes, max {max_bytes}")]
    FrameTooLarge { bytes: usize, max_bytes: usize },

    #[error("fec configuration error: {0}")]
    FecConfig(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = TransportError::SessionIdMismatch { expected: 1, actual: 2 };
        assert_eq!(e.to_string(), "session_id mismatch: expected 1, got 2");
    }
}
```

- [ ] **Step 2: Transport trait**

File: `crates/transport/src/transport_trait.rs`
```rust
use crate::error::TransportError;
use prdt_protocol::{control::ControlMessage, input::InputEvent, EncodedFrame};

/// A message delivered to `Transport::recv()`. Video frames are returned
/// only after all chunks have been reassembled (or reconstructed via FEC).
#[derive(Debug, Clone)]
pub enum ReceivedMessage {
    Video(EncodedFrame),
    Input(InputEvent),
    Control(ControlMessage),
}

/// Transport trait: async UDP-ish bidirectional channel.
///
/// Implementations: `CustomUdpTransport` (real UDP) and `InProcTransport`
/// (in-memory, test-only, supports drop/latency injection).
#[async_trait::async_trait]
pub trait Transport: Send {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError>;
    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError>;
    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<ReceivedMessage, TransportError>;
}
```

**注**: `async-trait` 依存を追加する必要あり。次 Step で Cargo.toml 修正。

- [ ] **Step 3: `async-trait` 依存を `transport/Cargo.toml` に追加**

File: `crates/transport/Cargo.toml` の `[dependencies]` セクションに追加:
```toml
async-trait = "0.1"
```

- [ ] **Step 4: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod error;
pub mod transport_trait;

pub use error::TransportError;
pub use transport_trait::{ReceivedMessage, Transport};
```

- [ ] **Step 5: テスト**

Run:
```bash
cargo test -p prdt-transport
```
Expected: 1 test passed (error::tests::display_is_stable)

- [ ] **Step 6: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add TransportError and Transport trait"
```

---

### Task 14: FEC ラッパ(`fec.rs`)

**Files:**
- Create: `crates/transport/src/fec.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 5.7。

- [ ] **Step 1: FEC encoder/decoder 型**

File: `crates/transport/src/fec.rs`
```rust
use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::error::TransportError;

/// Default FEC parameters for Phase 0. See spec §5.7.
pub const DEFAULT_K: usize = 8;
pub const DEFAULT_M: usize = 2;
pub const MAX_SHARDS: usize = 32 + 16; // defensive cap; 1 frame max 32 source + 16 parity

/// Wraps a ReedSolomon codec with per-frame shard encoding/decoding.
///
/// All shards MUST have the same length. Callers pad the last source
/// shard with zeros before passing here (the length is tracked separately
/// in the VideoPacket.payload_bytes field so the receiver knows the true
/// length of the final source chunk).
pub struct FecCodec {
    k: usize,
    m: usize,
    rs: ReedSolomon,
}

impl FecCodec {
    pub fn new(k: usize, m: usize) -> Result<Self, TransportError> {
        if k == 0 || m == 0 {
            return Err(TransportError::FecConfig(format!("k={k}, m={m} must be > 0")));
        }
        if k + m > MAX_SHARDS {
            return Err(TransportError::FecConfig(format!(
                "k+m={} exceeds MAX_SHARDS={}", k + m, MAX_SHARDS,
            )));
        }
        let rs = ReedSolomon::new(k, m)
            .map_err(|e| TransportError::FecConfig(format!("reed-solomon: {e}")))?;
        Ok(Self { k, m, rs })
    }

    pub fn k(&self) -> usize { self.k }
    pub fn m(&self) -> usize { self.m }

    /// Produce m parity shards given k source shards (all same length).
    pub fn encode_parity(&self, source: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, TransportError> {
        if source.len() != self.k {
            return Err(TransportError::FecConfig(format!(
                "expected {} source shards, got {}", self.k, source.len(),
            )));
        }
        let shard_len = source[0].len();
        for s in source {
            if s.len() != shard_len {
                return Err(TransportError::FecConfig(
                    "source shards must all be same length".into(),
                ));
            }
        }
        let mut all: Vec<Vec<u8>> = source.to_vec();
        for _ in 0..self.m {
            all.push(vec![0u8; shard_len]);
        }
        self.rs.encode(&mut all)
            .map_err(|e| TransportError::FecConfig(format!("rs encode: {e}")))?;
        Ok(all.split_off(self.k)) // only parity
    }

    /// Reconstruct missing source shards. `shards[i] = None` marks missing.
    /// Length of `shards` must be exactly k + m. Returns k source shards.
    pub fn reconstruct(
        &self,
        shards: Vec<Option<Vec<u8>>>,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        if shards.len() != self.k + self.m {
            return Err(TransportError::FecConfig(format!(
                "expected {} shards, got {}", self.k + self.m, shards.len(),
            )));
        }
        let have = shards.iter().filter(|s| s.is_some()).count();
        if have < self.k {
            return Err(TransportError::FecFailed {
                frame_seq: 0, // caller overrides if they have seq context
                have,
                need: self.k,
            });
        }
        let mut rs_shards = shards;
        self.rs.reconstruct(&mut rs_shards)
            .map_err(|e| TransportError::FecConfig(format!("rs reconstruct: {e}")))?;
        Ok(rs_shards.into_iter().take(self.k).map(|s| s.unwrap()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fec_round_trip_no_loss() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 100]).collect();
        let parity = codec.encode_parity(&source).unwrap();
        assert_eq!(parity.len(), 2);
        assert_eq!(parity[0].len(), 100);
    }

    #[test]
    fn fec_reconstruct_one_lost_source() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 50]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        // Lose source shard index 1.
        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[1] = None;
        shards.extend(parity.into_iter().map(Some));

        let recovered = codec.reconstruct(shards).unwrap();
        assert_eq!(recovered.len(), 4);
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(*s, vec![i as u8; 50], "shard {i} mismatch");
        }
    }

    #[test]
    fn fec_reconstruct_two_lost() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 32]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[0] = None;
        shards[3] = None;
        shards.extend(parity.into_iter().map(Some));

        let recovered = codec.reconstruct(shards).unwrap();
        assert_eq!(recovered.len(), 4);
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(*s, vec![i as u8; 32]);
        }
    }

    #[test]
    fn fec_fails_when_too_many_lost() {
        let codec = FecCodec::new(4, 2).unwrap();
        let source: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 16]).collect();
        let parity = codec.encode_parity(&source).unwrap();

        // Lose 3 shards; with k=4, m=2 we need 4 of 6. 3 lost = 3 have → fail.
        let mut shards: Vec<Option<Vec<u8>>> = source.iter().cloned().map(Some).collect();
        shards[0] = None;
        shards[1] = None;
        shards[2] = None;
        shards.extend(parity.into_iter().map(Some));

        match codec.reconstruct(shards) {
            Err(TransportError::FecFailed { have: 3, need: 4, .. }) => {}
            other => panic!("expected FecFailed, got {:?}", other),
        }
    }

    #[test]
    fn fec_bad_config() {
        assert!(matches!(
            FecCodec::new(0, 2),
            Err(TransportError::FecConfig(_))
        ));
        assert!(matches!(
            FecCodec::new(100, 100),
            Err(TransportError::FecConfig(_))
        ));
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod error;
pub mod fec;
pub mod transport_trait;

pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use transport_trait::{ReceivedMessage, Transport};
```

- [ ] **Step 3: テスト**

Run:
```bash
cargo test -p prdt-transport fec
```
Expected: 5 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add Reed-Solomon FecCodec wrapper"
```

---

### Task 15: packetize.rs — EncodedFrame → Vec<VideoPacket>(FEC 適用)

**Files:**
- Create: `crates/transport/src/packetize.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 5.3、5.7、4.1 S3。

- [ ] **Step 1: 実装**

File: `crates/transport/src/packetize.rs`
```rust
use prdt_protocol::{wire::video_flags, EncodedFrame, VideoPacket, DEFAULT_CHUNK_PAYLOAD_LEN};

use crate::error::TransportError;
use crate::fec::FecCodec;

/// Max source chunks per frame (spec §5.3). Exceeding this should trigger
/// an IDR + bitrate drop at a higher layer.
pub const MAX_SOURCE_CHUNKS: usize = 32;

/// Split an EncodedFrame into k source chunks, then apply FEC to produce
/// m parity chunks. Returns exactly k + m VideoPackets.
///
/// All chunks use the SAME `chunk_payload` byte length (padded with zeros
/// on the last source chunk). The original frame byte length is preserved
/// indirectly through `payload_bytes` which records the true valid bytes
/// per chunk.
pub fn packetize(
    frame: &EncodedFrame,
    fec: &FecCodec,
    chunk_payload_len: usize,
) -> Result<Vec<VideoPacket>, TransportError> {
    let k = fec.k();
    let m = fec.m();

    // How many source chunks are needed?
    let bytes = frame.nal_units.len();
    let chunks_needed = (bytes + chunk_payload_len - 1) / chunk_payload_len;
    if chunks_needed > k {
        return Err(TransportError::FrameTooLarge {
            bytes,
            max_bytes: k * chunk_payload_len,
        });
    }
    if chunks_needed > MAX_SOURCE_CHUNKS {
        return Err(TransportError::FrameTooLarge {
            bytes,
            max_bytes: MAX_SOURCE_CHUNKS * chunk_payload_len,
        });
    }

    // Build k source shards, each of fixed length chunk_payload_len.
    let mut source: Vec<Vec<u8>> = Vec::with_capacity(k);
    for i in 0..k {
        let start = i * chunk_payload_len;
        let end = (start + chunk_payload_len).min(bytes);
        let mut shard = vec![0u8; chunk_payload_len];
        if start < bytes {
            shard[..end - start].copy_from_slice(&frame.nal_units[start..end]);
        }
        source.push(shard);
    }

    // Compute m parity shards.
    let parity = fec.encode_parity(&source)?;

    // Emit k + m VideoPackets.
    let kf_flag = if frame.is_keyframe { video_flags::IS_KEYFRAME } else { 0 };
    let mut out = Vec::with_capacity(k + m);
    for (idx, shard) in source.iter().enumerate() {
        let start = idx * chunk_payload_len;
        let end = (start + chunk_payload_len).min(bytes);
        let valid = end.saturating_sub(start) as u16;
        out.push(VideoPacket {
            frame_seq: frame.seq,
            timestamp_host_us: frame.timestamp_host_us,
            chunk_idx: idx as u16,
            source_chunks: k as u16,
            parity_chunks: m as u16,
            video_flags: kf_flag,
            payload_bytes: valid,
            chunk_payload: shard.clone(),
        });
    }
    for (idx, shard) in parity.iter().enumerate() {
        out.push(VideoPacket {
            frame_seq: frame.seq,
            timestamp_host_us: frame.timestamp_host_us,
            chunk_idx: (k + idx) as u16,
            source_chunks: k as u16,
            parity_chunks: m as u16,
            video_flags: kf_flag | video_flags::IS_PARITY,
            payload_bytes: chunk_payload_len as u16,
            chunk_payload: shard.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use prdt_protocol::frame::Codec;

    fn make_frame(bytes: &[u8]) -> EncodedFrame {
        EncodedFrame {
            seq: 1,
            timestamp_host_us: 42,
            is_keyframe: true,
            nal_units: Bytes::copy_from_slice(bytes),
            width: 3840,
            height: 2160,
            codec: Codec::H265,
        }
    }

    #[test]
    fn packetize_small_frame() {
        let fec = FecCodec::new(4, 2).unwrap();
        let payload = vec![0xAB; 10];
        let pkts = packetize(&make_frame(&payload), &fec, 100).unwrap();
        assert_eq!(pkts.len(), 6); // k + m
        assert_eq!(pkts[0].chunk_idx, 0);
        assert_eq!(pkts[0].source_chunks, 4);
        assert_eq!(pkts[0].parity_chunks, 2);
        assert!(pkts[0].is_keyframe());
        assert!(!pkts[0].is_parity());
        assert_eq!(pkts[0].payload_bytes, 10);
        assert_eq!(pkts[0].chunk_payload[..10], [0xAB; 10]);
        // rest of the shard is zero-padded
        assert_eq!(pkts[0].chunk_payload[10..], [0u8; 90]);
        // parity flag
        assert!(pkts[4].is_parity());
        assert!(pkts[5].is_parity());
    }

    #[test]
    fn packetize_frame_spanning_multiple_chunks() {
        let fec = FecCodec::new(4, 2).unwrap();
        let payload: Vec<u8> = (0..=255).cycle().take(350).collect();
        let pkts = packetize(&make_frame(&payload), &fec, 100).unwrap();
        assert_eq!(pkts.len(), 6);
        // chunk 0..=2 are full, chunk 3 has 50 valid bytes
        assert_eq!(pkts[0].payload_bytes, 100);
        assert_eq!(pkts[1].payload_bytes, 100);
        assert_eq!(pkts[2].payload_bytes, 100);
        assert_eq!(pkts[3].payload_bytes, 50);
    }

    #[test]
    fn packetize_rejects_oversize() {
        let fec = FecCodec::new(2, 1).unwrap();
        let huge = vec![0u8; 500]; // needs 5 chunks at 100B but k=2
        let err = packetize(&make_frame(&huge), &fec, 100).unwrap_err();
        assert!(matches!(err, TransportError::FrameTooLarge { .. }));
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod error;
pub mod fec;
pub mod packetize;
pub mod transport_trait;

pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-transport packetize
```
Expected: 3 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add packetize() to split EncodedFrame into FEC chunks"
```

---

### Task 16: FrameAssembler(受信側 reassembler + タイムアウト + FEC 復元)

**Files:**
- Create: `crates/transport/src/assembler.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 5.8、4.7。

- [ ] **Step 1: 実装**

File: `crates/transport/src/assembler.rs`
```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use prdt_protocol::{frame::Codec, wire::video_flags, EncodedFrame, VideoPacket};

use crate::error::TransportError;
use crate::fec::FecCodec;

pub const DEFAULT_ASSEMBLY_TIMEOUT: Duration = Duration::from_millis(100);
pub const STALE_SEQ_WINDOW: u64 = 8;

/// Per-frame partial state.
#[derive(Debug)]
struct Partial {
    first_seen: Instant,
    source_chunks: u16,
    parity_chunks: u16,
    // chunk_idx → payload (full-length shard, payload_bytes for valid length of last source chunk)
    chunks: HashMap<u16, Vec<u8>>,
    // payload_bytes of each source chunk we've received (idx → bytes)
    source_payload_bytes: HashMap<u16, u16>,
    is_keyframe: bool,
}

/// Reassembles VideoPackets into EncodedFrames.
///
/// Internally tracks many in-flight frames. Call `try_pop_ready` to retrieve
/// newly-completed frames. Call `purge` periodically to drop timed-out frames.
pub struct FrameAssembler {
    partials: HashMap<u64, Partial>,
    /// Highest frame_seq we've ever completed or declined. Used for stale-drop.
    high_water_seq: u64,
    timeout: Duration,
    width: u32,
    height: u32,
    codec: Codec,
}

/// Outcome of feeding one VideoPacket.
#[derive(Debug)]
pub enum FeedResult {
    /// Still waiting for more chunks.
    Pending,
    /// This chunk was dropped (stale, or frame already completed).
    Stale,
    /// Frame is fully recovered (either all source chunks arrived, or FEC
    /// reconstructed the missing ones).
    Complete(EncodedFrame),
}

impl FrameAssembler {
    pub fn new(width: u32, height: u32, codec: Codec) -> Self {
        Self {
            partials: HashMap::new(),
            high_water_seq: 0,
            timeout: DEFAULT_ASSEMBLY_TIMEOUT,
            width,
            height,
            codec,
        }
    }

    pub fn set_timeout(&mut self, d: Duration) {
        self.timeout = d;
    }

    /// Feed one VideoPacket. `fec` is used for reconstruction if enough
    /// chunks have arrived but some are missing.
    pub fn feed(
        &mut self,
        pkt: VideoPacket,
        fec: &FecCodec,
    ) -> Result<FeedResult, TransportError> {
        // Drop stale frames (older than high_water - window).
        if pkt.frame_seq + STALE_SEQ_WINDOW < self.high_water_seq.saturating_add(1) {
            return Ok(FeedResult::Stale);
        }

        let total = pkt.source_chunks as usize + pkt.parity_chunks as usize;
        let shard_len = pkt.chunk_payload.len();
        let is_source = !pkt.is_parity();
        let payload_bytes = pkt.payload_bytes;

        let entry = self.partials.entry(pkt.frame_seq).or_insert_with(|| Partial {
            first_seen: Instant::now(),
            source_chunks: pkt.source_chunks,
            parity_chunks: pkt.parity_chunks,
            chunks: HashMap::new(),
            source_payload_bytes: HashMap::new(),
            is_keyframe: pkt.is_keyframe(),
        });

        // Paranoia: if a later packet disagrees on source/parity counts, trust the first.
        if entry.chunks.contains_key(&pkt.chunk_idx) {
            return Ok(FeedResult::Pending);
        }
        entry.chunks.insert(pkt.chunk_idx, pkt.chunk_payload);
        if is_source {
            entry.source_payload_bytes.insert(pkt.chunk_idx, payload_bytes);
        }
        if pkt.is_keyframe() {
            entry.is_keyframe = true;
        }

        let have = entry.chunks.len();
        let k = entry.source_chunks as usize;

        if have >= k {
            // Attempt reconstruction (possibly trivial if all source present).
            let seq = pkt.frame_seq;
            let ts = pkt.timestamp_host_us;
            let is_kf = entry.is_keyframe;
            let maybe_frame = self.try_complete(seq, total, shard_len, ts, is_kf, fec);
            match maybe_frame {
                Ok(Some(frame)) => {
                    self.high_water_seq = self.high_water_seq.max(seq);
                    self.partials.remove(&seq);
                    return Ok(FeedResult::Complete(frame));
                }
                Ok(None) => return Ok(FeedResult::Pending),
                Err(e) => return Err(e),
            }
        }
        Ok(FeedResult::Pending)
    }

    fn try_complete(
        &mut self,
        seq: u64,
        total: usize,
        shard_len: usize,
        ts: u64,
        is_keyframe: bool,
        fec: &FecCodec,
    ) -> Result<Option<EncodedFrame>, TransportError> {
        let entry = match self.partials.get(&seq) {
            Some(e) => e,
            None => return Ok(None),
        };
        let k = entry.source_chunks as usize;
        if entry.chunks.len() < k {
            return Ok(None);
        }

        // Build k+m shard vector in index order with None for missing slots.
        let mut shards: Vec<Option<Vec<u8>>> = (0..total)
            .map(|i| entry.chunks.get(&(i as u16)).cloned())
            .collect();

        // If any source chunk missing, reconstruct.
        let missing_source = (0..k).any(|i| shards[i].is_none());
        let source: Vec<Vec<u8>> = if missing_source {
            let reconstructed = fec.reconstruct(shards.clone())
                .map_err(|e| match e {
                    TransportError::FecFailed { have, need, .. } => {
                        TransportError::FecFailed { frame_seq: seq, have, need }
                    }
                    other => other,
                })?;
            reconstructed
        } else {
            // All source present; take them directly.
            shards.drain(..k).map(|s| s.unwrap()).collect()
        };

        // Stitch source shards back into a single EncodedFrame, honouring
        // payload_bytes for the (possibly) partial last chunk.
        let total_bytes = Self::compute_total_bytes(k, shard_len, entry);
        let mut buf = Vec::with_capacity(total_bytes);
        for i in 0..k {
            let valid = entry
                .source_payload_bytes
                .get(&(i as u16))
                .copied()
                .unwrap_or(shard_len as u16) as usize;
            buf.extend_from_slice(&source[i][..valid]);
        }

        Ok(Some(EncodedFrame {
            seq,
            timestamp_host_us: ts,
            is_keyframe,
            nal_units: Bytes::from(buf),
            width: self.width,
            height: self.height,
            codec: self.codec,
        }))
    }

    fn compute_total_bytes(k: usize, shard_len: usize, entry: &Partial) -> usize {
        let mut total = 0;
        for i in 0..k {
            let valid = entry
                .source_payload_bytes
                .get(&(i as u16))
                .copied()
                .unwrap_or(shard_len as u16) as usize;
            total += valid;
        }
        total
    }

    /// Drop frames older than `self.timeout`. Returns Vec of frame_seqs
    /// that were purged; caller can use this to trigger IDR requests.
    pub fn purge(&mut self) -> Vec<u64> {
        let now = Instant::now();
        let stale: Vec<u64> = self
            .partials
            .iter()
            .filter(|(_, p)| now.duration_since(p.first_seen) > self.timeout)
            .map(|(seq, _)| *seq)
            .collect();
        for seq in &stale {
            self.partials.remove(seq);
            self.high_water_seq = self.high_water_seq.max(*seq);
        }
        stale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packetize::packetize;
    use bytes::Bytes;

    fn make_frame(seq: u64, bytes: &[u8]) -> EncodedFrame {
        EncodedFrame {
            seq,
            timestamp_host_us: seq * 1000,
            is_keyframe: true,
            nal_units: Bytes::copy_from_slice(bytes),
            width: 1920,
            height: 1080,
            codec: Codec::H265,
        }
    }

    #[test]
    fn assembler_trivial_all_chunks() {
        let fec = FecCodec::new(4, 2).unwrap();
        let frame = make_frame(1, &[0xAA; 250]);
        let pkts = packetize(&frame, &fec, 100).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        // Feed source chunks only; skip parity.
        let mut last = FeedResult::Pending;
        for p in pkts.iter().take(4).cloned() {
            last = asm.feed(p, &fec).unwrap();
        }
        match last {
            FeedResult::Complete(f) => {
                assert_eq!(f.seq, 1);
                assert_eq!(&f.nal_units[..], &[0xAA; 250][..]);
                assert!(f.is_keyframe);
            }
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn assembler_reconstructs_missing_source() {
        let fec = FecCodec::new(4, 2).unwrap();
        let frame = make_frame(1, &[0xCD; 200]);
        let mut pkts = packetize(&frame, &fec, 100).unwrap();
        // Drop source chunk idx 1.
        pkts.remove(1);
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);

        let mut final_result: Option<EncodedFrame> = None;
        for p in pkts {
            if let FeedResult::Complete(f) = asm.feed(p, &fec).unwrap() {
                final_result = Some(f);
                break;
            }
        }
        let f = final_result.expect("should complete via FEC");
        assert_eq!(&f.nal_units[..], &[0xCD; 200][..]);
    }

    #[test]
    fn assembler_drops_stale() {
        let fec = FecCodec::new(4, 2).unwrap();
        let f1 = make_frame(100, &[0; 10]);
        let pkts_f1 = packetize(&f1, &fec, 100).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);
        for p in pkts_f1.into_iter().take(4) {
            asm.feed(p, &fec).unwrap();
        }
        // Now try a stale seq = 50; high_water_seq is now 100.
        let stale_frame = make_frame(50, &[0; 10]);
        let stale_pkts = packetize(&stale_frame, &fec, 100).unwrap();
        let r = asm.feed(stale_pkts[0].clone(), &fec).unwrap();
        assert!(matches!(r, FeedResult::Stale));
    }

    #[test]
    fn assembler_purges_timed_out() {
        let fec = FecCodec::new(4, 2).unwrap();
        let frame = make_frame(1, &[0; 10]);
        let pkts = packetize(&frame, &fec, 100).unwrap();
        let mut asm = FrameAssembler::new(1920, 1080, Codec::H265);
        asm.set_timeout(Duration::from_millis(1));
        asm.feed(pkts[0].clone(), &fec).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let purged = asm.purge();
        assert_eq!(purged, vec![1]);
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod packetize;
pub mod transport_trait;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-transport assembler
```
Expected: 4 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add FrameAssembler with FEC reconstruction and timeout"
```

---

### Task 17: In-process(loopback)Transport 実装

**Files:**
- Create: `crates/transport/src/loopback.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 7.3(Integration Loopback Test)。

これは Plan 4(latency-bench)でも、本クレートのテストでも使う重要コンポーネント。

- [ ] **Step 1: 実装(人工損失/遅延注入対応)**

File: `crates/transport/src/loopback.rs`
```rust
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use prdt_protocol::{control::ControlMessage, input::InputEvent, EncodedFrame};
use tokio::sync::mpsc;

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

/// Options for simulating network degradation during tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackOptions {
    /// Per-message drop probability in ppm (0..=1_000_000).
    pub drop_ppm: u32,
    /// Fixed latency to add to every delivered message.
    pub latency: Option<Duration>,
}

/// An in-process transport that delivers messages directly via channels.
/// Used for unit/integration tests and for Phase 0 latency-bench M2.
pub struct InProcTransport {
    send_tx: mpsc::UnboundedSender<ReceivedMessage>,
    recv_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ReceivedMessage>>>,
    opts: LoopbackOptions,
}

impl InProcTransport {
    /// Create a connected pair (like `tokio::sync::mpsc::channel` but
    /// bidirectional and typed for our messages). Both ends can send and
    /// receive.
    pub fn pair(opts: LoopbackOptions) -> (Self, Self) {
        let (a_to_b_tx, a_to_b_rx) = mpsc::unbounded_channel();
        let (b_to_a_tx, b_to_a_rx) = mpsc::unbounded_channel();
        let side_a = InProcTransport {
            send_tx: a_to_b_tx,
            recv_rx: Arc::new(tokio::sync::Mutex::new(b_to_a_rx)),
            opts,
        };
        let side_b = InProcTransport {
            send_tx: b_to_a_tx,
            recv_rx: Arc::new(tokio::sync::Mutex::new(a_to_b_rx)),
            opts,
        };
        (side_a, side_b)
    }

    fn should_drop(&self) -> bool {
        if self.opts.drop_ppm == 0 { return false; }
        // xorshift64-ish per-call (cheap, good-enough for tests)
        use std::sync::atomic::{AtomicU64, Ordering};
        static STATE: AtomicU64 = AtomicU64::new(0x2545F4914F6CDD1D);
        let mut x = STATE.load(Ordering::Relaxed);
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        STATE.store(x, Ordering::Relaxed);
        let r = (x % 1_000_000) as u32;
        r < self.opts.drop_ppm
    }

    async fn send_msg(&self, msg: ReceivedMessage) -> Result<(), TransportError> {
        if self.should_drop() {
            return Ok(()); // silently drop, simulating UDP loss
        }
        if let Some(d) = self.opts.latency {
            tokio::time::sleep(d).await;
        }
        self.send_tx
            .send(msg)
            .map_err(|_| TransportError::PeerClosed)?;
        Ok(())
    }
}

#[async_trait]
impl Transport for InProcTransport {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Video(frame)).await
    }

    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Input(ev)).await
    }

    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError> {
        self.send_msg(ReceivedMessage::Control(msg)).await
    }

    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        let mut rx = self.recv_rx.lock().await;
        rx.recv().await.ok_or(TransportError::PeerClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use prdt_protocol::frame::Codec;

    fn frame(seq: u64) -> EncodedFrame {
        EncodedFrame {
            seq,
            timestamp_host_us: seq,
            is_keyframe: seq == 0,
            nal_units: Bytes::from_static(&[0xAA; 10]),
            width: 1920,
            height: 1080,
            codec: Codec::H265,
        }
    }

    #[tokio::test]
    async fn loopback_basic_round_trip() {
        let (a, b) = InProcTransport::pair(LoopbackOptions::default());
        a.send_video(frame(1)).await.unwrap();
        let msg = b.recv().await.unwrap();
        match msg {
            ReceivedMessage::Video(f) => assert_eq!(f.seq, 1),
            _ => panic!("expected Video"),
        }
    }

    #[tokio::test]
    async fn loopback_input_and_control() {
        let (a, b) = InProcTransport::pair(LoopbackOptions::default());
        a.send_input(InputEvent::Key { scancode: 0x1E, pressed: true })
            .await
            .unwrap();
        a.send_control(ControlMessage::RequestIdr).await.unwrap();
        let m1 = b.recv().await.unwrap();
        assert!(matches!(m1, ReceivedMessage::Input(_)));
        let m2 = b.recv().await.unwrap();
        assert!(matches!(m2, ReceivedMessage::Control(ControlMessage::RequestIdr)));
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use loopback::{InProcTransport, LoopbackOptions};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-transport loopback
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add InProcTransport for loopback tests"
```

---

### Task 18: CustomUdpTransport(送信側: chunk + FEC + sendto)

**Files:**
- Create: `crates/transport/src/udp.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 5.13、4.1 S3-S4、5.2-5.5。

送信側だけまず作る(受信側は次 Task)。

- [ ] **Step 1: 送信側の実装**

File: `crates/transport/src/udp.rs`
```rust
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use prdt_protocol::{
    control::ControlMessage,
    input::InputEvent,
    wire::{InputPacket, PacketHeader, PacketType, HEADER_LEN},
    EncodedFrame,
};
use tokio::net::UdpSocket;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::TransportError;
use crate::fec::FecCodec;
use crate::packetize::packetize;
use crate::transport_trait::{ReceivedMessage, Transport};

/// Configuration for a CustomUdpTransport instance.
#[derive(Debug, Clone, Copy)]
pub struct UdpTransportConfig {
    pub session_id: u64,
    pub chunk_payload_len: usize,
    pub fec_k: usize,
    pub fec_m: usize,
}

impl Default for UdpTransportConfig {
    fn default() -> Self {
        Self {
            session_id: 0,
            chunk_payload_len: prdt_protocol::DEFAULT_CHUNK_PAYLOAD_LEN,
            fec_k: 8,
            fec_m: 2,
        }
    }
}

/// UDP transport with per-frame FEC. Recv path lives in a separate task.
pub struct CustomUdpTransport {
    socket: Arc<UdpSocket>,
    cfg: UdpTransportConfig,
    peer: AsyncMutex<Option<SocketAddr>>, // set after first packet received or configure_peer()
    fec: FecCodec,
    input_seq: AsyncMutex<u64>,
}

impl CustomUdpTransport {
    pub async fn bind(addr: SocketAddr, cfg: UdpTransportConfig) -> Result<Self, TransportError> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let fec = FecCodec::new(cfg.fec_k, cfg.fec_m)?;
        Ok(Self {
            socket,
            cfg,
            peer: AsyncMutex::new(None),
            fec,
            input_seq: AsyncMutex::new(0),
        })
    }

    pub async fn configure_peer(&self, peer: SocketAddr) {
        *self.peer.lock().await = Some(peer);
    }

    async fn current_peer(&self) -> Result<SocketAddr, TransportError> {
        self.peer
            .lock()
            .await
            .ok_or_else(|| TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "peer address not set",
            )))
    }

    async fn send_raw(&self, hdr: PacketHeader, body: &[u8]) -> Result<(), TransportError> {
        let peer = self.current_peer().await?;
        let mut buf = Vec::with_capacity(HEADER_LEN + body.len());
        buf.extend_from_slice(&hdr.encode());
        buf.extend_from_slice(body);
        self.socket.send_to(&buf, peer).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for CustomUdpTransport {
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError> {
        let pkts = packetize(&frame, &self.fec, self.cfg.chunk_payload_len)?;
        for pkt in pkts {
            let body = pkt.encode();
            let hdr = PacketHeader {
                packet_type: PacketType::Video,
                flags: 0,
                session_id: self.cfg.session_id,
                payload_len: body.len() as u32,
            };
            self.send_raw(hdr, &body).await?;
        }
        Ok(())
    }

    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError> {
        let seq = {
            let mut g = self.input_seq.lock().await;
            *g += 1;
            *g
        };
        let pkt = InputPacket {
            input_seq: seq,
            timestamp_viewer_us: now_monotonic_us(),
            event: ev,
        };
        let body = pkt.encode();
        let hdr = PacketHeader {
            packet_type: PacketType::Input,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw(hdr, &body).await
    }

    async fn send_control(&self, msg: ControlMessage) -> Result<(), TransportError> {
        let body = prdt_protocol::encode_control(&msg)?;
        let hdr = PacketHeader {
            packet_type: PacketType::Control,
            flags: 0,
            session_id: self.cfg.session_id,
            payload_len: body.len() as u32,
        };
        self.send_raw(hdr, &body).await
    }

    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        // Recv path is wired up in the next task. For now stub so the trait compiles.
        Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "recv not yet implemented (see Task 19)",
        )))
    }
}

/// Monotonic clock reading in microseconds (u64). Uses Instant::now() on a
/// per-process epoch that is set the first time this function is called.
pub fn now_monotonic_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_micros() as u64
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use loopback::{InProcTransport, LoopbackOptions};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
pub use udp::{now_monotonic_us, CustomUdpTransport, UdpTransportConfig};
```

- [ ] **Step 3: ビルド確認(まだテストなし)**

Run:
```bash
cargo check -p prdt-transport --all-targets
```
Expected: ビルド成功、警告ゼロ

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add CustomUdpTransport send path (video/input/control)"
```

---

### Task 19: CustomUdpTransport recv 経路(assembler 統合)

**Files:**
- Modify: `crates/transport/src/udp.rs`

- [ ] **Step 1: recv 実装**

`crates/transport/src/udp.rs` の `recv()` スタブ関数全体を以下で置き換え、必要なフィールド(`assembler`、`width`/`height`/`codec` などのデフォルト値)も同時に追加:

まず型定義(struct CustomUdpTransport)に以下を追加 — 既存フィールドの直後に:

```rust
    assembler: AsyncMutex<crate::assembler::FrameAssembler>,
```

既存の `Self { socket, cfg, peer, fec, input_seq }` の初期化を以下に置き換え(`bind` 内):
```rust
        Ok(Self {
            socket,
            cfg,
            peer: AsyncMutex::new(None),
            fec,
            input_seq: AsyncMutex::new(0),
            assembler: AsyncMutex::new(crate::assembler::FrameAssembler::new(
                1920, 1080, prdt_protocol::frame::Codec::H265,
            )),
        })
```

そして `recv` メソッド全体を以下に置き換え:

```rust
    async fn recv(&self) -> Result<ReceivedMessage, TransportError> {
        let mut buf = vec![0u8; 2048];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            // Record peer on first packet if not yet set.
            {
                let mut p = self.peer.lock().await;
                if p.is_none() {
                    *p = Some(from);
                }
            }

            let hdr = match prdt_protocol::wire::PacketHeader::decode(&buf[..n]) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(?e, "dropping malformed packet");
                    continue;
                }
            };
            if hdr.session_id != self.cfg.session_id && self.cfg.session_id != 0 {
                tracing::warn!(
                    "session mismatch: got {}, expected {}", hdr.session_id, self.cfg.session_id
                );
                continue;
            }
            let body_end = HEADER_LEN + hdr.payload_len as usize;
            if body_end > n {
                tracing::warn!(
                    "truncated packet: hdr.payload_len={} but only {} bytes received",
                    hdr.payload_len, n - HEADER_LEN,
                );
                continue;
            }
            let body = &buf[HEADER_LEN..body_end];

            match hdr.packet_type {
                PacketType::Video => {
                    let vp = match prdt_protocol::VideoPacket::decode(body) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(?e, "bad VideoPacket");
                            continue;
                        }
                    };
                    let mut asm = self.assembler.lock().await;
                    match asm.feed(vp, &self.fec) {
                        Ok(crate::FeedResult::Complete(frame)) => {
                            return Ok(ReceivedMessage::Video(frame));
                        }
                        Ok(crate::FeedResult::Pending) | Ok(crate::FeedResult::Stale) => continue,
                        Err(e) => {
                            tracing::warn!(?e, "assembler error");
                            continue;
                        }
                    }
                }
                PacketType::Input => {
                    let ip = prdt_protocol::InputPacket::decode(body)?;
                    return Ok(ReceivedMessage::Input(ip.event));
                }
                PacketType::Control => {
                    let msg = prdt_protocol::decode_control(body)?;
                    return Ok(ReceivedMessage::Control(msg));
                }
            }
        }
    }
```

- [ ] **Step 2: ビルド確認**

Run:
```bash
cargo check -p prdt-transport --all-targets
```
Expected: ビルド成功

- [ ] **Step 3: Integration test(実 UDP loopback)を追加**

File: `crates/transport/tests/udp_test.rs`
```rust
use std::time::Duration;

use bytes::Bytes;
use prdt_protocol::{control::ControlMessage, frame::Codec, EncodedFrame, InputEvent, MouseButton};
use prdt_transport::{
    CustomUdpTransport, ReceivedMessage, Transport, UdpTransportConfig,
};

#[tokio::test]
async fn udp_round_trip_control() {
    let cfg = UdpTransportConfig { session_id: 0xAA, ..Default::default() };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();

    let a_addr = a.local_addr().unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a_addr).await;

    a.send_control(ControlMessage::RequestIdr).await.unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), b.recv()).await.unwrap().unwrap();
    assert!(matches!(m, ReceivedMessage::Control(ControlMessage::RequestIdr)));
}

#[tokio::test]
async fn udp_round_trip_input() {
    let cfg = UdpTransportConfig { session_id: 1, ..Default::default() };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a.local_addr().unwrap()).await;

    a.send_input(InputEvent::MouseButton { button: MouseButton::Left, pressed: true })
        .await
        .unwrap();
    let m = tokio::time::timeout(Duration::from_millis(500), b.recv()).await.unwrap().unwrap();
    match m {
        ReceivedMessage::Input(InputEvent::MouseButton { button, pressed }) => {
            assert_eq!(button, MouseButton::Left);
            assert!(pressed);
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[tokio::test]
async fn udp_round_trip_video_small_frame() {
    let cfg = UdpTransportConfig { session_id: 2, fec_k: 4, fec_m: 2, ..Default::default() };
    let a = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();
    let b = CustomUdpTransport::bind("127.0.0.1:0".parse().unwrap(), cfg).await.unwrap();
    let b_addr = b.local_addr().unwrap();
    a.configure_peer(b_addr).await;
    b.configure_peer(a.local_addr().unwrap()).await;

    let frame = EncodedFrame {
        seq: 1,
        timestamp_host_us: 42,
        is_keyframe: true,
        nal_units: Bytes::copy_from_slice(&[0xAA; 500]),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    };
    a.send_video(frame.clone()).await.unwrap();
    let m = tokio::time::timeout(Duration::from_secs(1), b.recv()).await.unwrap().unwrap();
    match m {
        ReceivedMessage::Video(got) => {
            assert_eq!(got.seq, 1);
            assert!(got.is_keyframe);
            assert_eq!(&got.nal_units[..], &[0xAA; 500][..]);
        }
        other => panic!("unexpected {:?}", other),
    }
}
```

- [ ] **Step 2.5: `local_addr()` アクセサを `CustomUdpTransport` に追加**

`crates/transport/src/udp.rs` の `impl CustomUdpTransport { ... }` ブロックに追加:
```rust
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }
```

- [ ] **Step 4: テスト実行**

Run:
```bash
cargo test -p prdt-transport --test udp_test
```
Expected: 3 tests passed

- [ ] **Step 5: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): implement CustomUdpTransport recv path with assembler"
```

---

### Task 20: Handshake ヘルパ(Hello/HelloAck + タイムアウト)

**Files:**
- Create: `crates/transport/src/handshake.rs`
- Modify: `crates/transport/src/lib.rs`

**Spec ref:** 4.4。

- [ ] **Step 1: ヘルパ実装**

File: `crates/transport/src/handshake.rs`
```rust
use std::time::Duration;

use prdt_protocol::{
    control::ControlMessage,
    frame::Codec,
};

use crate::error::TransportError;
use crate::transport_trait::{ReceivedMessage, Transport};

pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(3);
pub const DEFAULT_HELLO_RETRIES: u8 = 3;

#[derive(Debug, Clone)]
pub struct HelloRequest {
    pub req_width: u32,
    pub req_height: u32,
    pub req_fps: u32,
    pub codec: Codec,
}

#[derive(Debug, Clone)]
pub struct SessionAck {
    pub session_id: u64,
    pub host_monotonic_base_us: u64,
    pub neg_width: u32,
    pub neg_height: u32,
    pub neg_fps: u32,
    pub neg_bitrate_bps: u32,
}

/// Send Hello, await HelloAck. Retries on timeout, returns session info on success.
pub async fn viewer_handshake<T: Transport>(
    transport: &T,
    req: &HelloRequest,
    per_attempt_timeout: Duration,
    retries: u8,
) -> Result<SessionAck, TransportError> {
    for _ in 0..retries {
        let hello = ControlMessage::Hello {
            protocol_version: 1,
            req_width: req.req_width,
            req_height: req.req_height,
            req_fps: req.req_fps,
            codec: req.codec,
        };
        transport.send_control(hello).await?;

        let ack_fut = async {
            loop {
                match transport.recv().await? {
                    ReceivedMessage::Control(ControlMessage::HelloAck {
                        session_id,
                        host_monotonic_base_us,
                        neg_width,
                        neg_height,
                        neg_fps,
                        neg_bitrate_bps,
                    }) => {
                        return Ok::<SessionAck, TransportError>(SessionAck {
                            session_id,
                            host_monotonic_base_us,
                            neg_width,
                            neg_height,
                            neg_fps,
                            neg_bitrate_bps,
                        });
                    }
                    // ignore other messages during handshake
                    _ => continue,
                }
            }
        };
        match tokio::time::timeout(per_attempt_timeout, ack_fut).await {
            Ok(r) => return r,
            Err(_) => continue, // retry
        }
    }
    Err(TransportError::HandshakeTimeout)
}

/// Host-side: await Hello, respond with HelloAck.
pub async fn host_handshake<T: Transport>(
    transport: &T,
    session_id: u64,
    host_monotonic_base_us: u64,
    negotiated_bitrate_bps: u32,
    wait_timeout: Duration,
) -> Result<HelloRequest, TransportError> {
    let fut = async {
        loop {
            match transport.recv().await? {
                ReceivedMessage::Control(ControlMessage::Hello {
                    protocol_version,
                    req_width,
                    req_height,
                    req_fps,
                    codec,
                }) => {
                    if protocol_version != 1 {
                        return Err(TransportError::Protocol(
                            prdt_protocol::ProtocolError::UnsupportedVersion(protocol_version),
                        ));
                    }
                    let ack = ControlMessage::HelloAck {
                        session_id,
                        host_monotonic_base_us,
                        neg_width: req_width,
                        neg_height: req_height,
                        neg_fps: req_fps,
                        neg_bitrate_bps: negotiated_bitrate_bps,
                    };
                    transport.send_control(ack).await?;
                    return Ok(HelloRequest { req_width, req_height, req_fps, codec });
                }
                _ => continue,
            }
        }
    };
    match tokio::time::timeout(wait_timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(TransportError::HandshakeTimeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback::{InProcTransport, LoopbackOptions};
    use prdt_protocol::frame::Codec;

    #[tokio::test]
    async fn handshake_happy_path() {
        let (viewer, host) = InProcTransport::pair(LoopbackOptions::default());

        let viewer_task = tokio::spawn(async move {
            viewer_handshake(
                &viewer,
                &HelloRequest {
                    req_width: 1920,
                    req_height: 1080,
                    req_fps: 60,
                    codec: Codec::H265,
                },
                Duration::from_millis(500),
                3,
            )
            .await
        });
        let host_task = tokio::spawn(async move {
            host_handshake(&host, 0x1234, 42, 10_000_000, Duration::from_millis(500)).await
        });

        let (v, h) = tokio::join!(viewer_task, host_task);
        let ack = v.unwrap().unwrap();
        let req = h.unwrap().unwrap();
        assert_eq!(ack.session_id, 0x1234);
        assert_eq!(ack.neg_width, 1920);
        assert_eq!(req.req_fps, 60);
    }

    #[tokio::test]
    async fn handshake_timeout_when_no_ack() {
        // drop every control packet
        let (viewer, _host) = InProcTransport::pair(LoopbackOptions {
            drop_ppm: 1_000_000,
            latency: None,
        });

        let err = viewer_handshake(
            &viewer,
            &HelloRequest {
                req_width: 1920,
                req_height: 1080,
                req_fps: 60,
                codec: Codec::H265,
            },
            Duration::from_millis(50),
            2,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::HandshakeTimeout));
    }
}
```

- [ ] **Step 2: `lib.rs` から公開**

File: `crates/transport/src/lib.rs`
```rust
//! Custom UDP transport with FEC for power-remote-dt.

pub mod assembler;
pub mod error;
pub mod fec;
pub mod handshake;
pub mod loopback;
pub mod packetize;
pub mod transport_trait;
pub mod udp;

pub use assembler::{FeedResult, FrameAssembler, DEFAULT_ASSEMBLY_TIMEOUT, STALE_SEQ_WINDOW};
pub use error::TransportError;
pub use fec::{FecCodec, DEFAULT_K, DEFAULT_M};
pub use handshake::{
    host_handshake, viewer_handshake, HelloRequest, SessionAck, DEFAULT_HELLO_RETRIES,
    DEFAULT_HELLO_TIMEOUT,
};
pub use loopback::{InProcTransport, LoopbackOptions};
pub use packetize::{packetize, MAX_SOURCE_CHUNKS};
pub use transport_trait::{ReceivedMessage, Transport};
pub use udp::{now_monotonic_us, CustomUdpTransport, UdpTransportConfig};
```

- [ ] **Step 3: テスト実行**

Run:
```bash
cargo test -p prdt-transport handshake
```
Expected: 2 tests passed

- [ ] **Step 4: コミット**

Run:
```bash
git add -A
git commit -m "feat(transport): add Hello/HelloAck handshake helpers with timeout+retry"
```

---

### Task 21: Loopback integration test(人工損失+レイテンシ注入、多数フレーム)

**Files:**
- Create: `crates/transport/tests/loopback_test.rs`

- [ ] **Step 1: 大量フレームを人工損失下で流す integration テスト**

File: `crates/transport/tests/loopback_test.rs`
```rust
use std::time::Duration;

use bytes::Bytes;
use prdt_protocol::{frame::Codec, EncodedFrame};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};

fn make_frame(seq: u64, size: usize) -> EncodedFrame {
    EncodedFrame {
        seq,
        timestamp_host_us: seq * 1000,
        is_keyframe: seq % 60 == 0,
        nal_units: Bytes::from(vec![(seq as u8).wrapping_mul(7); size]),
        width: 1920,
        height: 1080,
        codec: Codec::H265,
    }
}

/// Smoke: 100 frames, no loss, all delivered in order.
#[tokio::test]
async fn loopback_100_frames_no_loss() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions::default());
    let handle = tokio::spawn(async move {
        for i in 0..100 {
            host.send_video(make_frame(i, 500)).await.unwrap();
        }
    });
    let mut received = 0u64;
    while received < 100 {
        let m = tokio::time::timeout(Duration::from_secs(2), viewer.recv()).await.unwrap().unwrap();
        if let ReceivedMessage::Video(f) = m {
            assert_eq!(f.seq, received);
            received += 1;
        }
    }
    handle.await.unwrap();
}

/// With 10ms latency every message still arrives.
#[tokio::test]
async fn loopback_with_latency() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: 0,
        latency: Some(Duration::from_millis(10)),
    });
    let start = std::time::Instant::now();
    let sender = tokio::spawn(async move {
        for i in 0..20 {
            host.send_video(make_frame(i, 100)).await.unwrap();
        }
    });
    let mut count = 0;
    while count < 20 {
        if let ReceivedMessage::Video(_) =
            tokio::time::timeout(Duration::from_secs(5), viewer.recv()).await.unwrap().unwrap()
        {
            count += 1;
        }
    }
    sender.await.unwrap();
    // Last frame must arrive strictly after >= 20 * 10ms = 200ms (since latency is per-message serial).
    assert!(start.elapsed() >= Duration::from_millis(200));
}

/// With 5% drop rate, we expect some losses but the pipeline must not panic
/// and at least half the frames should still arrive.
#[tokio::test]
async fn loopback_with_drops_survives() {
    let (host, viewer) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: 50_000, // 5%
        latency: None,
    });
    let sender = tokio::spawn(async move {
        for i in 0..200 {
            let _ = host.send_video(make_frame(i, 100)).await;
        }
    });
    let mut received = 0;
    loop {
        match tokio::time::timeout(Duration::from_millis(100), viewer.recv()).await {
            Ok(Ok(ReceivedMessage::Video(_))) => received += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    sender.await.unwrap();
    assert!(received > 100, "too many losses: only {received}/200");
}
```

- [ ] **Step 2: テスト実行**

Run:
```bash
cargo test -p prdt-transport --test loopback_test -- --nocapture
```
Expected: 3 tests passed

- [ ] **Step 3: コミット**

Run:
```bash
git add -A
git commit -m "test(transport): add loopback integration tests (100 frames, latency, drops)"
```

---

### Task 22: `latency-bench` バイナリの骨組み(M2 モード)

**Files:**
- Modify: `crates/latency-bench/src/main.rs`
- Modify: `crates/latency-bench/Cargo.toml`

**Spec ref:** 7.4 M2。フルの M2 計測実装は Plan 4 で詳しくする。ここでは骨組みのみ。

- [ ] **Step 1: Cargo.toml に依存追加**

File: `crates/latency-bench/Cargo.toml` の `[dependencies]` に追加:
```toml
bytes = { workspace = true }
```

- [ ] **Step 2: 骨組み実装**

File: `crates/latency-bench/src/main.rs`
```rust
//! Phase 0 Plan 1: skeleton only. The full M2 harness lands in Plan 4,
//! but we lay out the CLI and the in-process test loop here so the
//! transport layer exercise path exists.

use std::time::{Duration, Instant};

use bytes::Bytes;
use clap::Parser;
use prdt_protocol::{frame::Codec, EncodedFrame};
use prdt_transport::{InProcTransport, LoopbackOptions, ReceivedMessage, Transport};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "prdt-latency-bench")]
struct Args {
    /// Mode: only `in-process` for Phase 0 Plan 1 (loopback via InProcTransport).
    #[arg(long, default_value = "in-process")]
    mode: String,

    /// Resolution (for sizing the synthetic frame). WxH.
    #[arg(long, default_value = "1920x1080")]
    resolution: String,

    /// Frames per second.
    #[arg(long, default_value_t = 60u32)]
    fps: u32,

    /// How long to run.
    #[arg(long, default_value = "5s")]
    duration: humantime::Duration,

    /// Per-message drop probability in ppm.
    #[arg(long, default_value_t = 0u32)]
    drop_ppm: u32,

    /// Added latency per message in milliseconds.
    #[arg(long, default_value_t = 0u64)]
    latency_ms: u64,
}

fn parse_res(s: &str) -> (u32, u32) {
    let (w, h) = s.split_once('x').unwrap_or(("1920", "1080"));
    (w.parse().unwrap_or(1920), h.parse().unwrap_or(1080))
}

fn synthetic_bytes(bytes: usize) -> Bytes {
    Bytes::from(vec![0x42u8; bytes])
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    if args.mode != "in-process" {
        anyhow::bail!("Phase 0 Plan 1 only supports --mode=in-process");
    }
    let (w, h) = parse_res(&args.resolution);
    let duration: Duration = args.duration.into();
    let frame_interval = Duration::from_secs_f64(1.0 / args.fps as f64);
    // Approx bitrate ~50 Mbps at 4K60. Frame bytes = bitrate / 8 / fps.
    let target_bitrate_bps = 50_000_000u64;
    let frame_bytes = (target_bitrate_bps / 8 / args.fps as u64) as usize;
    let frame_bytes = frame_bytes.min(12 * 1200); // cap at 12 chunks for Plan 1

    info!(
        resolution = %args.resolution,
        fps = args.fps,
        duration_ms = duration.as_millis(),
        frame_bytes,
        "starting in-process latency bench"
    );

    let (host_side, viewer_side) = InProcTransport::pair(LoopbackOptions {
        drop_ppm: args.drop_ppm,
        latency: if args.latency_ms > 0 {
            Some(Duration::from_millis(args.latency_ms))
        } else {
            None
        },
    });

    let deadline = Instant::now() + duration;
    let sender = tokio::spawn(async move {
        let mut seq = 0u64;
        let mut next = Instant::now();
        while Instant::now() < deadline {
            tokio::time::sleep_until(next.into()).await;
            let now_us = (Instant::now().elapsed().as_micros()) as u64;
            let frame = EncodedFrame {
                seq,
                timestamp_host_us: now_us,
                is_keyframe: seq % 60 == 0,
                nal_units: synthetic_bytes(frame_bytes),
                width: w,
                height: h,
                codec: Codec::H265,
            };
            if host_side.send_video(frame).await.is_err() {
                break;
            }
            seq += 1;
            next += frame_interval;
        }
        seq
    });

    let mut received = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), viewer_side.recv()).await {
            Ok(Ok(ReceivedMessage::Video(_))) => received += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let sent = sender.await.unwrap_or(0);
    info!(sent, received, "bench done");
    Ok(())
}
```

- [ ] **Step 3: `humantime` と `anyhow` の依存を追加**

File: `crates/latency-bench/Cargo.toml`(最終形):
```toml
[package]
name = "prdt-latency-bench"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[[bin]]
name = "prdt-latency-bench"
path = "src/main.rs"

[dependencies]
prdt-protocol = { path = "../protocol" }
prdt-transport = { path = "../transport" }
tokio.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
bytes.workspace = true
anyhow = "1"
humantime = "2"
```

- [ ] **Step 4: 動作確認(短時間実行)**

Run:
```bash
cargo run -p prdt-latency-bench --release -- --resolution 1920x1080 --fps 60 --duration 2s
```
Expected: 完走、stdout に `bench done` ログ、`sent=~120, received=~120` が表示される。

- [ ] **Step 5: コミット**

Run:
```bash
git add -A
git commit -m "feat(latency-bench): skeleton in-process mode for Phase 0 Plan 1"
```

---

### Task 23: 最終チェック — 全テスト + clippy + fmt + Plan 1 完了タグ

**Files:**
- なし(確認のみ)

- [ ] **Step 1: 全体テスト**

Run:
```bash
cargo test -p prdt-protocol -p prdt-transport -p prdt-latency-bench --all-targets
```
Expected: 全テスト成功(Task 4〜21 合計で protocol 20+ 件、transport 15+ 件、loopback 3 件)

- [ ] **Step 2: clippy 全体**

Run:
```bash
cargo clippy -p prdt-protocol -p prdt-transport -p prdt-latency-bench --all-targets -- -D warnings
```
Expected: warning ゼロ

- [ ] **Step 3: rustfmt**

Run:
```bash
cargo fmt --all -- --check
```
Expected: no output(整形済み)

- [ ] **Step 4: workspace 全体のビルドも通る**

Run:
```bash
cargo build --workspace --release
```
Expected: 成功(`prdt-host`、`prdt-viewer` の空バイナリもビルドされる)

- [ ] **Step 5: README にフェーズ進捗を 1 行追記**

File: `README.md`(新規作成、簡潔なもの)
```markdown
# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [ ] Plan 2: `media-win` (DXGI capture, NVENC, NVDEC, D3D11 render)
- [ ] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria

## Building

Requires Rust stable (≥1.78), Windows 11 for Plan 2+.

```bash
cargo test -p prdt-protocol -p prdt-transport
cargo run -p prdt-latency-bench --release -- --duration 2s
```
```

- [ ] **Step 6: Plan 1 完了コミット + タグ**

Run:
```bash
git add -A
git commit -m "docs: mark Phase 0 Plan 1 complete"
git tag phase0-plan1-complete
```

---

## Plan 1 完了判定

Plan 1 が完了したと言えるのは:

- [ ] Task 1〜23 の全ステップ完了
- [ ] `cargo test -p prdt-protocol -p prdt-transport --all-targets` が pass(約 38 件)
- [ ] `cargo clippy` が warning ゼロ
- [ ] `cargo run -p prdt-latency-bench --release -- --duration 2s` が完走し、`received` カウントが `sent` と同程度
- [ ] コミット履歴に 20+ の機能的コミット

**次のステップ**: Plan 2(`media-win` クレート — DXGI キャプチャ + NVENC/NVDEC FFI + D3D11 レンダ)に進む。Plan 1 で組んだ `Transport` trait / `EncodedFrame` / `InputEvent` を前提に、Plan 2 では実際の Windows GPU コードを書く。

---

*End of Phase 0 — Plan 1 of 4.*
