# Windows MF H.265 Encoder Fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Media Foundation H.265 encoder MFT path so non-NVIDIA Windows GPUs (AMD / Intel) can run the host bin. Refactor existing `NvencEncoder` and the new `MfH265Encoder` behind a shared `Hevc265Encoder` trait. Producer dispatches via an enum; the host bin auto-selects based on adapter vendor and accepts an explicit `--encoder` override.

**Architecture:** Single new trait `Hevc265Encoder` in a new file `crates/media-win/src/encoder_trait.rs`. `EncodedH265Frame` moves there. `NvencEncoder` and `MfH265Encoder` both implement it. `HwHevcEncoder` enum dispatches at runtime. `DxgiNvencProducer` becomes `DxgiHevcProducer` (alias kept) holding the enum. Host CLI gets `--encoder {auto,nvenc,mf}`. `prdt-bench-matrix` gains `--encoders` axis. The MF encoder uses the OS H.265 encoder MFT (any GPU vendor) over a D3D11 device manager, with a BGRA→NV12 conversion shader for the input.

**Tech Stack:** Rust 2021, `windows` 0.58 (Media Foundation + D3D11VideoProcessor APIs already pulled in for the decoder side), existing `bytes`/`tracing`/`anyhow`/`async_trait`. No new workspace deps.

**Spec:** `docs/superpowers/specs/2026-04-26-mf-encoder-design.md`

---

## File Structure

**Created files:**

```
crates/media-win/src/
  encoder_trait.rs                  Hevc265Encoder trait + EncodedH265Frame
  d3d11/
    bgra_to_nv12.rs                 D3D11 VideoProcessor wrapper (BGRA8 -> NV12)
  mf/
    encoder.rs                      MfH265Encoder (MFT lifecycle + encode loop)

docs/
  encoders.md                       Auto-selection rules + --encoder flag + tradeoffs
```

**Modified files:**

```
crates/media-win/src/lib.rs         Re-export Hevc265Encoder, EncodedH265Frame,
                                    HwHevcEncoder, MfH265Encoder
crates/media-win/src/nvenc/
  encoder.rs                        Move EncodedH265Frame out, impl Hevc265Encoder
crates/media-win/src/mf/mod.rs      Add ensure_mf_runtime() pub(crate) helper,
                                    re-export MfH265Encoder
crates/media-win/src/mf/decoder.rs  Replace inline MFStartup/CoInitializeEx
                                    with ensure_mf_runtime()
crates/media-win/src/pipeline/
  producer.rs                       Wrap encoder in HwHevcEncoder enum;
                                    add new() taking pre-built encoder
crates/media-win/src/d3d11/mod.rs   Re-export bgra_to_nv12 module

crates/host/Cargo.toml               (no change)
crates/host/src/main.rs              Add EncoderChoice + --encoder flag,
                                     adapter-based auto select
crates/gui-common/src/config.rs     Add HostConfig::encoder field with serde default
crates/latency-bench/src/
  full_pipeline.rs                  Accept encoder backend; update FullPipelineConfig
  lib.rs                            Add EncoderBackend to MatrixAxes + ConfigStats
  bin/bench-matrix.rs               Add --encoders axis CLI

docs/superpowers/STATUS.md          Add tag row + update test count
```

---

## Verified API references

```rust
// Existing (DO NOT change signatures)
pub struct EncodedH265Frame {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp: u64,
}

// nvenc/encoder.rs:208 — current encode() takes &self; we change to &mut self
// to match the trait. The struct's interior fields are FFI pointers anyway, so
// no callers depend on encode-via-shared-ref semantics.
impl NvencEncoder {
    pub fn new(dev: &D3d11Device, cfg: &NvencEncoderConfig) -> Result<Self>;
    pub fn encode(&mut self, texture: &D3d11Texture, force_idr: bool, timestamp: u64) -> Result<EncodedH265Frame>;
}

// mf/decoder.rs:48-52 — MF init pattern (replicate via shared helper)
static MF_INIT: OnceLock<Option<String>> = OnceLock::new();
fn ensure_mf_started() -> Result<()> {
    if let Some(err) = MF_INIT.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    }) {
        return Err(MediaError::Other(err.clone()));
    }
    Ok(())
}
```

```rust
// New (introduced by this work)
pub trait Hevc265Encoder: Send {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError>;
    fn set_target_bitrate(&mut self, bps: u32);
    fn backend_name(&self) -> &'static str;
}

pub enum HwHevcEncoder {
    Nvenc(NvencEncoder),
    Mf(MfH265Encoder),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderChoice { Auto, Nvenc, Mf }
```

---

## Task 1: `Hevc265Encoder` trait + relocate `EncodedH265Frame` + NvencEncoder impl

**Files:**
- Create: `crates/media-win/src/encoder_trait.rs`
- Modify: `crates/media-win/src/lib.rs`
- Modify: `crates/media-win/src/nvenc/encoder.rs`

- [ ] **Step 1: Create the trait file**

Create `crates/media-win/src/encoder_trait.rs`:

```rust
//! Shared abstraction over Windows H.265 hardware encoders.
//!
//! Two implementations:
//! - `crate::nvenc::NvencEncoder` (NVIDIA HW only, lowest latency)
//! - `crate::mf::MfH265Encoder` (any DXGI adapter via Media Foundation MFT)
//!
//! Both produce Annex-B H.265 NAL units consumable by the existing
//! `MfD3d11Consumer` / `NvdecD3d11Consumer` decoders without any
//! transport-layer change.
//!
//! Future: a `Dx12Hevc265Encoder` trait taking `&D3d12Resource` will be
//! added when DX12 Video Encode is wired in. The two trait families
//! stay separate because D3D11 and D3D12 textures are not interchangeable.

use crate::d3d11::D3d11Texture;
use crate::error::MediaError;

/// One encoded H.265 access unit (Annex-B byte stream).
#[derive(Debug, Clone)]
pub struct EncodedH265Frame {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp: u64,
}

/// HW H.265 encoder operating on D3D11 input textures.
pub trait Hevc265Encoder: Send {
    /// Encode a `B8G8R8A8_UNORM` D3D11 texture into a single H.265
    /// access unit. `force_idr == true` requests an IDR + parameter
    /// sets at the next encode opportunity.
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError>;

    /// Best-effort target bitrate update (bits per second). The encoder
    /// may take effect on the next IDR or sooner depending on backend.
    fn set_target_bitrate(&mut self, bps: u32);

    /// Stable identifier for logs / bench output.
    fn backend_name(&self) -> &'static str;
}
```

- [ ] **Step 2: Re-export from lib.rs**

Edit `crates/media-win/src/lib.rs`. Find the existing `pub use nvenc::{EncodedH265Frame, NvEncLibrary, NvencEncoder, NvencEncoderConfig};` line. Replace with:

```rust
pub mod encoder_trait;
pub use encoder_trait::{EncodedH265Frame, Hevc265Encoder};
pub use nvenc::{NvEncLibrary, NvencEncoder, NvencEncoderConfig};
```

- [ ] **Step 3: Move `EncodedH265Frame` out of nvenc/encoder.rs**

Edit `crates/media-win/src/nvenc/encoder.rs`. Delete the existing definition at line 45-49:

```rust
pub struct EncodedH265Frame {
    pub nal_bytes: Vec<u8>,
    pub is_keyframe: bool,
    pub timestamp: u64,
}
```

Add at the top of the file (after existing imports), in the same import block as other crate imports:

```rust
use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder};
```

(Adjust to match the existing import grouping style; should be alongside `use crate::nvenc::ffi;` etc.)

- [ ] **Step 4: Change `encode` to take `&mut self`**

In `crates/media-win/src/nvenc/encoder.rs`, find the existing line 208:

```rust
    pub fn encode(
        &self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp: u64,
    ) -> Result<EncodedH265Frame> {
```

Replace `&self` with `&mut self`. The function body already mutates session state via FFI; the lifetime change is purely Rust-level.

- [ ] **Step 5: Implement the trait for NvencEncoder**

At the end of `crates/media-win/src/nvenc/encoder.rs` (after the existing `impl NvencEncoder` block and before any tests), add:

```rust
impl Hevc265Encoder for NvencEncoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        // Delegates to the inherent method.
        NvencEncoder::encode(self, texture, force_idr, timestamp_us)
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        // The current NVENC implementation does not yet support live
        // bitrate reconfiguration; record the requested value for the
        // next session restart. This matches the existing behaviour:
        // bitrate is set in `NvencEncoderConfig::bitrate_bps` at
        // construction time.
        tracing::warn!(
            target = "nvenc",
            requested_bps = bps,
            "set_target_bitrate is currently a no-op for NVENC \
             (rate-control reconfiguration is a follow-up)"
        );
    }

    fn backend_name(&self) -> &'static str {
        "nvenc"
    }
}
```

