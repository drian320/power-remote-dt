# Phase 0 — Plan 2a of 4: D3D11 Foundation (media-win scaffold)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `media-win` クレート内に Windows 固有インフラ(D3D11 デバイス管理、テクスチャ操作、エラー型、Windows プラットフォーム検出)を構築する。NVENC/NVDEC/DXGI Desktop Duplication は Plan 2b で追加。この Plan は **unsafe FFI の最小限のみ**(`windows` crate 経由の D3D11)で、GPU が使えれば実機テスト可能だが CI では `--lib` ビルドのみ確認。

**Architecture:** `media-win` は `cfg(windows)` 下でのみコンパイルされる。内部に以下のモジュール:
- `error` — `MediaError` 型
- `d3d11` — `D3d11Device`(スレッドセーフラッパ)、`D3d11Texture` 薄いラッパ、ステージングコピー
- `adapter` — GPU アダプタ列挙、名前取得
- `platform` — MMCSS ("Games") スレッドタスクブースト、RT 優先度の管理
- `synthetic` — テスト用合成テクスチャ生成(単色塗り、カウンタパターン)

**Tech Stack:** Rust stable、`windows` crate(DXGI / D3D11 / Multimedia API)、Windows 11 開発、NVIDIA GPU 想定(ただし Plan 2a ではベンダ非依存)。

**Spec reference:** `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`(セクション 1.2、2.5、3.3、6.6 に対応)

---

## File Structure

```
crates/media-win/
├── Cargo.toml                          [modify] windows crate + 依存追加
├── src/
│   ├── lib.rs                          [modify] モジュール宣言、公開 API
│   ├── error.rs                        [new] MediaError + Result エイリアス
│   ├── d3d11/
│   │   ├── mod.rs                      [new] 公開 API 束ね
│   │   ├── device.rs                   [new] D3d11Device 作成、ImmediateContext 管理
│   │   └── texture.rs                  [new] D3d11Texture ラッパ、staging copy
│   ├── adapter.rs                      [new] アダプタ列挙
│   ├── platform.rs                     [new] MMCSS ヘルパ
│   └── synthetic.rs                    [new] 合成テクスチャ(テスト用)
└── tests/
    └── smoke.rs                        [new] GPU あり環境での smoke test(cfg ignore 可)
```

---

## Task List

23 タスクを Plan 2a として実装。

### Task 1: `media-win` Cargo.toml を更新(windows crate 追加)

**Files:**
- Modify: `crates/media-win/Cargo.toml`

- [ ] **Step 1: 更新後の Cargo.toml 内容を確認**

Current:
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

置き換え:
```toml
[package]
name = "prdt-media-win"
version = "0.0.1"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[target.'cfg(windows)'.dependencies]
prdt-protocol = { path = "../protocol" }
thiserror = { workspace = true }
tracing = { workspace = true }
bytes = { workspace = true }
windows = { version = "0.58", features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_System_Threading",
    "Win32_System_Performance",
    "Win32_Media",
] }

[target.'cfg(windows)'.dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "time"] }
```

- [ ] **Step 2: ビルド確認**

Run:
```bash
cargo check -p prdt-media-win
```
Expected: 成功(`windows` crate の初回ダウンロードで数秒〜数十秒)

- [ ] **Step 3: コミット**

```bash
git add crates/media-win/Cargo.toml
git commit -m "build(media-win): add windows crate + support deps for D3D11"
```

---

### Task 2: `MediaError` 型を定義

**Files:**
- Create: `crates/media-win/src/error.rs`
- Modify: `crates/media-win/src/lib.rs`

- [ ] **Step 1: Create `crates/media-win/src/error.rs`**

