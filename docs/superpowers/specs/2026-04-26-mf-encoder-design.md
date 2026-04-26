# Windows MF H.265 Encoder Fallback — Design Spec

**Date:** 2026-04-26
**Tag (on completion):** `mf-encoder-fallback-complete`
**Scope:** Add a Media Foundation H.265 encoder MFT path to `prdt-media-win` so non-NVIDIA Windows GPUs (AMD / Intel) can run the host bin. Refactor `NvencEncoder` and the new `MfH265Encoder` behind a shared `Hevc265Encoder` trait. Producer layer selects encoder based on adapter capability or explicit override.

## Goal

A single Windows host build that runs the same H.265 transport pipeline on:

- NVIDIA GPUs → NVENC (existing path, lowest latency)
- AMD / Intel GPUs → MF H.265 encoder MFT (new path, slightly higher latency)
- No GPU at all → fail fast (no software encoder)

The two HW paths are interchangeable at the bitstream level — both emit
Annex-B H.265 NAL units consumable by the existing MF / NVDEC decoders
without any transport-layer change.

## Non-goals

- **DX12 Video Encode**: future work; design leaves a clean trait extension point for it but does not implement
- **AV1**: blocked on Ada Lovelace+ NVENC AV1 or DX12 Video Encode AV1 path
- **Software encoder fallback** (x264 / x265 / SVT): too slow for real-time, GPL licence problems
- **Linux MF**: MF is Windows-only; Linux gets VA-API in Phase 1 (out of scope here)
- **Multi-encoder simultaneous use** (e.g. dual-output): one encoder per producer
- **Dynamic encoder switching at runtime**: encoder is fixed at producer construction
- **Encoder-specific tuning surface beyond bitrate**: NVENC presets / MF properties stay internal

## Verified context (read 2026-04-26)

- `crates/protocol/src/video_pipeline.rs:34` — `VideoProducer` trait (capture+encode); `next_frame() -> Result<EncodedFrame, _>`, `request_idr()`, `set_target_bitrate(u32)`
- `crates/media-win/src/nvenc/encoder.rs:45` — `EncodedH265Frame { nal_bytes: Vec<u8>, is_keyframe: bool, timestamp: u64 }` (already codec-agnostic in shape)
- `crates/media-win/src/nvenc/encoder.rs:208` — `NvencEncoder::encode(&self, &D3d11Texture, force_idr, timestamp_us) -> Result<EncodedH265Frame>`
- `crates/media-win/src/mf/decoder.rs:48-52` — MF runtime init pattern (`OnceLock` + `CoInitializeEx + MFStartup(MF_VERSION, MFSTARTUP_FULL)`)
- `crates/media-win/src/pipeline/producer.rs:93` — `impl VideoProducer for DxgiNvencProducer`
- `crates/host/src/main.rs:138` — `pick_default_adapter()` returns first adapter (any vendor); current code then unconditionally constructs `DxgiNvencProducer` which fails on non-NVIDIA

## Architecture

### New trait

```rust
// crates/media-win/src/encoder_trait.rs (new file)
pub trait Hevc265Encoder: Send {
    /// Encode a B8G8R8A8 D3D11 texture into an H.265 access unit.
    /// `force_idr == true` requests an IDR + parameter sets.
    fn encode(
        &mut self,
        texture: &D3d11Texture,
        force_idr: bool,
        timestamp_us: u64,
    ) -> Result<EncodedH265Frame, MediaError>;

    /// Best-effort target bitrate update.
    fn set_target_bitrate(&mut self, bps: u32);

    /// Encoder identity for logging / bench output.
    fn backend_name(&self) -> &'static str;
}
```

`EncodedH265Frame` stays as defined in `nvenc/encoder.rs` (move it to
`encoder_trait.rs` so it is not nvenc-specific).

### Implementations

| Type | File | Notes |
|---|---|---|
| `NvencEncoder` | `nvenc/encoder.rs` (existing) | Add `impl Hevc265Encoder`; behaviour unchanged |
| `MfH265Encoder` | `mf/encoder.rs` (new) | Wraps the OS H.265 encoder MFT with a D3D11 device manager |

`encode()` signature is shared so the producer layer doesn't care which
backend is in use.

