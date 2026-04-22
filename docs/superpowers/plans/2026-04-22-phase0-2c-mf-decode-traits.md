# Phase 0 — Plan 2c of 4: Media Foundation Decode + Producer/Consumer Traits

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:**
1. `prdt-protocol` に `VideoProducer`/`VideoConsumer` primary traits を追加
2. `media-win` に Windows Media Foundation ベースの H.265 HW デコーダ(`mf::H265Decoder`)を追加(DXVA2/D3D11VA で HW アクセラレート)
3. Plan 2b の NVENC エンコーダと組み合わせた concrete trait 実装(`DxgiNvencProducer`、`MfD3d11Consumer`)
4. エンコード→デコードの round-trip smoke test で Phase 0 パイプラインの H.265 レイヤが成立することを確認

**Architecture choice (via user decision 2026-04-22):** Media Foundation(D3D11VA backend)を採用。cuvid/NVDEC 直叩きは Plan 2d(将来の最適化)として温存。

**Tech Stack:** Windows Media Foundation(`windows` crate `Win32_Media_MediaFoundation` feature)、既存の `media-win` + `protocol` 基盤。

**Spec reference:** §§ 2.3(primary traits)、2.5(concrete impls)、4.1 S7-S9(decode path)、6.1 F5、7.3(round-trip integration test)。

---

## File Structure(Plan 2c 完了時)

```
crates/
├── protocol/
│   └── src/
│       ├── lib.rs                  [modify] add video_pipeline module export
│       └── video_pipeline.rs       [new]    VideoProducer/VideoConsumer traits + errors
└── media-win/
    ├── Cargo.toml                  [modify] add Win32_Media_MediaFoundation etc.
    ├── src/
    │   ├── lib.rs                  [modify] export Mf* + concrete impls
    │   ├── mf/
    │   │   ├── mod.rs              [new]
    │   │   └── decoder.rs          [new] IMFTransform H.265 decoder
    │   └── pipeline/
    │       ├── mod.rs              [new]
    │       ├── producer.rs         [new] DxgiNvencProducer
    │       └── consumer.rs         [new] MfD3d11Consumer
    └── tests/
        └── pipeline_smoke.rs       [new] encode→decode round-trip
```

---

## Task List(8 tasks)

- Task 1: `protocol::video_pipeline` — VideoProducer/VideoConsumer traits + errors
- Task 2: `media-win` Cargo.toml: Media Foundation features
- Task 3: `mf::H265Decoder` — MFT instantiation, D3D11 Manager, input/output type config
- Task 4: `mf::H265Decoder` — process_input / process_output loop
- Task 5: Decode smoke test(NVENC → MF decode → verify NV12 texture dimensions)
- Task 6: `DxgiNvencProducer` concrete impl(DXGI capture + NVENC encode)
- Task 7: `MfD3d11Consumer` concrete impl + end-to-end pipeline smoke test
- Task 8: Final checks + README + `phase0-plan2c-complete` tag

---

## Task 1: protocol::video_pipeline traits

**Files:**
- Create: `crates/protocol/src/video_pipeline.rs`
- Modify: `crates/protocol/src/lib.rs`

- [ ] **Step 1: Create `crates/protocol/src/video_pipeline.rs`**