```rust
//! Error surface for media-win. Wraps both windows HRESULT and higher-level
//! semantic failures (e.g. "no adapter found").

use windows::core::HRESULT;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("D3D11: {context}: HRESULT 0x{hresult:08x}")]
    D3D11 { context: &'static str, hresult: u32 },

    #[error("DXGI: {context}: HRESULT 0x{hresult:08x}")]
    Dxgi { context: &'static str, hresult: u32 },

    #[error("no suitable adapter found (requested: {requested})")]
    NoAdapter { requested: String },

    #[error("adapter index out of range: {index} (have {count})")]
    AdapterOutOfRange { index: u32, count: u32 },

    #[error("unsupported pixel format: {fmt}")]
    UnsupportedFormat { fmt: &'static str },

    #[error("MMCSS task registration failed: {reason}")]
    MmcssFailed { reason: String },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("other: {0}")]
    Other(String),
}

impl MediaError {
    /// Helper for wrapping a windows::core::Error from a D3D11 call.
    pub fn d3d11(context: &'static str, err: windows::core::Error) -> Self {
        Self::D3D11 { context, hresult: err.code().0 as u32 }
    }

    /// Helper for wrapping a windows::core::Error from a DXGI call.
    pub fn dxgi(context: &'static str, err: windows::core::Error) -> Self {
        Self::Dxgi { context, hresult: err.code().0 as u32 }
    }

    /// Return the HRESULT if this is an HRESULT-bearing variant.
    pub fn hresult(&self) -> Option<HRESULT> {
        match self {
            Self::D3D11 { hresult, .. } | Self::Dxgi { hresult, .. } => {
                Some(HRESULT(*hresult as i32))
            }
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, MediaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        let e = MediaError::NoAdapter { requested: "nvidia".into() };
        assert_eq!(e.to_string(), "no suitable adapter found (requested: nvidia)");

        let e = MediaError::D3D11 { context: "CreateDevice", hresult: 0x887A0005 };
        assert_eq!(
            e.to_string(),
            "D3D11: CreateDevice: HRESULT 0x887a0005"
        );
    }

    #[test]
    fn hresult_roundtrip() {
        let e = MediaError::Dxgi { context: "EnumAdapters", hresult: 0x887A0002 };
        assert_eq!(e.hresult().unwrap().0 as u32, 0x887A0002);

        let e = MediaError::NoAdapter { requested: "any".into() };
        assert!(e.hresult().is_none());
    }
}
```

- [ ] **Step 2: Replace `crates/media-win/src/lib.rs`**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod error;

pub use error::{MediaError, Result};
```

- [ ] **Step 3: テスト**

```bash
cargo test -p prdt-media-win
```
Expected: **2 tests passed**

- [ ] **Step 4: Commit**

```bash
git add crates/media-win/src/
git commit -m "feat(media-win): add MediaError type with HRESULT wrapping"
```

---

### Task 3: Adapter 列挙

**Files:**
- Create: `crates/media-win/src/adapter.rs`
- Modify: `crates/media-win/src/lib.rs`

- [ ] **Step 1: Create `crates/media-win/src/adapter.rs`**

```rust
//! DXGI adapter enumeration. Provides a safe, ergonomic view of available
//! GPU adapters so callers can pick by vendor name or index.

use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1,
    DXGI_ADAPTER_FLAG_SOFTWARE,
};

use crate::error::{MediaError, Result};

/// Describes one DXGI adapter at enumeration time.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub index: u32,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory_bytes: u64,
    pub is_software: bool,
}

impl AdapterInfo {
    pub fn is_nvidia(&self) -> bool {
        self.vendor_id == 0x10DE
    }
    pub fn is_amd(&self) -> bool {
        self.vendor_id == 0x1002
    }
    pub fn is_intel(&self) -> bool {
        self.vendor_id == 0x8086
    }
}

/// Enumerate all DXGI 1.1+ adapters on this system.
pub fn enumerate_adapters() -> Result<Vec<AdapterInfo>> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?;

        let mut out = Vec::new();
        let mut i: u32 = 0;
        loop {
            let adapter_res: windows::core::Result<IDXGIAdapter1> = factory.EnumAdapters1(i);
            let adapter = match adapter_res {
                Ok(a) => a,
                Err(e) if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(MediaError::dxgi("EnumAdapters1", e)),
            };
            let mut desc = DXGI_ADAPTER_DESC1::default();
            adapter
                .GetDesc1(&mut desc)
                .map_err(|e| MediaError::dxgi("GetDesc1", e))?;

            let name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(0)],
            );

            out.push(AdapterInfo {
                index: i,
                name,
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                dedicated_video_memory_bytes: desc.DedicatedVideoMemory as u64,
                is_software: (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0,
            });
            let _ = adapter.as_raw(); // keep variable alive to silence warning
            i += 1;
        }
        Ok(out)
    }
}

/// Pick a specific adapter by index. Fails if index is out of range.
pub fn pick_adapter_by_index(index: u32) -> Result<AdapterInfo> {
    let all = enumerate_adapters()?;
    if (index as usize) >= all.len() {
        return Err(MediaError::AdapterOutOfRange {
            index,
            count: all.len() as u32,
        });
    }
    Ok(all[index as usize].clone())
}

