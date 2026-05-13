# P5C-1: VAAPI H.264 Encoder (Linux HW codec, minimal viable) — Design

**Status:** Draft (2026-05-13)
**Predecessor:** `phase-p5b2c-cursor-hide-polish-rebuild` (master HEAD, commit `4b5739f`)
**Branch:** `phase-p5c1-vaapi-h264-encoder`

## 1. Goal

Land a working **VAAPI H.264 encoder** as a new Linux backend so hosts with Intel iGPU or AMD APU stop burning CPU on OpenH264 SW encode. Cross-platform CI continues to gate; the actual VAAPI runtime is verified via manual walkthrough on real hardware.

## 2. Constraints (locked in)

| Constraint | Choice | Why |
|---|---|---|
| Crate | `cros-libva 0.0.13` | Safe RAII wrappers (Display/Config/Context/Surface/Buffer); actively maintained (chromeos/cros-libva, last update 2024-12, 466K downloads). `libva-sys` is stale (2021); `fev` self-marks unsound; `cros-codecs` is a heavyweight pipeline — useful reference but not direct dep |
| Public API | `encode(&I420Frame, force_idr, ts_us) → Result<EncodedFrame>` (matches existing `SwH264Encoder` trait) | Minimal disruption to `LinuxSwProducer`; existing BGRA→I420 CPU step reused |
| Internal seam | `FrameInput::CpuI420 | VaSurface | Dmabuf` enum from day 1; only `CpuI420` arm implemented in P5C-1 | Lets P5C-2 (DMABUF zero-copy) slot in without touching the encoder state machine |
| Driver matrix | **Intel iHD + AMD radeonsi via Mesa libva**; NVIDIA excluded | `nvidia-vaapi-driver` README explicitly states decode-only. NVENC-Linux is P5C-3 |
| Profile | `VAProfileH264ConstrainedBaseline` | Maximum encoder compatibility across Intel + AMD generations; matches OpenH264 baseline output |
| Rate control | CBR via `VA_RC_CBR` with capability probe (`get_config_attributes`) before `create_config`; dynamic bitrate update via per-frame `VAEncMiscParameterRateControl` swap within the same RC mode | CBR↔VBR switching needs re-`create_config` on some drivers (trap; deferred) |
| Output format | Annex-B with `00 00 00 01` start codes + SPS/PPS prepended | Matches OpenH264 output so downstream `prdt-protocol` unchanged |
| Build env | `scripts/dev-container.sh` extended with `libva-dev` (+ `libva-drm2`, `libva-x11-2` runtime) | Debian bookworm has all three. Real VAAPI device tests deferred to walkthrough (container lacks `/dev/dri/*`) |
| DoD | Auto-evidence (container clippy + affected-crate lib tests + unit tests on FFI glue/state machine/bitstream normalizer) + walkthrough | Same scope as P5B-2a/2b/2c. Real-device smoke = user-side manual |
| Out of scope | DMABUF zero-copy, NVENC-Linux, VAAPI decode, V4L2 M2M, AMD-specific tuning, AVCC output, multi-slice encoding | Each becomes its own subphase (P5C-2/3/4/5) |

## 3. Architecture

### 3.1 New crate: `crates/media-vaapi/`

Self-contained crate so the Linux HW path can be feature-gated independent of the SW path. Workspace member; consumed by `prdt-media-linux` as a Linux-only dep.

```
crates/media-vaapi/
├── Cargo.toml          (cros-libva 0.0.13, prdt-media-sw types, thiserror, tracing)
├── src/
│   ├── lib.rs          (public re-exports: VaapiH264Encoder, VaapiError, FrameInput)
│   ├── display.rs      (RAII Display open, capability probe, render-node probe)
│   ├── encoder.rs      (VaapiH264Encoder struct + impl)
│   ├── frame_input.rs  (FrameInput enum; CpuI420 only in P5C-1)
│   ├── annexb.rs       (NAL start-code normalizer + SPS/PPS prepender)
│   ├── error.rs        (VaapiError + VAStatus → Result mapping)
│   └── rc.rs           (Rate-control parameter buffer builder)
```