(If `NvencEncoder` already has a `set_target_bitrate` inherent method, replace the body of the trait method to call it. As of 2026-04-26 it does not — the warn! stub matches current behaviour.)

- [ ] **Step 6: Update producer call site**

In `crates/media-win/src/pipeline/producer.rs`, find the call site of
`encoder.encode(...)` (around lines 130–150 in the `next_frame` body).
The call is unchanged in shape; the only difference is that `&mut`
borrowing now applies. Since `producer.encoder` is already accessed
via `&mut self.encoder` implicitly from inside `&mut self` methods,
no syntactic change is needed. Just verify by build.

- [ ] **Step 7: Build + test**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build -p prdt-media-win 2>&1 | tail -3
cargo test -p prdt-media-win 2>&1 | tail -10
cargo clippy -p prdt-media-win --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clean build; all existing media-win tests pass; clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/media-win/src/encoder_trait.rs \
        crates/media-win/src/lib.rs \
        crates/media-win/src/nvenc/encoder.rs
git commit -m "media-win: extract Hevc265Encoder trait + EncodedH265Frame; impl for NvencEncoder"
```

---

## Task 2: `HwHevcEncoder` enum + producer wiring

**Files:**
- Modify: `crates/media-win/src/encoder_trait.rs` (add the enum)
- Modify: `crates/media-win/src/pipeline/producer.rs` (wrap encoder in enum)
- Modify: `crates/media-win/src/lib.rs` (re-export)

- [ ] **Step 1: Add the enum**

Edit `crates/media-win/src/encoder_trait.rs`. Append:

```rust
use crate::nvenc::NvencEncoder;
// MfH265Encoder import added in Task 4.

/// Runtime-dispatched HW H.265 encoder. Used by the producer layer so
/// the rest of the pipeline (transport, decoder selection, etc.) does
/// not care which backend is in use.
pub enum HwHevcEncoder {
    Nvenc(NvencEncoder),
    // Mf(MfH265Encoder),  // added in Task 4
}

impl Hevc265Encoder for HwHevcEncoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        match self {
            Self::Nvenc(e) => e.encode(texture, force_idr, timestamp_us),
        }
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            Self::Nvenc(e) => e.set_target_bitrate(bps),
        }
    }

    fn backend_name(&self) -> &'static str {
        match self {
            Self::Nvenc(e) => e.backend_name(),
        }
    }
}

impl From<NvencEncoder> for HwHevcEncoder {
    fn from(e: NvencEncoder) -> Self {
        Self::Nvenc(e)
    }
}
```

The Mf variant + match arms get added in Task 4. The compiler will be
happy with a one-variant enum for now.

- [ ] **Step 2: Re-export**

In `crates/media-win/src/lib.rs`, extend the existing re-export to include `HwHevcEncoder`:

```rust
pub use encoder_trait::{EncodedH265Frame, Hevc265Encoder, HwHevcEncoder};
```

- [ ] **Step 3: Wrap producer's encoder in the enum**

In `crates/media-win/src/pipeline/producer.rs`:

Replace `encoder: NvencEncoder,` (around line 17) with:

```rust
    encoder: HwHevcEncoder,
```

Update the import at the top of the file:

```rust
use crate::encoder_trait::{Hevc265Encoder, HwHevcEncoder};
use crate::nvenc::{NvencEncoder, NvencEncoderConfig};
```

In `DxgiNvencProducer::new`, change the assignment:

```rust
        let encoder = NvencEncoder::new(dev, &cfg)?;
```

to:

```rust
        let encoder: HwHevcEncoder = NvencEncoder::new(dev, &cfg)?.into();
```

Inside `next_frame()`'s body, find the existing `encoder.encode(...)` call. Calls go through the trait method now; syntax is identical (`encoder.encode(...)` resolves to the trait method via `HwHevcEncoder`'s `Hevc265Encoder` impl).

- [ ] **Step 4: Add a constructor that takes a pre-built encoder**

In `crates/media-win/src/pipeline/producer.rs`, after the existing `pub fn new(...)`, add a new constructor:

```rust
impl DxgiNvencProducer {
    /// Construct a producer with a pre-built encoder. Used by the host
    /// bin when it has chosen the backend explicitly (`--encoder mf`,
    /// etc.) so the producer layer doesn't need a vendor switch.
    pub fn with_encoder(
        dev: &D3d11Device,
        output: &OutputInfo,
        encoder: HwHevcEncoder,
    ) -> Result<Self, MediaError> {
        let dup = DesktopDuplication::new(dev, output)?;
        let width = dup.width();
        let height = dup.height();
        Ok(Self {
            dev: dev.clone(),
            output: output.clone(),
            dup,
            encoder,
            seq: 0,
            idr_pending: true,
            width,
            height,
        })
    }
}
```

Keep the existing `pub fn new(...)` as a convenience that builds an
NVENC encoder + delegates to `with_encoder`:

```rust
    pub fn new(
        dev: &D3d11Device,
        output: &OutputInfo,
        bitrate_bps: u32,
    ) -> Result<Self, MediaError> {
        let dup = DesktopDuplication::new(dev, output)?;
        let width = dup.width();
        let height = dup.height();
        let cfg = NvencEncoderConfig {
            width,
            height,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps,
            gop_length: 60,
        };
        let encoder: HwHevcEncoder = NvencEncoder::new(dev, &cfg)?.into();
        Self::with_encoder(dev, output, encoder)
    }
```

- [ ] **Step 5: Build + test**

```bash
cargo build -p prdt-media-win -p prdt-host 2>&1 | tail -5
cargo test -p prdt-media-win 2>&1 | tail -10
cargo clippy -p prdt-media-win -p prdt-host --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/media-win/src/encoder_trait.rs \
        crates/media-win/src/lib.rs \
        crates/media-win/src/pipeline/producer.rs
git commit -m "media-win: HwHevcEncoder enum + DxgiNvencProducer::with_encoder constructor"
```

---

## Task 3: MF runtime helper + BGRA→NV12 conversion

**Files:**
- Modify: `crates/media-win/src/mf/mod.rs` (add `ensure_mf_runtime`)
- Modify: `crates/media-win/src/mf/decoder.rs` (use the helper)
- Create: `crates/media-win/src/d3d11/bgra_to_nv12.rs`
- Modify: `crates/media-win/src/d3d11/mod.rs` (re-export)

- [ ] **Step 1: Extract MF runtime init**

Edit `crates/media-win/src/mf/mod.rs`. Replace its current content with:

```rust
//! Media Foundation-based H.265 decoder + encoder support.
//!
//! Both the decoder MFT and encoder MFT need a one-shot global init
//! sequence: `CoInitializeEx` + `MFStartup`. We share that via
//! `ensure_mf_runtime()` so each component does not duplicate it.

pub mod decoder;
pub mod encoder;

pub use decoder::H265Decoder;
pub use encoder::MfH265Encoder;

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::error::MediaError;

static MF_INIT: OnceLock<Option<String>> = OnceLock::new();

/// One-shot Media Foundation runtime init. Idempotent; subsequent calls
/// reuse the cached result. Returns `MediaError::Other` if `MFStartup`
/// failed on the first invocation.
pub(crate) fn ensure_mf_runtime() -> Result<(), MediaError> {
    let cached = MF_INIT.get_or_init(|| unsafe {
        // CoInitializeEx returns S_FALSE if COM is already initialised
        // on this thread; treat both Ok and S_FALSE as success.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    });
    if let Some(err) = cached {
        return Err(MediaError::Other(err.clone()));
    }
    Ok(())
}
```

(Note: `pub mod encoder;` will refer to a file we create in Task 4.
Build will fail until Task 4. **Skip Step 5 of this task until Task 4
lands.** Or, defer adding `pub mod encoder;` here until Task 4.
Cleaner: defer.)

Actually replace with the deferred form (no `encoder` reference yet):

```rust
//! Media Foundation H.265 decoder support. Encoder lives in
//! `mf::encoder` (added by Task 4 of mf-encoder-fallback).

pub mod decoder;
pub use decoder::H265Decoder;

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::error::MediaError;

static MF_INIT: OnceLock<Option<String>> = OnceLock::new();

pub(crate) fn ensure_mf_runtime() -> Result<(), MediaError> {
    let cached = MF_INIT.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    });
    if let Some(err) = cached {
        return Err(MediaError::Other(err.clone()));
    }
    Ok(())
}
```

- [ ] **Step 2: Use the helper in the decoder**

Edit `crates/media-win/src/mf/decoder.rs`. Find the existing `static`
block + lazy-init function around lines 44-60:

```rust
static MF_INIT: OnceLock<Option<String>> = OnceLock::new();