/// Pick the default adapter: prefer the first non-software adapter.
pub fn pick_default_adapter() -> Result<AdapterInfo> {
    let all = enumerate_adapters()?;
    all.iter()
        .find(|a| !a.is_software)
        .cloned()
        .or_else(|| all.first().cloned())
        .ok_or_else(|| MediaError::NoAdapter {
            requested: "default".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_runs() {
        // On any Windows dev machine there is at least a software (WARP)
        // adapter available, so enumerate_adapters() should return at
        // least one entry without error.
        let adapters = enumerate_adapters().expect("DXGI enumerate should work");
        assert!(!adapters.is_empty(), "expected >= 1 adapter");
        // Smoke: every entry has a non-zero vendor and a non-empty name.
        for a in &adapters {
            assert!(!a.name.is_empty(), "adapter name empty at idx {}", a.index);
        }
    }

    #[test]
    fn default_adapter_resolves() {
        let a = pick_default_adapter().expect("at least one adapter");
        assert!(!a.name.is_empty());
    }

    #[test]
    fn out_of_range_index_errors() {
        let err = pick_adapter_by_index(9999).unwrap_err();
        matches!(err, MediaError::AdapterOutOfRange { index: 9999, .. });
    }
}
```

- [ ] **Step 2: Update `crates/media-win/src/lib.rs`**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod error;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use error::{MediaError, Result};
```

- [ ] **Step 3: Run tests (requires Windows + DXGI runtime)**

```bash
cargo test -p prdt-media-win
```
Expected: **5 tests passed** (2 error + 3 adapter). If on a system without DXGI (e.g., WSL), tests may fail with runtime errors — that's expected; note it as a concern but the code is correct.

- [ ] **Step 4: Commit**

```bash
git add crates/media-win/
git commit -m "feat(media-win): enumerate DXGI adapters"
```

---

### Task 4: D3D11 Device 作成(モジュール構造)

**Files:**
- Create: `crates/media-win/src/d3d11/mod.rs`
- Create: `crates/media-win/src/d3d11/device.rs`
- Modify: `crates/media-win/src/lib.rs`

- [ ] **Step 1: Create `crates/media-win/src/d3d11/mod.rs`**

```rust
//! D3D11 device management and texture utilities.

pub mod device;
pub mod texture;

pub use device::D3d11Device;
pub use texture::{D3d11Texture, TextureFormat};
```

- [ ] **Step 2: Create `crates/media-win/src/d3d11/device.rs`**

```rust
//! Safe wrapper around ID3D11Device + ID3D11DeviceContext.
//!
//! The device handle itself is free-threaded. The immediate context is
//! single-threaded-only; we gate access behind a Mutex. For the hot path
//! (encode/decode), production code can use deferred contexts or bypass
//! this wrapper.

use std::sync::{Arc, Mutex};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

use crate::adapter::AdapterInfo;
use crate::error::{MediaError, Result};

/// Safe handle to an ID3D11Device + its immediate context.
///
/// Clone-cheap (Arc-wrapped). The immediate context is guarded by a Mutex
/// because D3D11 does NOT allow concurrent use of the same immediate context.
#[derive(Clone)]
pub struct D3d11Device {
    inner: Arc<D3d11Inner>,
}

struct D3d11Inner {
    device: ID3D11Device,
    context: Mutex<ID3D11DeviceContext>,
    feature_level: D3D_FEATURE_LEVEL,
    adapter: AdapterInfo,
}

impl D3d11Device {
    /// Create a D3D11 device on the specified DXGI adapter.
    pub fn create(adapter: &AdapterInfo) -> Result<Self> {
        // Re-enumerate to obtain IDXGIAdapter (we don't cache COM pointers).
        let factory = unsafe {
            windows::Win32::Graphics::Dxgi::CreateDXGIFactory1::<
                windows::Win32::Graphics::Dxgi::IDXGIFactory1,
            >()
            .map_err(|e| MediaError::dxgi("CreateDXGIFactory1", e))?
        };
        let dxgi_adapter: IDXGIAdapter = unsafe {
            factory
                .EnumAdapters1(adapter.index)
                .map_err(|e| MediaError::dxgi("EnumAdapters1", e))?
                .cast()
                .map_err(|e| MediaError::dxgi("IDXGIAdapter cast", e))?
        };

        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut out_device: Option<ID3D11Device> = None;
        let mut out_context: Option<ID3D11DeviceContext> = None;
        let mut out_feature_level = D3D_FEATURE_LEVEL::default();

        let flags: D3D11_CREATE_DEVICE_FLAG = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

        unsafe {
            D3D11CreateDevice(
                &dxgi_adapter,
                D3D_DRIVER_TYPE_UNKNOWN, // required when passing a specific adapter
                windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut out_device),
                Some(&mut out_feature_level),
                Some(&mut out_context),
            )
            .map_err(|e| MediaError::d3d11("D3D11CreateDevice", e))?;
        }

        let device = out_device.ok_or_else(|| MediaError::D3D11 {
            context: "D3D11CreateDevice returned null device",
            hresult: 0,
        })?;
        let context = out_context.ok_or_else(|| MediaError::D3D11 {
            context: "D3D11CreateDevice returned null context",
            hresult: 0,
        })?;

        let _ = D3D_DRIVER_TYPE_HARDWARE; // silence unused import lint on some feature combos

        Ok(Self {
            inner: Arc::new(D3d11Inner {
                device,
                context: Mutex::new(context),
                feature_level: out_feature_level,
                adapter: adapter.clone(),
            }),
        })
    }

    /// Create on the default adapter (first non-software).
    pub fn create_default() -> Result<Self> {
        let adapter = crate::adapter::pick_default_adapter()?;
        Self::create(&adapter)
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.inner.device
    }

    pub fn adapter(&self) -> &AdapterInfo {
        &self.inner.adapter
    }

    pub fn feature_level(&self) -> D3D_FEATURE_LEVEL {
        self.inner.feature_level
    }

    /// Run a closure with the locked immediate context. Releases the mutex
    /// when the closure returns.
    pub fn with_context<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ID3D11DeviceContext) -> R,
    {
        let guard = self.inner.context.lock().expect("D3D11 context mutex poisoned");
        f(&guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_default_device() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        assert!(!dev.adapter().name.is_empty());
        // Non-zero feature level.
        assert!(dev.feature_level().0 > 0);
    }

    #[test]
    fn device_is_cloneable_and_shares_inner() {
        let a = D3d11Device::create_default().expect("D3D11 device");
        let b = a.clone();
        // Same underlying Arc.
        assert_eq!(Arc::strong_count(&a.inner), 2);
        drop(b);
        assert_eq!(Arc::strong_count(&a.inner), 1);
    }
}
```

- [ ] **Step 3: Update `crates/media-win/src/lib.rs`**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod error;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, TextureFormat};
pub use error::{MediaError, Result};
```

Note: `D3d11Texture` and `TextureFormat` are declared but not yet created; Task 5 creates the file. You can add this `pub use` now OR wait until Task 5. If Rust errors on undefined symbols, wait until after Task 5.

Recommendation: delete the `D3d11Texture, TextureFormat` parts from the `pub use d3d11::` line for now, add them in Task 5.

So for now use:
```rust
pub use d3d11::D3d11Device;
```

- [ ] **Step 4: Test**

```bash
cargo test -p prdt-media-win
```
Expected: **7 tests** (2 error + 3 adapter + 2 device). The device tests require a working GPU + D3D11 runtime; on a CI machine without GPU they will fail but that's expected. On your dev machine they should pass.

- [ ] **Step 5: Commit**

```bash
git add crates/media-win/
git commit -m "feat(media-win): add D3d11Device with adapter selection"
```

---

### Task 5: D3D11 Texture ラッパ + ピクセルフォーマット

**Files:**
- Create: `crates/media-win/src/d3d11/texture.rs`
- Modify: `crates/media-win/src/d3d11/mod.rs` (already points to texture, just ensure it's fine)
- Modify: `crates/media-win/src/lib.rs` (add texture exports)

- [ ] **Step 1: Create `crates/media-win/src/d3d11/texture.rs`**

```rust
//! Safe wrapper around ID3D11Texture2D with helpers for common operations:
//! staging-buffer readback, creation by explicit desc, format enum.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};

use crate::d3d11::device::D3d11Device;
use crate::error::{MediaError, Result};

/// Pixel formats supported by the media-win pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// 8-bit BGRA, post-DXGI-capture default.
    Bgra8,
    /// 8-bit RGBA (used by some tooling paths).
    Rgba8,
    /// NV12 (Y plane + interleaved UV half-res) — the NVDEC default output.
    Nv12,
}