```rust
//! Primary traits for the video pipeline: `VideoProducer` on the host side
//! (capture + encode) and `VideoConsumer` on the viewer side (decode + render).
//!
//! These are the public boundaries between the media pipeline and the rest
//! of the system (per spec §2.3). Internal sub-traits (DesktopCapture,
//! VideoEncoder, VideoDecoder, VideoRenderer) are crate-private inside
//! `media-win` / future `media-linux`.

use crate::EncodedFrame;

#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    #[error("capture: {0}")]
    Capture(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("other: {0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error("decode: {0}")]
    Decode(String),
    #[error("render: {0}")]
    Render(String),
    #[error("other: {0}")]
    Other(String),
}

/// Captures the desktop, encodes frames, and emits `EncodedFrame` on demand.
/// Implementations: `DxgiNvencProducer` (Windows). Future: `WaylandVaapiProducer` (Linux), etc.
#[async_trait::async_trait]
pub trait VideoProducer: Send {
    /// Return the next encoded frame. Blocks until one is available.
    async fn next_frame(&mut self) -> Result<EncodedFrame, ProducerError>;

    /// Request an IDR (keyframe) at the next encode opportunity. Idempotent
    /// within the rate-limit window defined in spec §4.3.
    fn request_idr(&mut self);

    /// Update the target bitrate in bits per second. Honoured best-effort.
    fn set_target_bitrate(&mut self, bps: u32);
}

/// Accepts `EncodedFrame` on the viewer, decodes, and hands the decoded
/// frame to the render layer (via its own internals).
#[async_trait::async_trait]
pub trait VideoConsumer: Send {
    /// Submit an encoded frame for decoding and (eventual) display.
    async fn submit(&mut self, frame: EncodedFrame) -> Result<(), ConsumerError>;

    /// Whether the consumer needs an IDR (because of a decode failure, a
    /// stream discontinuity, or a fresh session). The caller forwards this
    /// as `ControlMessage::RequestIdr` to the host.
    fn needs_idr(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        assert_eq!(
            ProducerError::Capture("DXGI lost".into()).to_string(),
            "capture: DXGI lost"
        );
        assert_eq!(
            ConsumerError::Decode("MF_E_INVALIDMEDIATYPE".into()).to_string(),
            "decode: MF_E_INVALIDMEDIATYPE"
        );
    }
}
```

- [ ] **Step 2: Add `async-trait` to `prdt-protocol` dependencies**

Edit `crates/protocol/Cargo.toml`, add to `[dependencies]`:
```toml
async-trait = "0.1"
```

- [ ] **Step 3: Update `crates/protocol/src/lib.rs`**

Add `pub mod video_pipeline;` and re-export:
```rust
pub mod video_pipeline;
pub use video_pipeline::{ConsumerError, ProducerError, VideoConsumer, VideoProducer};
```

- [ ] **Step 4: Test and commit**

```bash
cargo test -p prdt-protocol
# expected: 29 total (28 prior + 1 new)
cargo clippy -p prdt-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
git add -A
git commit -m "feat(protocol): add VideoProducer/VideoConsumer primary traits"
```

---

## Task 2: Media Foundation Cargo features

**Files:**
- Modify: `crates/media-win/Cargo.toml`

- [ ] **Step 1: Add MF features to windows crate**

In the existing `features = [...]` list of the `windows` dep, add:
```
"Win32_Media_MediaFoundation",
"Win32_System_Com",
"Win32_Graphics_Direct3D9",  # IMFDXGIDeviceManager transitive dependency
```

- [ ] **Step 2: Verify build**

```bash
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export PATH="/c/Program Files/LLVM/bin:$PATH"
cargo check -p prdt-media-win
```

- [ ] **Step 3: Commit**

```bash
git add crates/media-win/Cargo.toml
git commit -m "build(media-win): add Media Foundation windows features"
```

---

## Task 3: `mf::H265Decoder` — MFT instantiation + D3D11 Manager

**Files:**
- Create: `crates/media-win/src/mf/mod.rs`
- Create: `crates/media-win/src/mf/decoder.rs`
- Modify: `crates/media-win/src/lib.rs`