### 3.2 Public API

```rust
// crates/media-vaapi/src/lib.rs
pub use encoder::VaapiH264Encoder;
pub use error::VaapiError;
pub use frame_input::FrameInput;

// crates/media-vaapi/src/encoder.rs
pub struct VaapiH264EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub initial_bitrate_bps: u32,
    pub gop_size: u32,           // distance between IDRs (default 60)
    pub render_node: Option<std::path::PathBuf>,  // None = auto-pick first /dev/dri/renderD*
}

pub struct VaapiH264Encoder { /* private */ }

impl VaapiH264Encoder {
    pub fn new(cfg: VaapiH264EncoderConfig) -> Result<Self, VaapiError>;
    pub fn encode(
        &mut self,
        frame: &prdt_media_sw::I420Frame,
        force_idr: bool,
        ts_us: u64,
    ) -> Result<prdt_media_sw::EncodedFrame, VaapiError>;
    pub fn set_target_bitrate(&mut self, bps: u32) -> Result<(), VaapiError>;
    pub fn backend_name(&self) -> &'static str { "vaapi-h264-cbr-baseline" }
}

// crates/media-vaapi/src/frame_input.rs
/// Discriminator for encoder input. Only CpuI420 has an arm in P5C-1.
/// VaSurface/Dmabuf are reserved for P5C-2 (zero-copy).
pub enum FrameInput<'a> {
    CpuI420(&'a prdt_media_sw::I420Frame),
    VaSurface(/* P5C-2: wrapper around libva::Surface */),
    Dmabuf(/* P5C-2: { fds, planes, modifier, ... } */),
}
```

### 3.3 Internal layout (encoder.rs)

```
VaapiH264Encoder {
    // FIELD ORDER MATTERS (Drop runs in declaration order).
    // Inner state types each hold Option<...> so we can Option::take() in
    // explicit reverse order inside an explicit Drop impl, guaranteeing
    // image → coded → surface → context → config → display teardown.
    state: Option<EncoderState>,
}

struct EncoderState {
    pending_coded_bufs: Vec<libva::Buffer>,
    surfaces: SurfacePool,                  // pre-allocated NV12 surfaces
    sequence: SequenceParams,                // SPS-equivalent
    rc_target_bps: u32,
    frames_emitted: u64,
    idr_pic_id: u16,
    context: std::rc::Rc<libva::Context>,    // outlives surfaces
    config: libva::Config,                   // outlives context
    display: std::rc::Rc<libva::Display>,    // outlives all
}
```

### 3.4 Drop order policy (load-bearing)

cros-libva's Display is held as `Rc<Display>` (cloned into Context + Surface). Per Codex finding: explicit `Drop` impl using `Option::take()` to force the order:

1. mapped coded buffers / VAImage handles
2. per-request `libva::Buffer` objects (RC, slice, picture params)
3. SurfacePool (each surface holds an Rc<Display> ref but freeing requires Context to be alive)
4. `Rc<Context>`
5. `libva::Config`
6. `Rc<Display>`

`#[derive(Debug)]` for `VaapiH264Encoder` is fine but `Drop` MUST be manual.

### 3.5 Encode-loop state machine

```
encode(frame, force_idr, ts):
  1. Acquire a free Surface from the pool (block-or-error if pool empty).
  2. upload_i420_to_surface(frame, &surface):
       - vaDeriveImage → fast path; else vaCreateImage + vaPutImage
       - Y/U/V plane row-copy to mapped image
       - Unmap before submit.
  3. Build picture params (idr_pic_flag = self.frames_emitted == 0 || force_idr).
  4. Build slice params (slice_type = I if IDR else P; first_mb_in_slice=0).
  5. Build rate-control misc param (only if bitrate changed since last frame).
  6. Create coded output Buffer (size hint = w*h*4 to be safe).
  7. vaBeginPicture / vaRenderPicture / vaEndPicture, sync.
  8. Map coded buffer → walk segments → normalize_to_annexb:
       - Prepend SPS+PPS for IDR (or every-IDR via packed_headers flag).
       - Convert any 3-byte start codes to 4-byte (00 00 00 01).
  9. Build EncodedFrame { seq, nal_units, is_keyframe, ts_us }.
  10. Recycle surface to pool, increment counters.
```