fn ensure_mf_started() -> Result<()> {
    if let Some(err) = MF_INIT.get_or_init(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        match MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            Ok(()) => None,
            Err(e) => Some(format!("MFStartup: {e}")),
        }
    }) {
        return Err(MediaError::Other(err.clone()));
    }
    Ok(())
}
```

Delete the static + function. Wherever the old `ensure_mf_started()`
was called, replace with `super::ensure_mf_runtime()`. Remove the now-unused
`OnceLock`, `MFStartup`, `CoInitializeEx`, `MF_VERSION`, `MFSTARTUP_FULL`,
`COINIT_MULTITHREADED` imports from this file.

- [ ] **Step 3: Build + test (decoder still works)**

```bash
cargo build -p prdt-media-win 2>&1 | tail -3
cargo test -p prdt-media-win 2>&1 | tail -10
```

Expected: clean. Any existing decoder tests continue to pass.

- [ ] **Step 4: Create BGRA → NV12 D3D11 converter**

Create `crates/media-win/src/d3d11/bgra_to_nv12.rs`:

```rust
//! D3D11 VideoProcessor wrapper that converts a B8G8R8A8_UNORM source
//! texture into an NV12 destination texture, both on the same D3D11
//! device. Used by the MF H.265 encoder when the encoder MFT only
//! accepts NV12 input (true for AMD/Intel/NVIDIA-driver MFTs).
//!
//! Performs colour space conversion BT.709 limited-range; sRGB output
//! semantics. Fully GPU; no CPU readback.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12};

use crate::d3d11::{D3d11Device, D3d11Texture};
use crate::error::MediaError;

pub struct BgraToNv12 {
    width: u32,
    height: u32,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl BgraToNv12 {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self, MediaError> {
        unsafe {
            let video_device: ID3D11VideoDevice =
                dev.raw().cast().map_err(|e| MediaError::Other(format!("ID3D11VideoDevice cast: {e}")))?;
            let video_context: ID3D11VideoContext = dev
                .immediate_context()
                .cast()
                .map_err(|e| MediaError::Other(format!("ID3D11VideoContext cast: {e}")))?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: width,
                InputHeight: height,
                OutputFrameRate: windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let mut enumerator: Option<ID3D11VideoProcessorEnumerator> = None;
            video_device
                .CreateVideoProcessorEnumerator(&content_desc, &mut enumerator)
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorEnumerator: {e}")))?;
            let enumerator = enumerator.ok_or_else(|| {
                MediaError::Other("CreateVideoProcessorEnumerator returned null".into())
            })?;

            let mut processor: Option<ID3D11VideoProcessor> = None;
            video_device
                .CreateVideoProcessor(&enumerator, 0, &mut processor)
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessor: {e}")))?;
            let processor = processor.ok_or_else(|| {
                MediaError::Other("CreateVideoProcessor returned null".into())
            })?;

            Ok(Self {
                width,
                height,
                video_device,
                video_context,
                enumerator,
                processor,
            })
        }
    }

    /// Allocate an NV12 D3D11 texture compatible as the output destination.
    pub fn allocate_nv12_output(&self, dev: &D3d11Device) -> Result<D3d11Texture, MediaError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.width,
            Height: self.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        D3d11Texture::create(dev, &desc)
    }

    /// Convert one frame: BGRA `src` → NV12 `dst`.
    pub fn convert(&self, src: &D3d11Texture, dst: &D3d11Texture) -> Result<(), MediaError> {
        unsafe {
            // Input view (BGRA shader resource)
            let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0, // use texture format
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    src.raw(),
                    &self.enumerator,
                    &in_view_desc,
                    &mut in_view,
                )
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorInputView: {e}")))?;
            let in_view =
                in_view.ok_or_else(|| MediaError::Other("input view null".into()))?;

            // Output view (NV12 render target)
            let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VPOV {
                        MipSlice: 0,
                    },
                },
            };
            let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_device
                .CreateVideoProcessorOutputView(
                    dst.raw(),
                    &self.enumerator,
                    &out_view_desc,
                    &mut out_view,
                )
                .map_err(|e| MediaError::Other(format!("CreateVideoProcessorOutputView: {e}")))?;
            let out_view =
                out_view.ok_or_else(|| MediaError::Other("output view null".into()))?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(in_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let streams = [stream];

            self.video_context
                .VideoProcessorBlt(&self.processor, &out_view, 0, &streams)
                .map_err(|e| MediaError::Other(format!("VideoProcessorBlt: {e}")))?;
        }
        Ok(())
    }
}

unsafe impl Send for BgraToNv12 {}
```

> **Note:** `D3d11Texture::create` and `D3d11Device::immediate_context()`
> are assumed to exist. If they don't with this exact signature,
> the implementer adapts to the actual API. Verify with:
> `grep -n "pub fn create\|pub fn immediate_context\|pub fn raw" crates/media-win/src/d3d11/`

- [ ] **Step 5: Re-export the converter**

In `crates/media-win/src/d3d11/mod.rs`, add the new module:

```rust
pub mod bgra_to_nv12;

pub use bgra_to_nv12::BgraToNv12;
```

(Place adjacent to existing `pub mod` declarations alphabetically.)

- [ ] **Step 6: Build**

```bash
cargo build -p prdt-media-win 2>&1 | tail -10
```

Expected: clean. If `D3d11Texture::create` does not exist with the
shape used above, the implementer adapts the call and re-runs. The
critical constraint: the resulting NV12 texture must have
`BindFlags = RENDER_TARGET | SHADER_RESOURCE` so the
`VideoProcessor` can write to it.

- [ ] **Step 7: Smoke test the converter**

Add a smoke test (manual run, NVIDIA dev box):

```bash
cargo test -p prdt-media-win bgra_to_nv12_smoke 2>&1 | tail -10
```

The plan does NOT include this test in committed source — adding a
real D3D11 device test requires GPU access. Document as a manual smoke
in the docs created in Task 6. Skip coding this test.

- [ ] **Step 8: Commit**

```bash
git add crates/media-win/src/mf/mod.rs \
        crates/media-win/src/mf/decoder.rs \
        crates/media-win/src/d3d11/bgra_to_nv12.rs \
        crates/media-win/src/d3d11/mod.rs
git commit -m "media-win: ensure_mf_runtime() helper + BgraToNv12 D3D11 VideoProcessor"
```

---

## Task 4: `MfH265Encoder` implementation

**Files:**
- Create: `crates/media-win/src/mf/encoder.rs`
- Modify: `crates/media-win/src/mf/mod.rs` (add `pub mod encoder;` + re-export)
- Modify: `crates/media-win/src/encoder_trait.rs` (add `Mf` enum variant)
- Modify: `crates/media-win/src/lib.rs` (re-export `MfH265Encoder`)

This is the largest task. The skeleton:

- MFT enumeration (find a hardware H.265 encoder MFT)
- Configure low-latency mode + bitrate via `CODECAPI_*` properties
- Set input type (NV12) + output type (HEVC)
- Bind D3D11 device manager
- ProcessInput per frame, ProcessOutput drains the encoded sample
- BgraToNv12 owned by encoder for the input conversion

- [ ] **Step 1: Skeleton with `new` and `Drop`**

Create `crates/media-win/src/mf/encoder.rs`:

```rust
//! Media Foundation H.265 encoder MFT wrapper. Provides a
//! `Hevc265Encoder` impl that takes a B8G8R8A8 D3D11 texture and emits
//! an Annex-B H.265 access unit on each call.
//!
//! Works on any DXGI adapter that exposes a hardware H.265 encoder MFT
//! (NVIDIA / AMD / Intel — driver-provided MFT). Falls back to a
//! software MFT if no hardware MFT is present (slow but functional).
//!
//! Internally:
//!   1. Capture produces BGRA D3D11 texture.
//!   2. `BgraToNv12::convert` produces an NV12 D3D11 texture.
//!   3. The NV12 texture wraps in an `IMFSample` via `MFCreateDXGISurfaceBuffer`.
//!   4. `IMFTransform::ProcessInput` queues the sample.
//!   5. `IMFTransform::ProcessOutput` drains the encoded `IMFSample`.
//!   6. The encoded buffer's bytes are an Annex-B HEVC NAL stream.

use std::sync::Arc;

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform,
    MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
    MFCreateSample, MFTEnumEx, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
    MFT_REGISTER_TYPE_INFO, MFVideoFormat_HEVC, MFVideoFormat_NV12,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_LOW_LATENCY, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_USER_DATA,
    MF_TRANSFORM_ASYNC_UNLOCK, MFMediaType_Video, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;