impl TextureFormat {
    pub fn to_dxgi(self) -> windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT {
        match self {
            Self::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
            Self::Rgba8 => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Nv12 => DXGI_FORMAT_NV12,
        }
    }

    /// Bytes per pixel when the format is interleaved (YUV planar formats
    /// like NV12 return the Y-plane byte rate; callers that need full size
    /// must account for the UV plane separately).
    pub fn bytes_per_pixel_y(self) -> usize {
        match self {
            Self::Bgra8 | Self::Rgba8 => 4,
            Self::Nv12 => 1, // Y plane; UV is interleaved at half-res per dim
        }
    }
}

/// A 2D texture on the GPU.
#[derive(Clone)]
pub struct D3d11Texture {
    inner: ID3D11Texture2D,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl D3d11Texture {
    /// Create a fresh DEFAULT-usage texture ready for render target / shader
    /// resource binding.
    pub fn new_default(dev: &D3d11Device, width: u32, height: u32, fmt: TextureFormat) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a SHARED texture (for NVENC input, Phase 2b).
    pub fn new_shared_for_encoder(
        dev: &D3d11Device,
        width: u32,
        height: u32,
        fmt: TextureFormat,
    ) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a STAGING texture for CPU readback.
    pub fn new_staging(dev: &D3d11Device, width: u32, height: u32, fmt: TextureFormat) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt.to_dxgi(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        Self::new_with_desc(dev, desc, fmt, None)
    }

    /// Create a texture from an arbitrary desc. Internal.
    fn new_with_desc(
        dev: &D3d11Device,
        desc: D3D11_TEXTURE2D_DESC,
        fmt: TextureFormat,
        init: Option<&D3D11_SUBRESOURCE_DATA>,
    ) -> Result<Self> {
        let mut out: Option<ID3D11Texture2D> = None;
        unsafe {
            dev.device()
                .CreateTexture2D(&desc, init.map(|d| d as *const _), Some(&mut out))
                .map_err(|e| MediaError::d3d11("CreateTexture2D", e))?;
        }
        let inner = out.ok_or_else(|| MediaError::D3D11 {
            context: "CreateTexture2D returned null",
            hresult: 0,
        })?;
        Ok(Self {
            inner,
            width: desc.Width,
            height: desc.Height,
            format: fmt,
        })
    }

    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }
    pub fn format(&self) -> TextureFormat { self.format }