### 3.6 Annex-B normalizer (`annexb.rs`)

VAAPI coded buffer layout is driver-dependent. Per Codex: SPS/PPS may be embedded or omitted; start codes may be 3-byte or 4-byte; multi-segment buffers are possible. Normalizer:

```rust
/// Walk the coded buffer's NAL units and re-emit with consistent 4-byte
/// Annex-B start codes. If the first IDR is missing SPS/PPS, prepend them
/// from the cached `sps_pps` blob captured at encoder init via packed
/// headers.
pub fn normalize_to_annexb(raw: &[u8], sps_pps: &[u8], is_idr: bool, out: &mut Vec<u8>);
```

Tests cover:
- 3-byte start code → 4-byte conversion
- SPS/PPS prepend on IDR
- Multi-segment coded buffer concat
- Empty input (encoder failed) → error

### 3.7 Error model (`error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum VaapiError {
    #[error("display open failed: {0}")]
    DisplayOpen(String),
    #[error("no /dev/dri/renderD* found")]
    NoRenderNode,
    #[error("configuration not supported: {0}")]
    NotSupported(String),
    #[error("hardware busy (retry exhausted, attempts={attempts})")]
    HardwareBusy { attempts: u32 },
    #[error("driver returned VA_STATUS_ERROR_{0}")]
    DriverError(i32),
    #[error("bitstream normalization failed: {0}")]
    Bitstream(String),
    #[error("encoder closed (call new() to reopen)")]
    Closed,
}

pub(crate) fn map_va_status(status: i32, ctx: &'static str) -> Result<(), VaapiError> {
    match status {
        VA_STATUS_SUCCESS => Ok(()),
        VA_STATUS_ERROR_HW_BUSY | VA_STATUS_ERROR_TIMEDOUT => {
            // caller decides retry
            Err(VaapiError::HardwareBusy { attempts: 0 })
        }
        VA_STATUS_ERROR_OPERATION_FAILED | VA_STATUS_ERROR_INVALID_CONFIG => {
            Err(VaapiError::NotSupported(format!("{ctx}: status={status}")))
        }
        _ => Err(VaapiError::DriverError(status)),
    }
}
```

Retry policy (in `encode()` only; init failures are hard fails): exponential backoff `0.5ms → 1ms → 2ms → 4ms → 8ms`, max 5 attempts. After exhaustion: surface `HardwareBusy`. Caller (`LinuxVideoProducer`) translates to `ProducerError::DeviceLost` per the P5A `DeviceLost` route, triggering OpenH264 SW fallback.

### 3.8 Policy / factory integration (`crates/media-linux` + `crates/media-policy`)

**`media-policy/src/capability.rs`** — add to `BackendKind`:

```rust
pub enum BackendKind {
    // ...
    Vaapi,  // NEW
}
```

**`media-policy/src/capability.rs::EncoderCapability`** — registered by Linux probe:

```rust
EncoderCapability {
    backend: BackendKind::Vaapi,
    codec: Codec::H264,
    priority: 90,                  // between Windows NVENC (100) and OpenH264 (50)
    zero_copy: false,              // P5C-2 will flip
    max_resolution: (3840, 2160),
    min_bitrate_bps: 100_000,
    requires_d3d11: false,
}
```

**`media-linux/src/policy.rs`** — update Linux probe:

```rust
fn list_encoders(&self) -> Vec<EncoderCapability> {
    let mut out = vec![/* existing OpenH264 entry */];
    if vaapi_runtime_present() {
        out.push(/* Vaapi entry above */);
    }
    out
}