use crate::d3d11::{BgraToNv12, D3d11Device, D3d11Texture};
use crate::encoder_trait::{EncodedH265Frame, Hevc265Encoder};
use crate::error::MediaError;
use crate::nvenc::NvencEncoderConfig;

pub struct MfH265Encoder {
    transform: IMFTransform,
    device_manager: IMFDXGIDeviceManager,
    bgra_to_nv12: BgraToNv12,
    nv12_input: D3d11Texture,
    width: u32,
    height: u32,
    sample_seq: u64,
    pending_idr: bool,
}

impl MfH265Encoder {
    /// Construct an MF H.265 encoder bound to the given D3D11 device.
    /// Uses the OS-default hardware encoder MFT when one is available.
    /// `cfg` shares fields with NVENC: width, height, fps, bitrate.
    pub fn new(dev: &D3d11Device, cfg: &NvencEncoderConfig) -> Result<Self, MediaError> {
        super::ensure_mf_runtime()?;

        let transform = enumerate_h265_encoder_mft()?;
        let device_manager = create_dxgi_device_manager(dev)?;

        // Bind D3D11 device manager (MFT_MESSAGE_SET_D3D_MANAGER).
        unsafe {
            transform
                .ProcessMessage(
                    windows::Win32::Media::MediaFoundation::MFT_MESSAGE_SET_D3D_MANAGER,
                    device_manager.as_raw() as usize,
                )
                .map_err(|e| MediaError::Other(format!("MFT_MESSAGE_SET_D3D_MANAGER: {e}")))?;
        }

        configure_output_type(&transform, cfg)?;
        configure_input_type(&transform, cfg)?;
        set_low_latency(&transform)?;

        let bgra_to_nv12 = BgraToNv12::new(dev, cfg.width, cfg.height)?;
        let nv12_input = bgra_to_nv12.allocate_nv12_output(dev)?;

        unsafe {
            transform.ProcessMessage(
                windows::Win32::Media::MediaFoundation::MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
                0,
            ).map_err(|e| MediaError::Other(format!("BEGIN_STREAMING: {e}")))?;
            transform.ProcessMessage(
                windows::Win32::Media::MediaFoundation::MFT_MESSAGE_NOTIFY_START_OF_STREAM,
                0,
            ).map_err(|e| MediaError::Other(format!("START_OF_STREAM: {e}")))?;
        }

        Ok(Self {
            transform,
            device_manager,
            bgra_to_nv12,
            nv12_input,
            width: cfg.width,
            height: cfg.height,
            sample_seq: 0,
            pending_idr: true,
        })
    }
}

impl Drop for MfH265Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(
                windows::Win32::Media::MediaFoundation::MFT_MESSAGE_NOTIFY_END_OF_STREAM,
                0,
            );
            let _ = self.transform.ProcessMessage(
                windows::Win32::Media::MediaFoundation::MFT_MESSAGE_NOTIFY_END_STREAMING,
                0,
            );
        }
    }
}

unsafe impl Send for MfH265Encoder {}

// ====== Helper functions (called from new/encode) ==========================

fn enumerate_h265_encoder_mft() -> Result<IMFTransform, MediaError> {
    use windows::core::GUID;
    let category = MFT_CATEGORY_VIDEO_ENCODER;
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    let flags = (MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0) as u32;

    let mut p_activates: *mut Option<windows::Win32::Media::MediaFoundation::IMFActivate> =
        std::ptr::null_mut();
    let mut count: u32 = 0;
    unsafe {
        MFTEnumEx(
            category,
            flags,
            None,
            Some(&output_info),
            &mut p_activates,
            &mut count,
        )
        .map_err(|e| MediaError::Other(format!("MFTEnumEx: {e}")))?;

        if count == 0 {
            return Err(MediaError::Other(
                "no H.265 encoder MFT registered (HEVC Video Extensions installed? \
                 GPU driver provides one?)"
                    .into(),
            ));
        }
        let activates = std::slice::from_raw_parts(p_activates, count as usize);
        let activate = activates[0]
            .clone()
            .ok_or_else(|| MediaError::Other("first activate is None".into()))?;
        let transform: IMFTransform = activate
            .ActivateObject()
            .map_err(|e| MediaError::Other(format!("IMFActivate::ActivateObject: {e}")))?;
        // Free the array allocation. The COM objects in it are still owned by
        // the activates we cloned/moved out.
        windows::Win32::System::Com::CoTaskMemFree(Some(p_activates as *const _));
        Ok(transform)
    }
}

fn create_dxgi_device_manager(dev: &D3d11Device) -> Result<IMFDXGIDeviceManager, MediaError> {
    let mut reset_token: u32 = 0;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
            .map_err(|e| MediaError::Other(format!("MFCreateDXGIDeviceManager: {e}")))?;
        let manager = manager.ok_or_else(|| {
            MediaError::Other("MFCreateDXGIDeviceManager returned null".into())
        })?;
        manager
            .ResetDevice(dev.raw(), reset_token)
            .map_err(|e| MediaError::Other(format!("ResetDevice: {e}")))?;
        Ok(manager)
    }
}

fn configure_output_type(
    transform: &IMFTransform,
    cfg: &NvencEncoderConfig,
) -> Result<(), MediaError> {
    unsafe {
        let mut out_type: Option<IMFMediaType> = None;
        MFCreateMediaType(&mut out_type).map_err(|e| MediaError::Other(format!("MFCreateMediaType: {e}")))?;
        let out_type = out_type.ok_or_else(|| MediaError::Other("out_type null".into()))?;

        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
        // bitrate
        out_type.SetUINT32(
            &windows::Win32::Media::MediaFoundation::MF_MT_AVG_BITRATE,
            cfg.bitrate_bps,
        )?;
        // frame rate (packed UINT64)
        let fr_packed = (cfg.fps_numerator as u64) << 32 | cfg.fps_denominator as u64;
        out_type.SetUINT64(&MF_MT_FRAME_RATE, fr_packed)?;
        // frame size (packed UINT64)
        let size_packed = (cfg.width as u64) << 32 | cfg.height as u64;
        out_type.SetUINT64(&MF_MT_FRAME_SIZE, size_packed)?;
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // pixel aspect ratio 1:1
        out_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1u64)?;

        transform.SetOutputType(0, &out_type, 0)
            .map_err(|e| MediaError::Other(format!("SetOutputType: {e}")))?;
    }
    Ok(())
}

fn configure_input_type(
    transform: &IMFTransform,
    cfg: &NvencEncoderConfig,
) -> Result<(), MediaError> {
    unsafe {
        let mut in_type: Option<IMFMediaType> = None;
        MFCreateMediaType(&mut in_type).map_err(|e| MediaError::Other(format!("MFCreateMediaType: {e}")))?;
        let in_type = in_type.ok_or_else(|| MediaError::Other("in_type null".into()))?;

        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        let fr_packed = (cfg.fps_numerator as u64) << 32 | cfg.fps_denominator as u64;
        in_type.SetUINT64(&MF_MT_FRAME_RATE, fr_packed)?;
        let size_packed = (cfg.width as u64) << 32 | cfg.height as u64;
        in_type.SetUINT64(&MF_MT_FRAME_SIZE, size_packed)?;
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1u64 << 32 | 1u64)?;

        transform.SetInputType(0, &in_type, 0)
            .map_err(|e| MediaError::Other(format!("SetInputType: {e}")))?;
    }
    Ok(())
}

fn set_low_latency(transform: &IMFTransform) -> Result<(), MediaError> {
    unsafe {
        let attrs: IMFAttributes = transform
            .GetAttributes()
            .map_err(|e| MediaError::Other(format!("GetAttributes: {e}")))?;
        attrs.SetUINT32(&MF_LOW_LATENCY, 1)
            .map_err(|e| MediaError::Other(format!("SetUINT32(MF_LOW_LATENCY): {e}")))?;
        // Async MFTs need explicit unlock to deliver output events.
        let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
    }
    Ok(())
}
```

Note: this is the skeleton; `encode` is added in Step 2. The build
will succeed at this point only if all imports resolve. The
implementer may need to remove unused imports until Step 2 lands.

- [ ] **Step 2: Implement the `encode` method**

Add to the same file, after the `Drop` impl:

```rust
impl Hevc265Encoder for MfH265Encoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        // 1. BGRA -> NV12
        self.bgra_to_nv12.convert(texture, &self.nv12_input)?;

        // 2. Wrap NV12 texture in an IMFSample.
        let sample = wrap_d3d11_in_sample(&self.nv12_input, timestamp_us, self.width, self.height)?;

        // Force IDR if requested or initial frame.
        if force_idr || self.pending_idr {
            unsafe {
                sample.SetUINT32(
                    &windows::Win32::Media::MediaFoundation::MFSampleExtension_VideoEncodeQP,
                    25,
                )?;
                // Indicate IDR via MFSampleExtension_CleanPoint.
                sample.SetUINT32(
                    &windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint,
                    1,
                )?;
            }
            self.pending_idr = false;
        }

        // 3. ProcessInput
        unsafe {
            self.transform.ProcessInput(0, &sample, 0)
                .map_err(|e| MediaError::Other(format!("ProcessInput: {e}")))?;
        }

        // 4. ProcessOutput in a loop until we get one encoded sample
        // (low-latency mode usually returns 1:1).
        let encoded = drain_one_output(&self.transform)?;

        self.sample_seq += 1;
        Ok(EncodedH265Frame {
            nal_bytes: encoded.bytes,
            is_keyframe: encoded.is_idr,
            timestamp: timestamp_us,
        })
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        // Re-set output type bitrate. MFT may need full reset; for now,
        // log and defer like the NVENC equivalent.
        tracing::warn!(
            target = "mf",
            requested_bps = bps,
            "set_target_bitrate is currently a no-op for MF (rate-control \
             reconfig requires MFT reset)"
        );
    }

    fn backend_name(&self) -> &'static str {
        "mf"
    }
}