    pub fn raw(&self) -> &ID3D11Texture2D {
        &self.inner
    }

    /// Copy this texture to a staging texture, map it, and return a CPU-side
    /// Vec<u8> containing the pixel bytes (row-major, tight packing for BGRA/RGBA).
    ///
    /// For NV12 this returns the Y plane only; UV plane read is a separate
    /// helper in NVDEC-oriented code in Plan 2c.
    pub fn read_back_bgra_or_rgba(&self, dev: &D3d11Device) -> Result<Vec<u8>> {
        if !matches!(self.format, TextureFormat::Bgra8 | TextureFormat::Rgba8) {
            return Err(MediaError::UnsupportedFormat {
                fmt: "read_back_bgra_or_rgba requires BGRA8 or RGBA8",
            });
        }
        let staging = Self::new_staging(dev, self.width, self.height, self.format)?;

        // Copy GPU -> staging. Needs immediate context.
        dev.with_context(|ctx| unsafe {
            ctx.CopyResource(&staging.inner, &self.inner);
        });

        // Map and copy to Vec<u8>.
        let bytes_per_pixel = self.format.bytes_per_pixel_y();
        let target_row_bytes = (self.width as usize) * bytes_per_pixel;
        let mut out = vec![0u8; target_row_bytes * self.height as usize];

        dev.with_context(|ctx| -> Result<()> {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                ctx.Map(&staging.inner, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|e| MediaError::d3d11("Map staging texture", e))?;
            }
            let row_pitch = mapped.RowPitch as usize;
            unsafe {
                for y in 0..self.height as usize {
                    let src_row = (mapped.pData as *const u8).add(y * row_pitch);
                    let dst_row = out.as_mut_ptr().add(y * target_row_bytes);
                    std::ptr::copy_nonoverlapping(src_row, dst_row, target_row_bytes);
                }
                ctx.Unmap(&staging.inner, 0);
            }
            Ok(())
        })?;

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_conversions() {
        assert_eq!(TextureFormat::Bgra8.bytes_per_pixel_y(), 4);
        assert_eq!(TextureFormat::Rgba8.bytes_per_pixel_y(), 4);
        assert_eq!(TextureFormat::Nv12.bytes_per_pixel_y(), 1);

        assert_eq!(
            TextureFormat::Bgra8.to_dxgi(),
            DXGI_FORMAT_B8G8R8A8_UNORM
        );
    }

    #[test]
    fn create_default_texture() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = D3d11Texture::new_default(&dev, 256, 256, TextureFormat::Bgra8)
            .expect("create texture");
        assert_eq!(tex.width(), 256);
        assert_eq!(tex.height(), 256);
        assert_eq!(tex.format(), TextureFormat::Bgra8);
    }

    #[test]
    fn create_staging_texture() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = D3d11Texture::new_staging(&dev, 64, 64, TextureFormat::Bgra8)
            .expect("create staging texture");
        assert_eq!(tex.width(), 64);
    }
}
```

- [ ] **Step 2: Update `crates/media-win/src/lib.rs` to export texture types**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod error;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, TextureFormat};
pub use error::{MediaError, Result};
```

- [ ] **Step 3: Test**

```bash
cargo test -p prdt-media-win
```
Expected: **10 tests** (7 prior + 3 new).

- [ ] **Step 4: Commit**

```bash
git add crates/media-win/
git commit -m "feat(media-win): add D3d11Texture wrapper with staging readback"
```

---

### Task 6: Synthetic テクスチャ生成(テスト/PoC 用)

**Files:**
- Create: `crates/media-win/src/synthetic.rs`
- Modify: `crates/media-win/src/lib.rs`

- [ ] **Step 1: Create `crates/media-win/src/synthetic.rs`**