fn vaapi_runtime_present() -> bool {
    // 1. /dev/dri/renderD* exists?
    // 2. libva can open it?
    // 3. The opened device advertises VAProfileH264ConstrainedBaseline + VAEntrypointEncSlice?
    // Cache result for the session.
}
```

`LinuxSwFactory::create` gets a `BackendKind::Vaapi` arm that constructs a `VaapiVideoProducer` (analogous to `LinuxSwProducer` but wrapping `VaapiH264Encoder`).

**Naming**: `LinuxSwFactory` is renamed to `LinuxVideoFactory` (Gemini's suggestion — accurate now that it dispatches both SW and HW backends). Re-export the old name as deprecated alias for one release cycle. Actually: too much churn for a minor naming gripe. **Decision: keep `LinuxSwFactory` name; document the misnomer in a comment.**

### 3.9 `LinuxVideoProducer` wiring

New `crates/media-linux/src/vaapi_pipeline.rs`:

```rust
pub struct VaapiVideoProducer {
    capture: Box<dyn CaptureSource>,
    encoder: prdt_media_vaapi::VaapiH264Encoder,
    bgra_scratch: Vec<u8>,
    i420_scratch: prdt_media_sw::I420Frame,
    sequence_counter: u64,
}

impl prdt_protocol::VideoProducer for VaapiVideoProducer {
    async fn next_frame(&mut self) -> Result<prdt_protocol::EncodedFrame, ProducerError>;
    fn request_idr(&mut self);
    fn set_target_bitrate(&mut self, bps: u32);
    fn backend_name(&self) -> &'static str { "linux-vaapi-h264" }
}
```

Internal `next_frame`:
1. `spawn_blocking { capture.capture_into(&mut bgra_scratch) }` (same pattern as `LinuxSwProducer`)
2. `bgra_to_i420(&bgra_scratch, w, h, stride, &mut i420_scratch)` (reuses existing CPU SIMD path)
3. `spawn_blocking { encoder.encode(&i420_scratch, force_idr, ts_us) }`
4. Map `VaapiError::HardwareBusy` → `ProducerError::DeviceLost` (triggers PolicyDriven SW fallback)

### 3.10 CLI surface

`prdt host --encoder vaapi` becomes a valid choice on Linux. The existing `--encoder auto` (P5A SelectionPolicy) auto-picks `vaapi` when probe succeeds. No new CLI flags.

## 4. Dev container

`scripts/Dockerfile.dev` apt list extended with:

```
libva-dev libva-drm2 libva-x11-2
```

Container can `cargo build --release -p prdt-client --target x86_64-unknown-linux-gnu` cleanly. Real VAAPI device tests run on the user's machine (container has no `/dev/dri/*`).

## 5. Tests

| Layer | Test | Notes |
|---|---|---|
| Unit | `annexb::normalize_appends_4byte_start_code` | Fixed-byte test vectors |
| Unit | `annexb::normalize_collapses_3byte_to_4byte` | |
| Unit | `annexb::normalize_prepends_sps_pps_on_idr` | |
| Unit | `error::map_va_status_classifies_hw_busy` | |
| Unit | `error::map_va_status_classifies_not_supported` | |
| Unit | `frame_input::cpu_i420_holds_borrow_lifetime` | Smoke test for enum |
| Integration | `vaapi_pipeline::producer_falls_back_on_device_lost` | Inject a stubbed encoder that returns `HardwareBusy` |
| Integration | `policy::probe_returns_empty_when_no_render_node` | Run inside container (no `/dev/dri/*`) |

Total: **~8 new tests** within container reach. Real VAAPI runtime tests = walkthrough.

## 6. Walkthrough (real-device verification)

Append §K to `docs/superpowers/p5b1-smoke-walkthrough.md`:

1. Verify VAAPI driver: `vainfo | grep H264ConstrainedBaseline`
2. Verify `/dev/dri/renderD128` (or 129/130) exists and is accessible to the user
3. Start host: `./prdt host --encoder vaapi --bitrate-mbps 5 --silent-allow 2>&1 | tee p5c1.log`
4. Expect log: `vaapi encoder initialized: driver=intel-iHD profile=ConstrainedBaseline`
5. Connect viewer: `./prdt connect --host <ip>:9000 --decoder openh264 --codec h264`
6. Verify frame flow at ≥30 fps in viewer
7. **CPU usage check**: `pidstat -p $(pgrep -f prdt) 1 30` — expect host %CPU << OpenH264 baseline
8. **Bitrate update**: from viewer adjust bitrate slider; host log shows `set_target_bitrate 8000000 → 8 Mbps`
9. **Failure fallback**: pull DRI permission with `sudo chmod 000 /dev/dri/renderD128`; verify host falls back to OpenH264 with warn log

## 7. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | Driver crashes / freezes during encode session | RAII Drop runs `vaDestroy*` cleanly; OS recovers DRI device |
| 2 | `vaPutImage` is slow → eats latency budget | Mitigated by `vaDeriveImage` fast path attempt first; CPU upload acceptable for 1080p60 |
| 3 | Annex-B normalizer misses driver-specific quirk | Multi-driver smoke walkthrough on Intel + AMD (deferred to user) |
| 4 | `cros-libva` API breaks in next minor bump | Pin to `=0.0.13` for now, audit on each future bump |
| 5 | Container clippy passes but runtime fails on real device | Walkthrough is the gate; user reports issues |
| 6 | Encoder thread blocks tokio runtime | `spawn_blocking` for the encode call, matches OpenH264 pattern |
| 7 | Multi-stream / future audio path needs different libva display | Out of scope; encoder owns its own Display |

## 8. References

- **`cros-libva`** docs.rs: <https://docs.rs/cros-libva/0.0.13>
- **`cros-codecs` H.264 backend**: `src/encoder/stateless/h264/vaapi.rs#L398–L444`
- **`cros-libva` encode demo**: `lib/src/lib.rs#L271`
- **GStreamer VAAPI H.264** (idr_pic_flag + slice_type + packed headers): `gst-libs/gst/vaapi/gstvaapiencoder_h264.c#L2247–L2301`
- **libva-utils h264encode** (RC capability + IDR): `encode/h264encode.c#L1176, L1615, L1858`
- **OBS VAAPI** (RC availability probe): `plugins/obs-ffmpeg/obs-ffmpeg-vaapi.c#L876, L951`
- **libva spec**: <https://intel.github.io/libva/group__api__core.html>
- **`nvidia-vaapi-driver` README** (decode-only confirmation): <https://github.com/elFarto/nvidia-vaapi-driver/blob/master/README.md>

## 9. Open ambiguities (resolve in plan)

1. **Surface pool size**: 4 (Codex's example) vs dynamic per-fps? Plan picks **4** as P5B-1's RawFrame channel cap was 2 and encoder needs a couple extra in flight.
2. **`vaapi_runtime_present()` caching**: per-process Once vs per-call? Plan picks **Once** with reset on `set_target_bitrate` succeeding (cheap signal that the device is live).
3. **`vaDeriveImage` fallback**: always try first then fall to `vaCreateImage` + `vaPutImage`? Plan picks **always**, with one-time warn log on fallback.
4. **`packed_headers` flag**: use the libva `VAEncPackedHeaderSequence` / `VAEncPackedHeaderPicture` path to embed SPS/PPS inline, OR manually prepend in normalizer? Plan picks **manually prepend** (simpler; one less FFI surface to test).
5. **Bitrate floor probing**: hard-coded `min_bitrate_bps = 100_000` (Intel/AMD typical floor) OR runtime probe via `VAConfigAttribEncBitRateControl::min`? Plan picks **hard-coded** for P5C-1; runtime probe is a follow-up.
6. **`renderD128 vs 129 vs 130` auto-pick**: first matching node? Plan picks **first node that opens AND advertises H264 EncSlice**.