struct DrainedOutput {
    bytes: Vec<u8>,
    is_idr: bool,
}

fn drain_one_output(transform: &IMFTransform) -> Result<DrainedOutput, MediaError> {
    use windows::Win32::Media::MediaFoundation::{
        MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    };
    unsafe {
        let stream_info: MFT_OUTPUT_STREAM_INFO = transform
            .GetOutputStreamInfo(0)
            .map_err(|e| MediaError::Other(format!("GetOutputStreamInfo: {e}")))?;
        let mft_provides_sample =
            stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        let mut sample: Option<IMFSample> = None;
        if !mft_provides_sample {
            // Caller must allocate. Allocate a memory buffer of cbSize.
            use windows::Win32::Media::MediaFoundation::{MFCreateMemoryBuffer, MFCreateSample as MFCreateSample2};
            let mut s: Option<IMFSample> = None;
            MFCreateSample2(&mut s).map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
            let s = s.ok_or_else(|| MediaError::Other("sample null".into()))?;
            let mut buf: Option<windows::Win32::Media::MediaFoundation::IMFMediaBuffer> = None;
            MFCreateMemoryBuffer(stream_info.cbSize, &mut buf)
                .map_err(|e| MediaError::Other(format!("MFCreateMemoryBuffer: {e}")))?;
            let buf = buf.ok_or_else(|| MediaError::Other("buf null".into()))?;
            s.AddBuffer(&buf).map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
            sample = Some(s);
        }

        loop {
            let mut data_buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(sample.clone()),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            };
            let mut data_buffers = [data_buffer];
            let mut status: u32 = 0;
            let res = transform.ProcessOutput(0, &mut data_buffers, &mut status);
            match res {
                Ok(()) => {
                    let out_sample = std::mem::ManuallyDrop::take(&mut data_buffers[0].pSample)
                        .ok_or_else(|| MediaError::Other("ProcessOutput: no sample".into()))?;
                    return read_sample_bytes(&out_sample);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                    return Err(MediaError::Other(
                        "ProcessOutput needs more input (low-latency violation; \
                         MFT did not emit a frame)".into(),
                    ));
                }
                Err(e) => {
                    return Err(MediaError::Other(format!("ProcessOutput: {e}")));
                }
            }
        }
    }
}

fn read_sample_bytes(sample: &IMFSample) -> Result<DrainedOutput, MediaError> {
    use windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint;
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| MediaError::Other(format!("ConvertToContiguousBuffer: {e}")))?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|e| MediaError::Other(format!("buffer.Lock: {e}")))?;
        let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
        buffer.Unlock().map_err(|e| MediaError::Other(format!("buffer.Unlock: {e}")))?;

        let is_idr = sample
            .GetUINT32(&MFSampleExtension_CleanPoint)
            .map(|v| v != 0)
            .unwrap_or(false);
        Ok(DrainedOutput { bytes, is_idr })
    }
}

fn wrap_d3d11_in_sample(
    texture: &D3d11Texture,
    timestamp_us: u64,
    _width: u32,
    _height: u32,
) -> Result<IMFSample, MediaError> {
    unsafe {
        let mut buffer: Option<windows::Win32::Media::MediaFoundation::IMFMediaBuffer> = None;
        MFCreateDXGISurfaceBuffer(
            &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::IID,
            texture.raw(),
            0,
            false,
            &mut buffer,
        )
        .map_err(|e| MediaError::Other(format!("MFCreateDXGISurfaceBuffer: {e}")))?;
        let buffer = buffer.ok_or_else(|| MediaError::Other("DXGI buffer null".into()))?;

        let mut sample: Option<IMFSample> = None;
        MFCreateSample(&mut sample).map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
        let sample = sample.ok_or_else(|| MediaError::Other("sample null".into()))?;
        sample.AddBuffer(&buffer).map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
        // Timestamp in 100ns units (MF convention)
        sample.SetSampleTime((timestamp_us * 10) as i64)
            .map_err(|e| MediaError::Other(format!("SetSampleTime: {e}")))?;
        Ok(sample)
    }
}
```

> **Note:** The above is a faithful skeleton. Some `windows` crate
> imports may need adjustment — the implementer adapts as the Rust
> compiler tells them. The structural correctness (MFT init, MFT
> message ordering, NV12 conversion, timestamp scaling) is the
> point.

- [ ] **Step 3: Re-export from `mf/mod.rs`**

In `crates/media-win/src/mf/mod.rs`, add `pub mod encoder;` and a
re-export:

```rust
pub mod decoder;
pub mod encoder;

pub use decoder::H265Decoder;
pub use encoder::MfH265Encoder;
```

- [ ] **Step 4: Add `Mf` variant to `HwHevcEncoder` enum**

In `crates/media-win/src/encoder_trait.rs`, find the enum and update:

```rust
use crate::mf::MfH265Encoder;
use crate::nvenc::NvencEncoder;

pub enum HwHevcEncoder {
    Nvenc(NvencEncoder),
    Mf(MfH265Encoder),
}

impl Hevc265Encoder for HwHevcEncoder {
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError> {
        match self {
            Self::Nvenc(e) => e.encode(texture, force_idr, timestamp_us),
            Self::Mf(e) => e.encode(texture, force_idr, timestamp_us),
        }
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        match self {
            Self::Nvenc(e) => e.set_target_bitrate(bps),
            Self::Mf(e) => e.set_target_bitrate(bps),
        }
    }

    fn backend_name(&self) -> &'static str {
        match self {
            Self::Nvenc(e) => e.backend_name(),
            Self::Mf(e) => e.backend_name(),
        }
    }
}

impl From<NvencEncoder> for HwHevcEncoder {
    fn from(e: NvencEncoder) -> Self { Self::Nvenc(e) }
}

impl From<MfH265Encoder> for HwHevcEncoder {
    fn from(e: MfH265Encoder) -> Self { Self::Mf(e) }
}
```

- [ ] **Step 5: Re-export `MfH265Encoder` from lib.rs**

In `crates/media-win/src/lib.rs`, add to the existing re-exports:

```rust
pub use mf::{H265Decoder, MfH265Encoder};
```

(Replace the existing `pub use mf::H265Decoder;` if it exists — adapt
to actual current state.)

- [ ] **Step 6: Build**

```bash
cargo build -p prdt-media-win 2>&1 | tail -10
cargo clippy -p prdt-media-win --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: clean. Compile errors here are commonly:
- Unused imports — remove
- `windows` crate path mismatches — check actual symbol path via
  `windows::Win32::Media::MediaFoundation::*` rustdoc or `cargo doc -p windows --open`
- `D3d11Texture::raw()` returning `Option<&ID3D11Texture2D>` vs `&ID3D11Texture2D` — adapt the `wrap_d3d11_in_sample` and `BgraToNv12::convert` paths

This task is the largest; budget at least one full day of fix-up
iterations against the compiler.

- [ ] **Step 7: Smoke (manual, NVIDIA dev box)**

This requires building the host bin with the new path. Defer to
Task 5 which adds the CLI flag. For now, just confirm the workspace
compiles.

- [ ] **Step 8: Commit**

```bash
git add crates/media-win/src/mf/encoder.rs \
        crates/media-win/src/mf/mod.rs \
        crates/media-win/src/encoder_trait.rs \
        crates/media-win/src/lib.rs
git commit -m "media-win: MfH265Encoder MFT wrapper (BGRA->NV12 + IMFTransform pipeline)"
```

---

## Task 5: Host CLI `--encoder` + `Config.host.encoder` + auto-select