```rust
//! Synthetic texture generators for tests and the Phase 0 latency-bench.
//! Produces CPU-side pixel buffers that can be uploaded to a D3d11Texture.

use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_SUBRESOURCE_DATA};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

use crate::d3d11::{D3d11Device, D3d11Texture, TextureFormat};
use crate::error::{MediaError, Result};

/// Generate a BGRA solid-color buffer.
pub fn solid_bgra(width: u32, height: u32, b: u8, g: u8, r: u8, a: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        buf.extend_from_slice(&[b, g, r, a]);
    }
    buf
}

/// Generate a BGRA texture with a "counter" encoded as a small 16x16 pixel
/// block at top-left. The counter value is spread across 4 channels of 16
/// pixels each — useful for measuring end-to-end frame delivery in tests.
///
/// Layout:
/// ```text
/// pixels[0..16]    = frame_seq byte 0 (low)
/// pixels[16..32]   = frame_seq byte 1
/// pixels[32..48]   = frame_seq byte 2
/// pixels[48..64]   = frame_seq byte 3
/// (frame_seq is truncated to u32; rest of image is solid background)
/// ```
pub fn bgra_with_counter(width: u32, height: u32, frame_seq: u32, bg: (u8, u8, u8)) -> Vec<u8> {
    let mut buf = solid_bgra(width, height, bg.0, bg.1, bg.2, 0xFF);
    // First 64 pixels encode the frame counter in 4 bytes of 16 pixels each.
    let bytes = frame_seq.to_le_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        for p in 0..16 {
            let pixel_index = i * 16 + p;
            let offset = pixel_index * 4;
            buf[offset]     = byte; // B
            buf[offset + 1] = byte; // G
            buf[offset + 2] = byte; // R
            buf[offset + 3] = 0xFF; // A
        }
    }
    buf
}

/// Decode a frame counter from a BGRA buffer produced by
/// `bgra_with_counter`. Returns `None` if the counter pixels disagree
/// (indicating the texture got corrupted).
pub fn decode_counter_bgra(buf: &[u8]) -> Option<u32> {
    if buf.len() < 64 * 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for i in 0..4 {
        // Take the B channel of the first pixel in each 16-pixel group as
        // the canonical byte; verify the rest of the group agrees.
        let canonical = buf[i * 16 * 4]; // B of first pixel in group
        for p in 1..16 {
            let offset = (i * 16 + p) * 4;
            if buf[offset] != canonical {
                return None;
            }
        }
        bytes[i] = canonical;
    }
    Some(u32::from_le_bytes(bytes))
}