### Producer dispatch

```rust
// crates/media-win/src/pipeline/producer.rs
pub enum HwHevcEncoder {
    Nvenc(NvencEncoder),
    Mf(MfH265Encoder),
}

impl Hevc265Encoder for HwHevcEncoder {
    fn encode(...) { match self { ... } }
    fn set_target_bitrate(...) { match self { ... } }
    fn backend_name(&self) -> &'static str { match self { ... } }
}
```

`DxgiNvencProducer` → renamed to `DxgiHevcProducer` and parametrised
on the enum (renaming preserves git blame; old name kept as a `pub use`
alias for one minor version).

### Encoder selection

```rust
// crates/host/src/main.rs (sketch)
let encoder = match args.encoder {
    EncoderChoice::Auto => choose_encoder_for_adapter(&adapter, &dev, ...)?,
    EncoderChoice::Nvenc => NvencEncoder::new(&dev, &cfg)?.into(),
    EncoderChoice::Mf => MfH265Encoder::new(&dev, &cfg)?.into(),
};
```

`choose_encoder_for_adapter` rule:
1. If `adapter.is_nvidia()` → NVENC
2. Else → MF
3. If MF init also fails → bail with a message naming the adapter vendor

### CLI

The host bin gains:

```
--encoder <auto|nvenc|mf>      # default: auto
```

Stored on `Config.host.encoder` for persistence across runs. Default
left empty in config so existing config.toml stays valid (auto wins).

## MF encoder details (the part that's new)

### MFT enumeration

Use `MFTEnumEx` to find a hardware `MFVideoFormat_HEVC` encoder MFT.
Filter: `MFT_CATEGORY_VIDEO_ENCODER` + output type
`MFVideoFormat_HEVC` + flags `MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT`.

If no hardware MFT is found, fall back to `MFT_ENUM_FLAG_SYNCMFT`
(software-assisted encoder; usable but slow).

### Input format

NVENC accepts B8G8R8A8 directly. The MF H.265 encoder MFTs typically
accept NV12 input (some accept B8G8R8A8 too — query `IMFTransform::GetInputAvailableType`).

To minimise GPU work the implementation queries supported input formats
in this order:

1. `MFVideoFormat_ARGB32` (= B8G8R8A8) — preferred, no colour conversion
2. `MFVideoFormat_NV12` — fall back if ARGB32 not supported. Add a
   D3D11 BGRA → NV12 conversion step using the existing
   `Nv12Renderer` infrastructure or a dedicated compute shader (TBD
   during implementation; viable if zero-copy ARGB32 path is missing)

If neither ARGB32 nor NV12 is supported, init fails with a clear
error.

### Low-latency configuration

Set on the MFT:

- `CODECAPI_AVLowLatencyMode = TRUE`
- `MF_LOW_LATENCY = TRUE` (set on output `IMFMediaType` attributes)
- `CODECAPI_AVEncCommonRateControlMode = eAVEncCommonRateControlMode_CBR`
- `CODECAPI_AVEncCommonMeanBitRate = bps`
- `MF_MT_FRAME_RATE` (numerator/denominator)
- `MF_MT_INTERLACE_MODE = MFVideoInterlace_Progressive`

GOP / B-frames: configure for IDR-on-request + zero B-frames if the MFT
exposes `CODECAPI_AVEncH265CABACEnable` etc. If not, accept defaults.

### Output

The MFT emits an `IMFSample` carrying an `IMFMediaBuffer` with the
H.265 Annex-B NAL bytes. Drain via `ProcessOutput`. Detect IDR by
checking the sample's `MFSampleExtension_CleanPoint` attribute.

### MF runtime init

Reuse the existing pattern: `OnceLock` guarding `CoInitializeEx` +
`MFStartup`. The decoder side already does this; the new encoder
shares the same lazy init. Move the init helper out of `mf/decoder.rs`
into `mf/mod.rs` as `pub(crate) fn ensure_mf_runtime() -> Result<()>`.

## DX12 extension hook (no implementation now)

The trait shape `Hevc265Encoder` deliberately accepts `&D3d11Texture`.
DX12 Video Encode requires `&D3d12Resource`, which is incompatible.