**Files:**
- Modify: `crates/host/src/main.rs`
- Modify: `crates/gui-common/src/config.rs`
- Modify: `crates/host/Cargo.toml` (only if a new dep is needed — should not be)

- [ ] **Step 1: Add `encoder` field to `HostConfig`**

In `crates/gui-common/src/config.rs`, find the existing `HostConfig` struct (around line 27) and add a field with `serde(default)` so legacy config.toml files still parse:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostConfig {
    pub bind: String,
    pub monitor: u32,
    pub bitrate_mbps: u32,
    pub outgoing_dir: PathBuf,
    #[serde(default)]
    pub signaling_url: String,
    pub host_id_file: PathBuf,
    pub key_file: PathBuf,
    #[serde(default)]
    pub auto_start: bool,
    /// Encoder backend choice. "auto" picks NVENC on NVIDIA, MF
    /// elsewhere. Other values: "nvenc", "mf".
    #[serde(default = "default_encoder_choice")]
    pub encoder: String,
}

fn default_encoder_choice() -> String {
    "auto".into()
}
```

Update `Default` impl to include `encoder: "auto".into()`:

```rust
impl Default for HostConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:9000".into(),
            monitor: 0,
            bitrate_mbps: 30,
            outgoing_dir: PathBuf::from("prdt-outgoing"),
            signaling_url: String::new(),
            host_id_file: PathBuf::from("host-id.txt"),
            key_file: PathBuf::from("host-key.bin"),
            auto_start: false,
            encoder: "auto".into(),
        }
    }
}
```

- [ ] **Step 2: Add `--encoder` CLI flag to host bin**

In `crates/host/src/main.rs`, find the existing `Args` struct (around line 42) and add a new field:

```rust
    /// Encoder backend: auto (default) | nvenc | mf.
    /// "auto" picks NVENC on NVIDIA GPUs and MF (Media Foundation
    /// H.265 encoder MFT) on AMD/Intel.
    #[arg(long, default_value = "auto")]
    encoder: String,
```

- [ ] **Step 3: Update `run_host` to construct the chosen encoder**

In `crates/host/src/main.rs`, find the section around lines 137-160 where the producer is built:

```rust
    let adapter = pick_default_adapter().context("no GPU adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;
    // ...
    let mut producer = DxgiNvencProducer::new(&dev, &output, bitrate_bps).context("producer")?;
```

Replace the producer construction with explicit encoder selection.
Add this helper at the bottom of `main.rs`:

```rust
fn pick_encoder(
    args_encoder: &str,
    adapter: &prdt_media_win::Adapter,
    dev: &D3d11Device,
    cfg: &prdt_media_win::NvencEncoderConfig,
) -> anyhow::Result<prdt_media_win::HwHevcEncoder> {
    use prdt_media_win::{HwHevcEncoder, MfH265Encoder, NvencEncoder};
    let choice = if args_encoder == "auto" {
        if adapter.is_nvidia() {
            "nvenc"
        } else {
            "mf"
        }
    } else {
        args_encoder
    };
    match choice {
        "nvenc" => {
            let enc = NvencEncoder::new(dev, cfg).context("NvencEncoder::new")?;
            Ok(HwHevcEncoder::Nvenc(enc))
        }
        "mf" => {
            let enc = MfH265Encoder::new(dev, cfg).context("MfH265Encoder::new")?;
            Ok(HwHevcEncoder::Mf(enc))
        }
        other => anyhow::bail!("unknown --encoder {other:?} (auto, nvenc, mf)"),
    }
}
```

(The exact `Adapter` type comes from `prdt_media_win::*` re-export
or `prdt_media_win::adapter::*`. Check `crates/media-win/src/lib.rs`
for the actual export name.)

Update the producer build block to use the helper:

```rust
    let adapter = pick_default_adapter().context("no GPU adapter")?;
    let dev = D3d11Device::create(&adapter).context("D3D11 device")?;
    let outputs = enumerate_outputs_for_adapter(&adapter).context("outputs")?;
    if outputs.is_empty() {
        anyhow::bail!("no display outputs found on adapter");
    }
    let output = outputs
        .get(args.monitor as usize)
        .with_context(|| {
            format!(
                "no output at index {} (available: 0..{})",
                args.monitor,
                outputs.len()
            )
        })?
        .clone();

    info!(
        monitor = args.monitor,
        device_name = %output.device_name,
        bitrate_mbps = args.bitrate_mbps,
        encoder = %args.encoder,
        "host starting"
    );

    let bitrate_bps = args.bitrate_mbps.saturating_mul(1_000_000);

    let enc_cfg = prdt_media_win::NvencEncoderConfig {
        width: (output.desktop_rect.right - output.desktop_rect.left) as u32,
        height: (output.desktop_rect.bottom - output.desktop_rect.top) as u32,
        fps_numerator: 60,
        fps_denominator: 1,
        bitrate_bps,
        gop_length: 60,
    };
    let encoder = pick_encoder(&args.encoder, &adapter, &dev, &enc_cfg)?;
    info!(backend = encoder.backend_name(), "encoder ready");
    let mut producer =
        DxgiNvencProducer::with_encoder(&dev, &output, encoder).context("producer")?;
```

(`encoder.backend_name()` requires `Hevc265Encoder` trait in scope:
add `use prdt_media_win::Hevc265Encoder;` at the top of main.rs.)

- [ ] **Step 4: Build + smoke (NVENC path unchanged)**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build --release -p prdt-host 2>&1 | tail -3
```

Expected: clean.

Smoke (NVIDIA dev box, default = auto = NVENC):

```bash
target/release/prdt-host.exe --headless --bind 127.0.0.1:9100 &
sleep 3
# Check stderr log for "encoder ready backend=nvenc"
```

(Skip actual viewer connect for this step; the encoder backend log
proves the CLI path works.)

- [ ] **Step 5: Smoke (`--encoder mf`)**

Run the host bin with the new path on the NVIDIA dev box (proves
the MF path works even when NVENC is also available):

```bash
target/release/prdt-host.exe --headless --bind 127.0.0.1:9100 --encoder mf &
sleep 5
# stderr should show "encoder ready backend=mf" and not crash within 5s
```

Document in the docs (Task 6) that this is a manual smoke; CI does
not exercise it.

- [ ] **Step 6: Commit**

```bash
git add crates/host/src/main.rs \
        crates/gui-common/src/config.rs
git commit -m "host: --encoder flag + Config.host.encoder + adapter-based auto select"
```

---

## Task 6: bench-matrix `--encoders` axis + docs + tag

**Files:**
- Modify: `crates/latency-bench/src/full_pipeline.rs` (accept encoder backend)
- Modify: `crates/latency-bench/src/lib.rs` (add `EncoderBackend` to MatrixAxes)
- Modify: `crates/latency-bench/src/bin/bench-matrix.rs` (add `--encoders` flag)
- Create: `docs/encoders.md`
- Modify: `docs/superpowers/STATUS.md`

- [ ] **Step 1: Add `EncoderBackend` enum to lib.rs**

In `crates/latency-bench/src/lib.rs`, find the `mod matrix` block. After
the existing `pub use full_pipeline::ConsumerBackend;`, add a parallel
`EncoderBackend`:

```rust
#[cfg(windows)]
pub use full_pipeline::EncoderBackend;
```

(Defined in Step 2 below.)

In `MatrixAxes`, add an `encoders` field:

```rust
    pub struct MatrixAxes {
        pub resolutions: Vec<(u32, u32)>,
        pub bitrates_mbps: Vec<u32>,
        pub decoders: Vec<ConsumerBackend>,
        pub encoders: Vec<EncoderBackend>,
        pub fps: Vec<u32>,
        pub duration: std::time::Duration,
    }
```

In `ConfigStats`, add `encoder` field:

```rust
    pub struct ConfigStats {
        pub config_id: String,
        pub resolution: (u32, u32),
        pub bitrate_mbps: u32,
        pub decoder: ConsumerBackend,
        pub encoder: EncoderBackend,
        // ... rest unchanged
    }
```

Update `config_id`:

```rust
    pub fn config_id(
        resolution: (u32, u32),
        fps: u32,
        bitrate_mbps: u32,
        decoder: ConsumerBackend,
        encoder: EncoderBackend,
    ) -> String {
        let dec = match decoder {
            ConsumerBackend::Mf => "mfdec",
            ConsumerBackend::Nvdec => "nvdec",
        };
        let enc = match encoder {
            EncoderBackend::Nvenc => "nvenc",
            EncoderBackend::Mf => "mfenc",
        };
        format!("{}p{}-{}mbps-enc{}-dec{}", resolution.1, fps, bitrate_mbps, enc, dec)
    }
```

Note the `mfdec` (was `mf`) renaming on the decoder side — this avoids
confusion with `mfenc` once both axes can be `mf`. Update existing
unit tests' expected strings accordingly.

Update `expand_matrix`:

```rust
    pub fn expand_matrix(axes: &MatrixAxes) -> Vec<FullPipelineConfig> {
        let mut out = Vec::with_capacity(
            axes.resolutions.len()
                * axes.bitrates_mbps.len()
                * axes.decoders.len()
                * axes.encoders.len()
                * axes.fps.len(),
        );
        for &res in &axes.resolutions {
            for &bitrate_mbps in &axes.bitrates_mbps {
                for &encoder in &axes.encoders {
                    for &decoder in &axes.decoders {
                        for &fps in &axes.fps {
                            out.push(FullPipelineConfig {
                                width: res.0,
                                height: res.1,
                                fps,
                                duration: axes.duration,
                                bitrate_bps: bitrate_mbps.saturating_mul(1_000_000),
                                drop_ppm: 0,
                                latency_ms: 0,
                                csv: None,
                                consumer: decoder,
                                encoder,
                            });
                        }
                    }
                }
            }
        }
        out
    }
```

Update `aggregate` to set the new `encoder` field on `ConfigStats`.

Update existing unit tests to construct configs with `encoder:
EncoderBackend::Nvenc` and to expect the new `config_id` format.

- [ ] **Step 2: Add `EncoderBackend` + `encoder` field to `FullPipelineConfig`**

In `crates/latency-bench/src/full_pipeline.rs`, after the existing
`pub enum ConsumerBackend { Mf, Nvdec }`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    Nvenc,
    Mf,
}