/// Create a D3D11 texture populated with a BGRA counter pattern.
///
/// Uploads the CPU-side buffer to a new DEFAULT-usage D3D11 texture using
/// `CreateTexture2D` with initial data.
pub fn make_counter_texture(
    dev: &D3d11Device,
    width: u32,
    height: u32,
    frame_seq: u32,
) -> Result<D3d11Texture> {
    let pixels = bgra_with_counter(width, height, frame_seq, (0x33, 0x66, 0x99));
    let row_pitch = (width * 4) as u32;

    // We have to construct the texture manually because `new_default`
    // doesn't support initial data. Mirror new_default's behavior.
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let initial = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr() as *const _,
        SysMemPitch: row_pitch,
        SysMemSlicePitch: 0,
    };

    let mut out: Option<ID3D11Texture2D> = None;
    unsafe {
        dev.device()
            .CreateTexture2D(&desc, Some(&initial), Some(&mut out))
            .map_err(|e| MediaError::d3d11("CreateTexture2D with initial data", e))?;
    }
    let inner = out.ok_or_else(|| MediaError::D3D11 {
        context: "CreateTexture2D returned null (synthetic)",
        hresult: 0,
    })?;

    // Wrap in D3d11Texture via a minimal constructor — since D3d11Texture
    // doesn't publicly expose one, we go through the texture module.
    // For now, we construct via a private helper below.
    Ok(D3d11Texture::from_raw(inner, width, height, TextureFormat::Bgra8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_round_trip_through_buffer() {
        for seq in [0u32, 1, 256, 65536, u32::MAX] {
            let buf = bgra_with_counter(64, 64, seq, (10, 20, 30));
            let back = decode_counter_bgra(&buf).expect("decode");
            assert_eq!(back, seq);
        }
    }

    #[test]
    fn solid_bgra_size_correct() {
        let buf = solid_bgra(10, 5, 1, 2, 3, 255);
        assert_eq!(buf.len(), 10 * 5 * 4);
        assert_eq!(buf[0..4], [1, 2, 3, 255]);
    }

    #[test]
    fn counter_texture_survives_gpu_round_trip() {
        let dev = D3d11Device::create_default().expect("D3D11 device");
        let tex = make_counter_texture(&dev, 128, 128, 42).expect("create counter tex");
        assert_eq!(tex.width(), 128);
        let buf = tex.read_back_bgra_or_rgba(&dev).expect("readback");
        let decoded = decode_counter_bgra(&buf).expect("decode counter");
        assert_eq!(decoded, 42);
    }
}
```

- [ ] **Step 2: Add a package-private constructor to D3d11Texture**

Edit `crates/media-win/src/d3d11/texture.rs`: add this inside `impl D3d11Texture`:

```rust
    /// Package-private helper for constructing from a raw ID3D11Texture2D.
    /// Used by `synthetic::make_counter_texture` and planned Plan 2b DXGI
    /// capture wrapper. Not public API.
    pub(crate) fn from_raw(
        inner: ID3D11Texture2D,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self { inner, width, height, format }
    }
```

- [ ] **Step 3: Update `crates/media-win/src/lib.rs`**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod error;
pub mod synthetic;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, TextureFormat};
pub use error::{MediaError, Result};
```

- [ ] **Step 4: Test**

```bash
cargo test -p prdt-media-win
```
Expected: **13 tests** (10 prior + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/media-win/
git commit -m "feat(media-win): add synthetic counter-pattern texture generator"
```

---

### Task 7: MMCSS ("Games") スレッドブースト

**Files:**
- Create: `crates/media-win/src/platform.rs`
- Modify: `crates/media-win/src/lib.rs`

**Spec ref:** §3.1, §3.2, §3.3 (MMCSS "Games" for capture/render threads).

- [ ] **Step 1: Create `crates/media-win/src/platform.rs`**

```rust
//! Windows MMCSS (Multimedia Class Scheduler Service) helpers.
//!
//! MMCSS boosts thread scheduling priority for multimedia tasks (video,
//! audio, gaming). We use the "Games" task for capture and render threads
//! per spec §3.1.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW};

use crate::error::{MediaError, Result};

/// RAII handle for an MMCSS task registration. Drops restore default
/// scheduling for the thread.
pub struct MmcssScope {
    handle: HANDLE,
}

impl MmcssScope {
    /// Register the current thread with the MMCSS "Games" task.
    pub fn games() -> Result<Self> {
        Self::with_task(w!("Games"))
    }

    /// Register with an arbitrary MMCSS task name. See
    /// https://learn.microsoft.com/en-us/windows/win32/procthread/multimedia-class-scheduler-service
    /// for the standard names ("Audio", "Capture", "Games", "Playback",
    /// "Pro Audio").
    pub fn with_task(task: PCWSTR) -> Result<Self> {
        let mut task_index: u32 = 0;
        let handle = unsafe {
            AvSetMmThreadCharacteristicsW(task, &mut task_index).map_err(|e| {
                MediaError::MmcssFailed {
                    reason: format!("AvSetMmThreadCharacteristicsW: {e}"),
                }
            })?
        };
        tracing::debug!(task_index, "MMCSS task attached");
        Ok(Self { handle })
    }
}

impl Drop for MmcssScope {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_task_attaches_and_detaches() {
        // On most dev machines MMCSS is available. On headless CI without
        // the audiosrv/MMCSS service it may fail; we treat that as a warn
        // and pass.
        match MmcssScope::games() {
            Ok(scope) => {
                drop(scope);
            }
            Err(MediaError::MmcssFailed { reason }) => {
                eprintln!("MMCSS unavailable on this machine: {reason}");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
```

- [ ] **Step 2: Update `crates/media-win/src/lib.rs`**

```rust
//! Windows media pipeline (DXGI / NVENC / NVDEC / D3D11).
//! Implemented across Phase 0 Plans 2a / 2b / 2c.

#![cfg(windows)]

pub mod adapter;
pub mod d3d11;
pub mod error;
pub mod platform;
pub mod synthetic;

pub use adapter::{enumerate_adapters, pick_adapter_by_index, pick_default_adapter, AdapterInfo};
pub use d3d11::{D3d11Device, D3d11Texture, TextureFormat};
pub use error::{MediaError, Result};
pub use platform::MmcssScope;
```

- [ ] **Step 3: Test**

```bash
cargo test -p prdt-media-win
```
Expected: **14 tests** (13 prior + 1 new).

- [ ] **Step 4: Commit**

```bash
git add crates/media-win/
git commit -m "feat(media-win): add MMCSS 'Games' thread priority scope"
```

---

### Task 8: Integration smoke test(実 GPU が必要、CI では ignore)

**Files:**
- Create: `crates/media-win/tests/smoke.rs`

- [ ] **Step 1: Create `crates/media-win/tests/smoke.rs`**

```rust
//! GPU integration smoke tests. These require a working D3D11 device and
//! will fail on headless CI without a GPU. We do NOT mark them `#[ignore]`
//! by default — the dev machine must pass them — but Plan 2a tasks document
//! this explicitly so CI fails loudly if ever run on a non-GPU runner.

#![cfg(windows)]

use prdt_media_win::{
    synthetic::{bgra_with_counter, decode_counter_bgra, make_counter_texture},
    D3d11Device, D3d11Texture, TextureFormat, MmcssScope,
};

#[test]
fn full_round_trip_counter_through_gpu() {
    let dev = D3d11Device::create_default().expect("D3D11 device");
    for seq in [0u32, 1, 123, 999_999, u32::MAX] {
        let tex = make_counter_texture(&dev, 256, 144, seq).expect("make texture");
        let buf = tex.read_back_bgra_or_rgba(&dev).expect("readback");
        let decoded = decode_counter_bgra(&buf).expect("decode counter");
        assert_eq!(decoded, seq);
    }
}

#[test]
fn default_texture_creation_all_formats() {
    let dev = D3d11Device::create_default().expect("D3D11 device");
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Bgra8).unwrap();
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Rgba8).unwrap();
    // NV12 often has strict size alignment (even dims). Ensure a 64x64 case works.
    let _ = D3d11Texture::new_default(&dev, 64, 64, TextureFormat::Nv12).unwrap();
}

#[test]
fn mmcss_games_under_task() {
    // Attach MMCSS, do some GPU work, detach. This is a sanity check that
    // MMCSS registration doesn't interact badly with D3D11 calls.
    let _scope = MmcssScope::games().expect("MMCSS games");
    let dev = D3d11Device::create_default().expect("D3D11 device");
    let buf = bgra_with_counter(128, 128, 42, (0, 0, 0));
    assert!(buf.len() > 0);
    let tex = make_counter_texture(&dev, 128, 128, 7).expect("texture");
    let back = tex.read_back_bgra_or_rgba(&dev).unwrap();
    assert_eq!(decode_counter_bgra(&back), Some(7));
}
```

- [ ] **Step 2: Test**

```bash
cargo test -p prdt-media-win
```
Expected: **17 tests total** (14 lib + 3 smoke).

- [ ] **Step 3: Commit**

```bash
git add crates/media-win/
git commit -m "test(media-win): add GPU integration smoke tests"
```

---

### Task 9: CI 更新 — `media-win` をビルドフラグチェックに追加

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Update `.github/workflows/ci.yml`**

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
      - name: clippy (protocol, transport)
        run: cargo clippy -p prdt-protocol -p prdt-transport --all-targets -- -D warnings
      - name: clippy (media-win lib only, no GPU tests)
        run: cargo clippy -p prdt-media-win --lib -- -D warnings
      - name: test (protocol, transport)
        run: cargo test -p prdt-protocol -p prdt-transport --all-targets
      - name: build
        run: cargo build --workspace
```

Note: CI does NOT run `media-win` tests because `windows-latest` runners don't have a GPU capable of D3D11 Feature Level 11.x in a way DXGI exposes reliably. Only the lib builds and clippy clean.

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add media-win clippy check (lib only, no GPU tests)"
```

---

### Task 10: 最終チェック + README + タグ

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Full local checks**

```bash
cargo test -p prdt-protocol -p prdt-transport -p prdt-media-win --all-targets
cargo clippy -p prdt-protocol -p prdt-transport -p prdt-media-win --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --workspace --release
```
Expected: all pass. Plan 2a test count: 51 (Plan 1) + 17 (Plan 2a) = **68 total**.

- [ ] **Step 2: Update `README.md`**

```markdown
# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [ ] Plan 2b: `media-win` DXGI capture + NVENC
- [ ] Plan 2c: `media-win` NVDEC + render + producer/consumer
- [ ] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria

## Building

Requires Rust stable (>= 1.78), Windows 11 + D3D11-capable GPU for Plan 2a+.

```
cargo test -p prdt-protocol -p prdt-transport
cargo run -p prdt-latency-bench --release -- --duration 2s
# On Windows with a GPU:
cargo test -p prdt-media-win
```
```

- [ ] **Step 3: Commit and tag**

```bash
git add README.md
git commit -m "docs: mark Phase 0 Plan 2a complete"
git tag phase0-plan2a-complete
```

- [ ] **Step 4: Verify**

```bash
git log --oneline phase0/plan2a-d3d11-foundation | head -15
git tag -l
```

---

## Plan 2a 完了判定

- [ ] Task 1〜10 の全ステップ完了
- [ ] `cargo test -p prdt-media-win` が 17 件 pass(GPU あり環境)
- [ ] `cargo clippy` 全クレートクリーン
- [ ] CI ymal に media-win lib-only clippy 追加済
- [ ] Commit 数 ~11 commits
- [ ] タグ `phase0-plan2a-complete` 作成

**次のステップ**: Plan 2b(DXGI Desktop Duplication + NVENC SDK bindgen + encoder wrapper)。

---

*End of Phase 0 — Plan 2a of 4.*