When DX12 is added:

1. New trait `Dx12Hevc265Encoder` in `encoder_trait.rs` taking
   `&D3d12Resource`
2. New producer `Dx12CaptureDx12HwProducer` paralleling
   `DxgiHevcProducer`
3. Selection happens at the producer level, not the encoder trait
   level

This decision is intentional: pretending to abstract over D3D11 and
D3D12 textures via a generic trait would force the project into a
D3D12 migration. The clean separation lets each rendering generation
have its own pipeline.

## Tests

### New unit tests

- `MfH265Encoder::new` succeeds when MF runtime is available (smoke,
  not CI)
- `Hevc265Encoder` enum dispatch returns the right `backend_name()`
  for both variants
- Bench-matrix with `--encoders nvenc,mf` produces 2 rows on NVIDIA
  hardware (smoke; CI cannot run NVENC anyway, so this is manual)

Most testing is end-to-end via existing test infrastructure plus a
new bench-matrix axis.

### Bench-matrix integration

`prdt-bench-matrix` already has a `--decoders mf,nvdec` axis. Add an
analogous `--encoders nvenc,mf` axis. With both axes enabled the
default sweep grows by 2× — keep the default encoder list
`[nvenc]` (current behaviour) and let users opt in.

## Out-of-scope error paths

- AMD AMF or Intel oneVPL native bindings: the MF MFT already
  delegates to the right vendor driver under the hood. Direct SDK
  access would shave 1-2 ms but is not worth the complexity here.
- macOS VideoToolbox: orthogonal; macOS host is Phase 5+ if at all.
- Hardware acceleration fallback chain ("try NVENC, then MF, then
  fail"): the user gets one clear `--encoder` choice. Auto-mode
  picks based on adapter vendor only.

## Exit criteria

1. `cargo build -p prdt-media-win` clean
2. `cargo test -p prdt-media-win` passes (existing tests + new
   trait dispatch tests)
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
4. Existing `prdt-host.exe` on NVIDIA hardware behaves identically
   (NVENC selected by default)
5. Manual smoke: `prdt-host.exe --encoder mf` on the same NVIDIA
   machine starts successfully and the viewer renders frames (proves
   the MF path works even when NVENC is available)
6. `docs/encoders.md` describes the auto-selection rules and the
   `--encoder` flag
7. STATUS.md updated
8. tag `mf-encoder-fallback-complete`

Manual smoke on actual AMD / Intel hardware is documented as a
prerequisite for Phase 5 release but is not required for this tag.

## Estimate

- spec (this doc): 0.25 d
- plan: 0.5 d
- trait extraction + NVENC impl: 0.25 d
- MF encoder implementation: 1.5-2 d (the meat of the work)
- Producer dispatch + CLI flag + Config.host.encoder: 0.5 d
- bench-matrix axis: 0.25 d
- docs + tag: 0.25 d
- total: ~3.5 d

The bulk of the time is the MF encoder MFT plumbing and the format
negotiation (ARGB32 vs NV12). Everything else is mechanical.

## Risks & open questions

- **MF H.265 encoder MFT availability on Windows 10**: HEVC Video
  Extensions ship the decoder MFT but the encoder MFT depends on the
  GPU driver. On NVIDIA + recent driver the MFT exists and dispatches
  to NVENC under the hood. On AMD / Intel similar. Verify on the
  development machine via `MFTEnumEx`.
- **ARGB32 support varies**: some MFTs only accept NV12. If the
  primary path requires NV12, factor in a colour conversion step
  (small D3D11 compute shader). Plan should include a small test
  config that exercises both paths.
- **Latency vs NVENC**: expect MF encoder p50 ~10-15 ms higher.
  Document in `docs/encoders.md` and STATUS.md so users understand
  the tradeoff.
- **`Config.host.encoder` default**: if a config.toml from a pre-fix
  install lacks the field, `serde(default)` should fall back to
  `"auto"`. Verify the existing `serde(default)` pattern at
  `crates/gui-common/src/config.rs` is in use here.
- **Hot-swap**: switching encoders at runtime is out of scope for
  this work. Producer is rebuilt only on stop-restart of the host
  task.