impl std::str::FromStr for EncoderBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "nvenc" => Ok(Self::Nvenc),
            "mf" => Ok(Self::Mf),
            other => anyhow::bail!("unknown encoder backend {other:?} (options: nvenc, mf)"),
        }
    }
}
```

Update `FullPipelineConfig`:

```rust
pub struct FullPipelineConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration: Duration,
    pub bitrate_bps: u32,
    pub drop_ppm: u32,
    pub latency_ms: u64,
    pub csv: Option<std::path::PathBuf>,
    pub consumer: ConsumerBackend,
    pub encoder: EncoderBackend,
}
```

In `run_for_matrix`, replace the unconditional `NvencEncoder::new` line with:

```rust
    use prdt_media_win::{HwHevcEncoder, MfH265Encoder};
    let encoder: HwHevcEncoder = match cfg.encoder {
        EncoderBackend::Nvenc => NvencEncoder::new(&dev, &enc_cfg)
            .map_err(|e| anyhow::anyhow!("NvencEncoder::new: {e}"))?
            .into(),
        EncoderBackend::Mf => MfH265Encoder::new(&dev, &enc_cfg)
            .map_err(|e| anyhow::anyhow!("MfH265Encoder::new: {e}"))?
            .into(),
    };
```

Then replace the encoder.encode() calls in the bench loop:

```rust
    let encoded = encoder
        .encode(&tex, force_idr, capture_us)
        .map_err(|e| anyhow::anyhow!("encode: {e}"))?;
```

Add `use prdt_media_win::Hevc265Encoder;` at the top of full_pipeline.rs
so the trait method resolves on the `HwHevcEncoder` enum.

- [ ] **Step 3: Add `--encoders` CLI flag to bench-matrix bin**

In `crates/latency-bench/src/bin/bench-matrix.rs`:

```rust
    /// Comma-separated encoders. Choices: nvenc, mf.
    #[arg(long, value_delimiter = ',', default_values_t = vec!["nvenc".to_string()])]
    encoders: Vec<String>,
```

Parse them in main():

```rust
fn parse_encoders(strs: &[String]) -> anyhow::Result<Vec<EncoderBackend>> {
    strs.iter()
        .map(|s| match s.as_str() {
            "nvenc" => Ok(EncoderBackend::Nvenc),
            "mf" => Ok(EncoderBackend::Mf),
            other => Err(anyhow::anyhow!("unknown encoder {other:?} (options: nvenc, mf)")),
        })
        .collect()
}
```

Use in `MatrixAxes` construction:

```rust
    let encoders = parse_encoders(&args.encoders)?;
    let axes = MatrixAxes {
        resolutions,
        bitrates_mbps: args.bitrates,
        decoders,
        encoders,
        fps: args.fps,
        duration: Duration::from(args.duration),
    };
```

Update import: `use prdt_latency_bench::EncoderBackend;`.

- [ ] **Step 4: Update bench-matrix CSV writer to include encoder column**

In `crates/latency-bench/src/lib.rs`'s `write_summary_csv`, add an
`encoder` column. The new header:

```
config_id,resolution,bitrate_mbps,decoder,encoder,fps,sent,received,loss_ppm,arrival_p50_us,arrival_p95_us,arrival_p99_us,decode_p50_us,decode_p95_us,decode_p99_us,e2e_p50_us,e2e_p95_us,e2e_p99_us
```

Adjust the row formatting to include the encoder string between
decoder and fps. Update the `summary_csv_writer_emits_header_and_one_row`
test to expect the new format.

- [ ] **Step 5: Build + dry-run smoke**

```bash
cargo build --release -p prdt-latency-bench --bin prdt-bench-matrix 2>&1 | tail -3
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir /tmp/dry --dry-run --encoders nvenc,mf 2>/dev/null | wc -l
```

Expected: clean build; dry-run line count = `3 res × 5 bitrates × 2 decoders × 2 encoders × 2 fps = 120`.

- [ ] **Step 6: Tests + clippy**

```bash
cargo test -p prdt-latency-bench 2>&1 | tail -10
cargo test -p prdt-media-win 2>&1 | tail -10
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: all existing tests pass after axis/CSV updates; clippy clean.

- [ ] **Step 7: Create `docs/encoders.md`**

```markdown
# Encoder backends (Windows)

`prdt-host` supports two H.265 encoder backends on Windows:

| Backend | GPU coverage | Latency | Default |
|---|---|---|---|
| `nvenc` | NVIDIA only | lowest (~5 ms encode p50) | when adapter is NVIDIA |
| `mf` | NVIDIA / AMD / Intel | higher (~10–15 ms encode p50) | when adapter is non-NVIDIA |

Output bitstream is identical (Annex-B H.265 NAL units), so the viewer
side (MF or NVDEC decoder) does not care which encoder was used.

## CLI

```
prdt-host.exe --encoder <auto|nvenc|mf>
```

`auto` (default) picks NVENC on NVIDIA, MF elsewhere.

The `Config.host.encoder` field in `%APPDATA%\prdt\config.toml` carries
the value across launches. Existing config.toml files default to
`auto` (via serde default).

## Bench

`prdt-bench-matrix` exposes `--encoders nvenc,mf` to compare both
backends side by side under the same resolution / bitrate / fps axes.
Default axis value is `nvenc` only; opt in by passing `--encoders nvenc,mf`.

## Manual smoke procedure

On any Windows machine with a working NVIDIA driver:

```bash
prdt-host.exe --headless --bind 127.0.0.1:9100 --encoder nvenc
```

stderr log line `encoder ready backend=nvenc`. Connect a viewer; verify frames render.

```bash
prdt-host.exe --headless --bind 127.0.0.1:9100 --encoder mf
```

stderr log line `encoder ready backend=mf`. Connect a viewer; verify frames
render. Expect ~10 ms higher e2e latency than the NVENC run.

## Future work

- DX12 Video Encode path: blocked on D3D12 base migration. Will live
  in a parallel `Dx12Hevc265Encoder` trait taking `&D3d12Resource`.
- AV1 encode: blocked on Ada Lovelace+ NVENC AV1 hardware OR DX12
  Video Encode AV1 (DX12 1.7+ + AV1-capable GPU).
- Software encoder fallback: not planned (latency unsuitable for
  real-time, GPL licence collisions).
```

- [ ] **Step 8: Update STATUS.md**

In `docs/superpowers/STATUS.md`:

Replace the `**Latest tag:**` line:

```markdown
**Latest tag:** `mf-encoder-fallback-complete`
```

Update `**Branch state:**`:

```markdown
**Branch state:** master (all phase work merged) — Phase 4 + Plan 4 B1 + B4 + B6 + B7 + B8 完了, MF encoder fallback for AMD/Intel Windows
```

Add a row in the relevant table (probably under Plan 4):