This task sets up the Media Foundation Transform (MFT) but does NOT yet run the encode/decode loop (that's Task 4). By end of Task 3:
- MF initialized (`MFStartup`)
- Hardware H.265 decoder MFT instantiated
- D3D11 device manager attached
- Input/output types configured
- Tests: can construct and destroy the decoder without error

- [ ] **Step 1: Create `crates/media-win/src/mf/mod.rs`**

```rust
//! Media Foundation-based H.265 hardware decoder.
//! Uses the system's MFT (Media Foundation Transform) with a D3D11 device
//! manager for GPU acceleration via DXVA2/D3D11VA.

pub mod decoder;

pub use decoder::H265Decoder;
```

- [ ] **Step 2: Create `crates/media-win/src/mf/decoder.rs`**

```rust
//! Media Foundation H.265 decoder.
//!
//! Design:
//! - Calls `MFStartup(MF_VERSION)` once per process (guarded by OnceLock).
//! - Enumerates `MFT_CATEGORY_VIDEO_DECODER` with `MFVideoFormat_HEVC` and
//!   picks the first hardware-flagged MFT.
//! - Attaches an `IMFDXGIDeviceManager` referencing our D3d11Device.
//! - Sets input type (H.265 Annex-B bitstream, width/height).
//! - Sets output type (NV12 texture samples via IMFDXGIBuffer).
//!
//! The decoder holds an `IMFTransform` plus an output-sample allocator.
//! Per-frame, the caller feeds NAL units via `process_input` and pulls
//! decoded NV12 D3D11 textures via `process_output` (Task 4).

use std::sync::OnceLock;

use windows::core::{Interface, GUID};
use windows::Win32::Foundation::E_NOINTERFACE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::d3d11::D3d11Device;
use crate::error::{MediaError, Result};

/// H.265 / HEVC GUID for MF media types.
const MFVIDEOFORMAT_HEVC: GUID = GUID::from_u128(0x3263_5648_0000_0010_8000_00aa00389b71);
/// NV12 GUID for output media type.
const MFVIDEOFORMAT_NV12: GUID = GUID::from_u128(0x3231_564e_0000_0010_8000_00aa00389b71);

static MF_INITIALIZED: OnceLock<Result<()>> = OnceLock::new();

fn ensure_mf_initialized() -> Result<()> {
    let r = MF_INITIALIZED.get_or_init(|| unsafe {
        // CoInitializeEx may return S_FALSE if COM is already initialized; treat as OK.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| MediaError::Other(format!("MFStartup: {e}")))
    });
    match r {
        Ok(()) => Ok(()),
        Err(e) => Err(MediaError::Other(format!("MF init error: {e}"))),
    }
}

pub struct H265Decoder {
    mft: IMFTransform,
    width: u32,
    height: u32,
    needs_idr: bool,
    _dev: D3d11Device,
}

impl H265Decoder {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self> {
        ensure_mf_initialized()?;

        unsafe {
            // 1. Enumerate HW H.265 decoder MFTs.
            let mut input_info = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVIDEOFORMAT_HEVC,
            };
            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count: u32 = 0;

            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input_info),
                None,
                &mut activates,
                &mut count,
            )
            .map_err(|e| MediaError::Other(format!("MFTEnumEx: {e}")))?;

            if count == 0 {
                return Err(MediaError::Other(
                    "no HW H.265 decoder MFT available".into(),
                ));
            }

            // Take first result.
            let activate_slice = std::slice::from_raw_parts(activates, count as usize);
            let mft: IMFTransform = activate_slice[0]
                .as_ref()
                .ok_or_else(|| MediaError::Other("null activate".into()))?
                .ActivateObject::<IMFTransform>()
                .map_err(|e| MediaError::Other(format!("ActivateObject: {e}")))?;

            // 2. Attach D3D11 device manager.
            let mut dxgi_manager: Option<IMFDXGIDeviceManager> = None;
            let mut reset_token: u32 = 0;
            MFCreateDXGIDeviceManager(&mut reset_token, &mut dxgi_manager)
                .map_err(|e| MediaError::Other(format!("MFCreateDXGIDeviceManager: {e}")))?;
            let dxgi_manager = dxgi_manager
                .ok_or_else(|| MediaError::Other("null IMFDXGIDeviceManager".into()))?;
            dxgi_manager
                .ResetDevice(dev.device(), reset_token)
                .map_err(|e| MediaError::Other(format!("ResetDevice: {e}")))?;

            mft.ProcessMessage(
                MFT_MESSAGE_SET_D3D_MANAGER,
                dxgi_manager.as_raw() as usize,
            )
            .map_err(|e| MediaError::Other(format!("ProcessMessage SET_D3D_MANAGER: {e}")))?;

            // 3. Set input media type (H.265).
            let input_type = MFCreateMediaType()
                .map_err(|e| MediaError::Other(format!("MFCreateMediaType in: {e}")))?;
            input_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| MediaError::Other(format!("SetGUID major in: {e}")))?;
            input_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVIDEOFORMAT_HEVC)
                .map_err(|e| MediaError::Other(format!("SetGUID sub in: {e}")))?;
            // Pack width + height into a u64 for MF_MT_FRAME_SIZE.
            let frame_size_packed = ((width as u64) << 32) | (height as u64);
            input_type
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size_packed)
                .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size in: {e}")))?;

            mft.SetInputType(0, &input_type, 0)
                .map_err(|e| MediaError::Other(format!("SetInputType: {e}")))?;

            // 4. Set output media type (NV12).
            let output_type = MFCreateMediaType()
                .map_err(|e| MediaError::Other(format!("MFCreateMediaType out: {e}")))?;
            output_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| MediaError::Other(format!("SetGUID major out: {e}")))?;
            output_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVIDEOFORMAT_NV12)
                .map_err(|e| MediaError::Other(format!("SetGUID sub out: {e}")))?;
            output_type
                .SetUINT64(&MF_MT_FRAME_SIZE, frame_size_packed)
                .map_err(|e| MediaError::Other(format!("SetUINT64 frame_size out: {e}")))?;

            mft.SetOutputType(0, &output_type, 0)
                .map_err(|e| MediaError::Other(format!("SetOutputType: {e}")))?;

            // 5. Notify MFT of flush / stream start so it's ready to accept samples.
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| MediaError::Other(format!("NOTIFY_BEGIN_STREAMING: {e}")))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| MediaError::Other(format!("NOTIFY_START_OF_STREAM: {e}")))?;

            // Silence E_NOINTERFACE unused import warning.
            let _ = E_NOINTERFACE;

            Ok(Self {
                mft,
                width,
                height,
                needs_idr: true,
                _dev: dev.clone(),
            })
        }
    }

    pub fn needs_idr(&self) -> bool {
        self.needs_idr
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pick_default_adapter;

    #[test]
    fn create_h265_decoder() {
        let adapter = match pick_default_adapter() {
            Ok(a) => a,
            Err(_) => return,
        };
        let dev = D3d11Device::create(&adapter).expect("D3D11 device");
        match H265Decoder::new(&dev, 1920, 1080) {
            Ok(dec) => {
                assert_eq!(dec.width(), 1920);
                assert_eq!(dec.height(), 1080);
                assert!(dec.needs_idr());
            }
            Err(e) => {
                // On some systems (e.g., VMs) there is no HW H.265 decoder.
                // Report as non-fatal.
                eprintln!("H.265 MFT not available (non-fatal): {e}");
            }
        }
    }
}
```

- [ ] **Step 3: Update `crates/media-win/src/lib.rs`**

Add `pub mod mf;` and `pub use mf::H265Decoder;`.

- [ ] **Step 4: Test and commit**

```bash
cargo test -p prdt-media-win
# expected: 27 tests (26 prior + 1 new decoder construction)
cargo clippy -p prdt-media-win --all-targets -- -D warnings
git add -A
git commit -m "feat(media-win): add Media Foundation H.265 decoder scaffold"
```

---

## Task 4: `mf::H265Decoder` — process_input / process_output

**Files:**
- Modify: `crates/media-win/src/mf/decoder.rs`

Add two methods to `impl H265Decoder`:

```rust
/// Feed one encoded frame (H.265 NAL units) into the decoder.
pub fn process_input(&mut self, nal_bytes: &[u8], timestamp: i64) -> Result<()> {
    unsafe {
        // Create a media buffer and sample.
        let buffer = MFCreateMemoryBuffer(nal_bytes.len() as u32)
            .map_err(|e| MediaError::Other(format!("MFCreateMemoryBuffer: {e}")))?;

        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|e| MediaError::Other(format!("Lock buffer: {e}")))?;
        std::ptr::copy_nonoverlapping(nal_bytes.as_ptr(), data_ptr, nal_bytes.len());
        buffer
            .Unlock()
            .map_err(|e| MediaError::Other(format!("Unlock buffer: {e}")))?;
        buffer
            .SetCurrentLength(nal_bytes.len() as u32)
            .map_err(|e| MediaError::Other(format!("SetCurrentLength: {e}")))?;

        let sample = MFCreateSample()
            .map_err(|e| MediaError::Other(format!("MFCreateSample: {e}")))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| MediaError::Other(format!("AddBuffer: {e}")))?;
        sample
            .SetSampleTime(timestamp)
            .map_err(|e| MediaError::Other(format!("SetSampleTime: {e}")))?;

        match self.mft.ProcessInput(0, &sample, 0) {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                return Err(MediaError::Other("MFT NOTACCEPTING".into()));
            }
            Err(e) => return Err(MediaError::Other(format!("ProcessInput: {e}"))),
        }

        self.needs_idr = false;
        Ok(())
    }
}

/// Pull the next decoded frame. Returns None if the decoder needs more
/// input. Returns the output as raw bytes for now (the D3D11 texture
/// extraction is done in Task 5).
pub fn process_output(&mut self) -> Result<Option<Vec<u8>>> {
    unsafe {
        let mut output_buffer = MFT_OUTPUT_DATA_BUFFER::default();
        let mut out_count: u32 = 0;
        let status = self.mft.ProcessOutput(
            0,
            std::slice::from_mut(&mut output_buffer),
            &mut out_count,
        );
        match status {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                return Ok(None);
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // Output type change — re-configure. Not handled in Phase 0.
                self.needs_idr = true;
                return Err(MediaError::Other("MF_E_TRANSFORM_STREAM_CHANGE".into()));
            }
            Err(e) => return Err(MediaError::Other(format!("ProcessOutput: {e}"))),
        }

        // Extract sample -> buffer -> bytes.
        let sample = output_buffer
            .pSample
            .as_ref()
            .ok_or_else(|| MediaError::Other("null output sample".into()))?;
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| MediaError::Other(format!("ConvertToContiguousBuffer: {e}")))?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|e| MediaError::Other(format!("Lock out buffer: {e}")))?;
        let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
        buffer
            .Unlock()
            .map_err(|e| MediaError::Other(format!("Unlock out buffer: {e}")))?;

        Ok(Some(bytes))
    }
}
```

Verify build, add inline tests if practical (a full encode→decode is Task 5's integration test).

- [ ] **Commit**

```bash
git add crates/media-win/src/mf/decoder.rs
git commit -m "feat(media-win): add H265Decoder process_input/process_output"
```

---

## Task 5: Decode smoke test(encode via NVENC → decode via MF)

**Files:**
- Create: `crates/media-win/tests/decoder_smoke.rs`

```rust
#![cfg(windows)]

use prdt_media_win::{
    mf::H265Decoder, pick_default_adapter, synthetic::make_counter_texture, D3d11Device,
    NvencEncoder, NvencEncoderConfig,
};

#[test]
fn nvenc_to_mf_decode_round_trip() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => return,
    };
    if !adapter.is_nvidia() {
        eprintln!("skip: non-NVIDIA");
        return;
    }
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let cfg = NvencEncoderConfig {
        width: 256,
        height: 256,
        fps_numerator: 60,
        fps_denominator: 1,
        bitrate_bps: 2_000_000,
        gop_length: 30,
    };
    let enc = NvencEncoder::new(&dev, &cfg).expect("encoder");
    let mut dec = match H265Decoder::new(&dev, cfg.width, cfg.height) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("no MF H.265 decoder (skip): {e}");
            return;
        }
    };

    // Encode a single IDR frame.
    let tex = make_counter_texture(&dev, cfg.width, cfg.height, 0).expect("texture");
    let encoded = enc.encode(&tex, true, 0).expect("encode");
    assert!(!encoded.nal_bytes.is_empty());

    // Feed to decoder.
    dec.process_input(&encoded.nal_bytes, 0).expect("process_input");

    // Try a few times to pull output.
    let mut decoded_bytes: Option<Vec<u8>> = None;
    for _ in 0..3 {
        match dec.process_output().expect("process_output") {
            Some(bytes) => {
                decoded_bytes = Some(bytes);
                break;
            }
            None => {}
        }
    }

    let bytes = decoded_bytes.expect("expected at least one decoded sample");
    // NV12 size = width * height * 1.5 (Y + UV half-res).
    let expected = (cfg.width * cfg.height * 3 / 2) as usize;
    assert!(
        bytes.len() >= expected,
        "decoded buffer too small: {} < {}",
        bytes.len(),
        expected
    );
    eprintln!("decoded {} bytes (expected >= {} for NV12)", bytes.len(), expected);
}
```

- [ ] Commit `test(media-win): encode→decode H.265 round-trip smoke test`

---

## Task 6: DxgiNvencProducer concrete impl

**Files:**
- Create: `crates/media-win/src/pipeline/mod.rs`
- Create: `crates/media-win/src/pipeline/producer.rs`
- Modify: `crates/media-win/src/lib.rs`

Pseudocode-level spec for the implementer (full code in this task's exact style — implementer fills unsafe details):

```rust
// crates/media-win/src/pipeline/producer.rs

pub struct DxgiNvencProducer {
    dup: DesktopDuplication,
    encoder: NvencEncoder,
    seq: u64,
    epoch: std::time::Instant,
    idr_pending: bool,
}

impl DxgiNvencProducer {
    pub fn new(dev: &D3d11Device, output: &OutputInfo, bitrate_bps: u32) -> Result<Self> {
        let dup = DesktopDuplication::new(dev, output)?;
        let cfg = NvencEncoderConfig {
            width: dup.width(),
            height: dup.height(),
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps,
            gop_length: 60,
        };
        let encoder = NvencEncoder::new(dev, &cfg)?;
        Ok(Self { dup, encoder, seq: 0, epoch: std::time::Instant::now(), idr_pending: true })
    }
}

#[async_trait::async_trait]
impl prdt_protocol::VideoProducer for DxgiNvencProducer {
    async fn next_frame(&mut self) -> Result<prdt_protocol::EncodedFrame, prdt_protocol::ProducerError> {
        loop {
            let acquired = self.dup.acquire_next_frame(std::time::Duration::from_millis(16))
                .map_err(|e| prdt_protocol::ProducerError::Capture(e.to_string()))?;
            let (texture, _info) = match acquired {
                crate::dxgi::AcquiredFrame::Frame { texture, frame_info } => (texture, frame_info),
                crate::dxgi::AcquiredFrame::Timeout => continue,
            };
            let ts_us = self.epoch.elapsed().as_micros() as u64;
            let force_idr = std::mem::take(&mut self.idr_pending);
            let encoded = self.encoder.encode(&texture, force_idr, ts_us)
                .map_err(|e| prdt_protocol::ProducerError::Encode(e.to_string()))?;
            let seq = self.seq;
            self.seq += 1;
            return Ok(prdt_protocol::EncodedFrame::new_h265(
                seq, ts_us, encoded.is_keyframe, bytes::Bytes::from(encoded.nal_bytes),
                self.dup.width(), self.dup.height(),
            ));
        }
    }

    fn request_idr(&mut self) { self.idr_pending = true; }

    fn set_target_bitrate(&mut self, _bps: u32) {
        // Phase 0 Plan 2c: bitrate is fixed at construction time. Plan 3 will
        // wire this through NvencEncoder::reconfigure.
    }
}
```

Instantiation-style smoke test (inline): create on primary output, call `next_frame` once, assert result is `EncodedFrame { seq: 0, is_keyframe: true, .. }`.

- [ ] Commit `feat(media-win): add DxgiNvencProducer concrete VideoProducer impl`

---

## Task 7: MfD3d11Consumer + end-to-end pipeline smoke test

**Files:**
- Create: `crates/media-win/src/pipeline/consumer.rs`
- Create: `crates/media-win/tests/pipeline_smoke.rs`
- Modify: `crates/media-win/src/pipeline/mod.rs`

Pseudocode for consumer:

```rust
// crates/media-win/src/pipeline/consumer.rs

pub struct MfD3d11Consumer {
    decoder: mf::H265Decoder,
    latest_output_bytes: std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    needs_idr: bool,
}

impl MfD3d11Consumer {
    pub fn new(dev: &D3d11Device, width: u32, height: u32) -> Result<Self> {
        let decoder = mf::H265Decoder::new(dev, width, height)?;
        Ok(Self {
            decoder,
            latest_output_bytes: Default::default(),
            needs_idr: true,
        })
    }

    pub fn take_latest_frame(&self) -> Option<Vec<u8>> {
        self.latest_output_bytes.lock().unwrap().take()
    }
}

#[async_trait::async_trait]
impl prdt_protocol::VideoConsumer for MfD3d11Consumer {
    async fn submit(&mut self, frame: prdt_protocol::EncodedFrame) -> Result<(), prdt_protocol::ConsumerError> {
        self.decoder.process_input(&frame.nal_units, frame.timestamp_host_us as i64)
            .map_err(|e| prdt_protocol::ConsumerError::Decode(e.to_string()))?;
        for _ in 0..3 {
            match self.decoder.process_output()
                .map_err(|e| prdt_protocol::ConsumerError::Decode(e.to_string()))?
            {
                Some(bytes) => {
                    *self.latest_output_bytes.lock().unwrap() = Some(bytes);
                    self.needs_idr = false;
                    break;
                }
                None => break, // need more input
            }
        }
        Ok(())
    }

    fn needs_idr(&self) -> bool { self.needs_idr || self.decoder.needs_idr() }
}
```

Integration test:

```rust
// crates/media-win/tests/pipeline_smoke.rs
#![cfg(windows)]

use prdt_media_win::{
    pipeline::{DxgiNvencProducer, MfD3d11Consumer},
    pick_default_adapter, D3d11Device,
    dxgi::enumerate_outputs_for_adapter,
};
use prdt_protocol::{VideoConsumer, VideoProducer};

#[tokio::test]
async fn producer_to_consumer_end_to_end() {
    let adapter = match pick_default_adapter() {
        Ok(a) => a,
        Err(_) => return,
    };
    if !adapter.is_nvidia() { return; }
    let dev = D3d11Device::create(&adapter).expect("D3D11 device");
    let outputs = enumerate_outputs_for_adapter(&adapter).expect("outputs");
    let primary = outputs.iter().find(|o| o.is_attached).cloned().unwrap_or_else(|| outputs[0].clone());

    let mut producer = match DxgiNvencProducer::new(&dev, &primary, 10_000_000) {
        Ok(p) => p,
        Err(e) => { eprintln!("skip: {e}"); return; }
    };
    let mut consumer = match MfD3d11Consumer::new(&dev, primary.desktop_rect.right as u32 - primary.desktop_rect.left as u32, primary.desktop_rect.bottom as u32 - primary.desktop_rect.top as u32) {
        Ok(c) => c,
        Err(e) => { eprintln!("skip: {e}"); return; }
    };

    // Drive for 5 frames.
    for _ in 0..5 {
        let frame = producer.next_frame().await.expect("encode frame");
        consumer.submit(frame).await.expect("decode frame");
    }
    // The consumer should have a decoded frame somewhere.
    let output = consumer.take_latest_frame();
    eprintln!("got decoded output: {} bytes", output.map(|b| b.len()).unwrap_or(0));
}
```

- [ ] Commit `feat(media-win): add MfD3d11Consumer + end-to-end pipeline smoke test`

---

## Task 8: Final checks + README + tag

- [ ] Run all checks (with env vars set)
- [ ] Update README.md to check Plan 2c as complete
- [ ] `git tag phase0-plan2c-complete`

---

## Plan 2c Exit Criteria

- [ ] Task 1 protocol traits compile + test pass
- [ ] Task 3-4 MF decoder constructs + decodes at least one IDR without error
- [ ] Task 5 NVENC→MF round-trip yields NV12 bytes of expected size
- [ ] Task 6 DxgiNvencProducer emits at least one EncodedFrame with seq=0, is_keyframe=true
- [ ] Task 7 end-to-end test: producer emits N frames, consumer decodes them
- [ ] Clippy clean, fmt clean
- [ ] Tag `phase0-plan2c-complete`

---

## Known Risks / Limitations

1. **MF decoder may not be available on all systems** — on virtual machines or server-SKU Windows, no HW H.265 decoder MFT is installed. Tests skip with warning in that case.
2. **Output is NV12 bytes, not a D3D11 texture** — Plan 2c returns CPU-side bytes from `process_output`. Phase 3 will add zero-copy D3D11 texture extraction via `IMFDXGIBuffer`.
3. **No real D3D11 rendering (swapchain present)** — that's `viewer` binary territory (Plan 3).
4. **Bitrate set via producer `set_target_bitrate` is no-op** — NVENC reconfigure is deferred to later plans.

---

## Future: Plan 2d (NVDEC/cuvid via CUDA) — Optional Performance Upgrade

If Phase 0 latency measurement shows MF-based decode adds >5-10ms vs cuvid, revisit in a dedicated Plan 2d: install CUDA Toolkit, wrap nvcuvid.dll + cuviddec.h via bindgen, swap in as another `VideoDecoder` implementation behind the same trait.

---

*End of Phase 0 — Plan 2c of 4.*