```markdown
| `mf-encoder-fallback-complete` | Windows MF H.265 encoder MFT fallback for non-NVIDIA GPUs。`Hevc265Encoder` trait + `EncodedH265Frame` 共有(`encoder_trait.rs`)、`HwHevcEncoder` enum で runtime dispatch。`NvencEncoder` と新 `MfH265Encoder` 両方 trait 実装。MF encoder は MFT enumeration → D3D11 device manager bind → BGRA→NV12 conversion(D3D11 VideoProcessor)→ `IMFTransform::ProcessInput`/`ProcessOutput` で Annex-B H.265 emit。`--encoder {auto,nvenc,mf}` CLI + `Config.host.encoder` 永続化(serde default で legacy config.toml 互換)。`prdt-bench-matrix` に `--encoders` 軸追加(NVENC vs MF 比較可能)。`docs/encoders.md` に backend 説明 + manual smoke。MF encoder の latency は NVENC より ~10ms 高い見込み。DX12 Video Encode は将来 trait 拡張点として spec 明記、本タスク不実装。 |
```

Adjust test count line:

```markdown
**Test count:** ~310 automated Rust tests + 11 Python tests, all passing
```

(Exact count depends on how many MfH265Encoder unit tests land. Update
to actual after running.)

- [ ] **Step 9: Run the full workspace tests + clippy + manual smoke**

```bash
cargo test --workspace 2>&1 | awk '/^test result:/ {p+=$4; f+=$6} END {print "total:", p, "failed:", f}'
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -3
target/release/prdt-host.exe --headless --bind 127.0.0.1:9100 --encoder nvenc &
sleep 3
# stderr log shows "encoder ready backend=nvenc"
kill %1
target/release/prdt-host.exe --headless --bind 127.0.0.1:9100 --encoder mf &
sleep 5
# stderr log shows "encoder ready backend=mf" with no panic
kill %1
```

Both smoke runs should print the corresponding `backend=` line in the
host stderr log without panicking. Exact log line shape may vary.

- [ ] **Step 10: Commit + tag**

```bash
git add crates/latency-bench/src/full_pipeline.rs \
        crates/latency-bench/src/lib.rs \
        crates/latency-bench/src/bin/bench-matrix.rs \
        docs/encoders.md \
        docs/superpowers/STATUS.md
git commit -m "bench-matrix: --encoders axis + docs/encoders.md + STATUS update"

git tag -a mf-encoder-fallback-complete -m "$(cat <<'EOF'
Windows MF H.265 encoder fallback complete

Adds Media Foundation H.265 encoder MFT path so non-NVIDIA Windows
GPUs (AMD / Intel) can run the host bin. Refactors NvencEncoder and
the new MfH265Encoder behind a shared Hevc265Encoder trait.

- crates/media-win/src/encoder_trait.rs: Hevc265Encoder trait +
  EncodedH265Frame (relocated from nvenc/encoder.rs) + HwHevcEncoder
  enum with runtime dispatch
- crates/media-win/src/mf/encoder.rs: MfH265Encoder using MFT
  enumeration + D3D11 device manager + low-latency mode + IMFSample
  via DXGI surface buffer
- crates/media-win/src/d3d11/bgra_to_nv12.rs: D3D11 VideoProcessor
  wrapper for the BGRA->NV12 input conversion (MFT typically only
  accepts NV12)
- crates/media-win/src/mf/mod.rs: ensure_mf_runtime() shared helper
  (CoInitializeEx + MFStartup, OnceLock-guarded)
- crates/host/src/main.rs: --encoder {auto, nvenc, mf} CLI flag,
  adapter-vendor-based auto selection
- crates/gui-common/src/config.rs: HostConfig::encoder with serde
  default ("auto") so legacy config.toml stays valid
- crates/latency-bench: --encoders axis on prdt-bench-matrix,
  EncoderBackend enum threaded through full_pipeline + matrix
- docs/encoders.md: backend table, CLI + bench usage, manual smoke

Out of scope: DX12 Video Encode (separate trait family for D3D12
resources, future work), AV1, software encoder fallback. Manual
smoke verified on NVIDIA RTX 3070 Ti for both --encoder nvenc and
--encoder mf paths.
EOF
)"
git tag | grep mf-encoder
```

- [ ] **Step 11: Final summary report**

Report back:

- Workspace test count + delta
- Workspace clippy result
- Manual smoke results for both `--encoder nvenc` and `--encoder mf`
- Files changed across all 6 tasks
- Tag listing
- Sample stderr log line proving each encoder backend was used

---

## Risks & Notes for Implementer

- **MF encoder MFT availability**: HEVC Video Extensions ship a software
  decoder MFT, but the encoder MFT is contributed by the GPU driver. On
  recent NVIDIA / AMD / Intel drivers it's present. If MFTEnumEx returns
  zero MFTs, the `MfH265Encoder::new` call will surface that. Document
  in docs/encoders.md as a known limitation on Server SKUs / minimal Windows
  installs.
- **NV12 input vs BGRA**: the plan unconditionally converts BGRA to NV12
  via D3D11 VideoProcessor. Some MFTs DO accept BGRA directly; the
  current plan prioritises the universally-supported NV12 path. A
  future optimisation can probe `IMFTransform::GetInputAvailableType`
  for BGRA support and skip the conversion if available.
- **Async vs sync MFT**: the plan uses sync MFT semantics
  (`MFT_ENUM_FLAG_HARDWARE` is not async-only). If the chosen MFT
  is actually async (returns `MF_E_TRANSFORM_NEED_MORE_INPUT`
  forever), the `drain_one_output` loop must be rewritten to
  hook the `IMFAsyncCallback` event sink. The plan's stub bails
  with an error in that case so the implementer notices.
- **Bitrate live update**: both `NvencEncoder::set_target_bitrate` and
  `MfH265Encoder::set_target_bitrate` log a warning and do nothing.
  Live rate-control reconfig is a separate work item; for now the
  bitrate is fixed at producer construction.
- **Test coverage**: the MF encoder cannot be unit-tested without GPU
  + MFT. The plan relies on integration smoke (host bin + bench-matrix
  with `--encoders mf`). CI cannot exercise this; document as manual
  pre-release check.
- **`encoder_trait.rs` file location**: top-level under `crates/media-win/src/`,
  not nested under a subdirectory. The trait is the cross-cutting concern
  shared by both nvenc and mf modules.
- **`Adapter::is_nvidia()` method**: verify it exists with `grep -n "fn is_nvidia" crates/media-win/src/adapter.rs`. If named differently, adapt `pick_encoder` accordingly.
- **Decoder name change**: Step 1 of Task 6 renames the decoder side
  config_id from `mf` to `mfdec` to avoid colliding with the new
  `mfenc` encoder string. Existing test fixtures need updating —
  search for `"mf"` strings in `crates/latency-bench/src/lib.rs` and
  `bench-matrix.rs` and update accordingly. Sample CSV outputs in
  `bench-results/` won't auto-update; that's expected (those are
  per-run artifacts, not committed).

---

## Self-Review

**Spec coverage:**
- §Architecture (trait, enum dispatch, producer alias, encoder selection) → Tasks 1, 2 ✓
- §Implementations (NvencEncoder + MfH265Encoder) → Tasks 1, 4 ✓
- §Producer dispatch + adapter-based auto select → Tasks 2, 5 ✓
- §CLI + Config.host.encoder → Task 5 ✓
- §MF encoder details (MFT enum, D3D11 device manager, low-latency, NV12 input, output drain) → Tasks 3, 4 ✓
- §DX12 extension hook (no implementation, trait shape leaves room) → Task 1 trait design ✓
- §Tests (unit dispatch + bench-matrix axis + manual smoke) → Tasks 2, 6 ✓
- §Out-of-scope (AMD AMF, Intel oneVPL, macOS, encoder chain, software encoder) → spec / docs/encoders.md ✓
- §Exit criteria 8 items → Task 6 covers all (build, test, clippy, NVENC unchanged, MF smoke, docs, STATUS, tag) ✓
- §Risks (MFT availability, ARGB32 vs NV12, latency tradeoff, Config default, hot-swap) → Risks section above ✓

**Placeholder scan:** no "TBD" / "implement later" — Task 4 has a few
"the implementer adapts" notes which are honest scope limits not
deferrals (the plan provides the structural code; specific `windows`
crate paths may need slight tweaks).

**Type consistency:**
- `Hevc265Encoder` trait → Task 1 def, Tasks 1, 2, 4 use ✓
- `EncodedH265Frame` → Task 1 def, Tasks 1, 4 use ✓
- `HwHevcEncoder` → Task 2 def, Tasks 2, 4, 5, 6 use ✓
- `MfH265Encoder` → Task 4 def, Tasks 4, 5, 6 use ✓
- `EncoderBackend` (in latency-bench) → Task 6 def, Task 6 use ✓
- `EncoderChoice` (in host CLI) → Task 5 def, Task 5 use ✓
- `pick_encoder` helper signature: `(args_encoder: &str, adapter: &Adapter, dev: &D3d11Device, cfg: &NvencEncoderConfig) -> Result<HwHevcEncoder>` ✓
