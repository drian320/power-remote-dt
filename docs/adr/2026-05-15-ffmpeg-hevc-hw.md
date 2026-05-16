# ADR: FFmpeg integration for H.265 (HEVC) hardware acceleration

- **Status:** Proposed (2026-05-15)
- **Tag:** _(unassigned — assigned on acceptance)_
- **Supersedes (partial):** `2026-04-27-software-codec-openh264.md` — Alternative B (FFmpeg via `ffmpeg-next` / `rsmpeg`) was rejected for **software** codec use. This ADR keeps that rejection intact for SW codecs and only carves out a narrow **HW HEVC** opening.
- **Plan:** _(to be written — `docs/superpowers/plans/2026-05-15-ffmpeg-hevc-hw.md`)_
- **Deciders:** CCG advisor consensus (Codex architecture + Gemini UX/alternatives), 2026-05-15

## Context

Today `power-remote-dt` has the following codec stack:

| Platform | H.265 encode | H.265 decode | H.264 encode | H.264 decode |
|---|---|---|---|---|
| Windows + NVIDIA | NVENC (native SDK) | NVDEC / MF | NVENC | MF / NVDEC |
| Windows + Intel / AMD | _(none)_ | MF (HEVC Extensions) | _(none)_ | MF / OpenH264 SW |
| Linux | _(none)_ | _(none)_ | VAAPI | OpenH264 SW |
| Linux (no GPU) | _(none)_ | _(none)_ | OpenH264 SW | OpenH264 SW |

There are two visible gaps:

1. **Linux has no H.265 path at all.** Even with capable HW (Intel/AMD VAAPI HEVC), the host falls back to H.264 + VAAPI or OpenH264 SW.
2. **Windows non-NVIDIA H.265 encode is missing.** Intel iGPU (QSV) and AMD GPU (AMF) hosts can decode HEVC via MF but cannot encode.

Filling these gaps with **three separate vendor SDK integrations** (oneVPL for QSV, AMF for AMD, libva already in place for Linux but HEVC pieces missing) would multiply the maintenance surface. FFmpeg's `libavcodec` is the only mature cross-vendor abstraction over `hevc_vaapi`, `hevc_qsv`, `hevc_amf`, and `hevc_nvenc` that the industry (Sunshine/Moonlight, OBS, GStreamer's libav plugin) already relies on for exactly this matrix.

The previous ADR (`2026-04-27-software-codec-openh264.md`) rejected FFmpeg on the grounds that:

> _"LGPL dynamic-link constraint forces shipping `avcodec.dll` separately and fights our single-MSI signing pipeline."_

That reasoning still holds for **SW codecs** (where OpenH264 statically vendored into the MSI is strictly better) and **is not contested by this ADR**. It does not, however, address the HW-codec matrix gap above, which OpenH264 cannot fill.

## Decision

Introduce a new crate `crates/media-ffmpeg` that:

1. Provides `prdt-media-core::VideoEncoder` / `VideoDecoder` implementations backed by FFmpeg's `libavcodec` HW codecs.
2. Is **opt-in via Cargo features** — never built unless `--features ffmpeg-*` is passed.
3. **Only wires HW backends.** SW HEVC encode/decode through FFmpeg is explicitly out of scope; OpenH264 remains the SW fallback per the prior ADR.
4. Links against **LGPL-only FFmpeg builds** — no GPL components (no `x264`, `x265`, etc. compiled in). This preserves the option to ship FFmpeg DLLs alongside our MSI under LGPL terms; static-linked FFmpeg builds are explicitly out of scope.
5. Reuses existing native paths as primary on platforms where they already work (Windows NVIDIA NVENC stays the default for that platform).

### Crate structure (target)

```
crates/media-ffmpeg/
  src/
    lib.rs          re-exports + runtime probe
    error.rs        FfmpegError (mapped from AVERROR_*)
    hwdevice.rs     AVHWDeviceContext setup (D3D11VA / VAAPI / QSV / CUDA)
    hwframes.rs     AVHWFramesContext setup, format pinning
    annexb.rs       re-export of media-core annex-B util (see Follow-ups)
    options.rs      low-latency option builder (bf=0, rc-lookahead=0, ...)
    encoder/
      mod.rs
      hevc_vaapi.rs    HevcVaapiFfmpegEncoder (Linux Intel/AMD)
      hevc_nvenc.rs    HevcNvencFfmpegEncoder (Linux NVIDIA + Windows scaffold)
      hevc_qsv.rs      HevcQsvFfmpegEncoder (Intel iGPU — phase ≥ P4)
      hevc_amf.rs      HevcAmfFfmpegEncoder (Windows AMD — phase ≥ P5)
    decoder/
      mod.rs
      hevc_vaapi.rs    HevcVaapiFfmpegDecoder (Linux)
      hevc_d3d11va.rs  HevcD3d11vaFfmpegDecoder (Windows — phase ≥ P3)
```

### Bindings

**`rusty_ffmpeg`** is selected over `ffmpeg-next` because the ultra-low-latency
contract requires fine-grained control over:

- `AVHWFramesContext` lifetime (zero-copy guarantee)
- `AVCodecContext` private options (`zerolatency`, `rc-lookahead`, `forced-idr`, `async_depth`)
- `AVHWDeviceContext` construction from a pre-existing `ID3D11Device` / VADisplay
- Format pinning to prevent implicit `hwdownload` / `swscale` insertion

`ffmpeg-next` is a higher-level wrapper that historically lags on HW-accel internals.
Raw `bindgen` is rejected for the same CI / `rustfmt`-on-Windows reasons as the
previous OpenH264 ADR.

### Linking

- **Linux:** dynamic link against system `libavcodec` / `libavutil` / `libavfilter`
  (Debian bookworm ships LGPL builds; matches our `scripts/dev-container.sh`).
- **Windows:** dynamic link against shipped LGPL FFmpeg DLLs (e.g.
  [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) `*-lgpl-shared`),
  bundled into the WiX MSI in a separate component group so signing remains
  one-pass.
- **No static linking.** Re-evaluate only if MSI signing tooling makes
  dynamic-link distribution untenable.

### Feature flags (fine-grained)

```toml
[features]
default = []
ffmpeg = []                              # base — pulls rusty_ffmpeg only
ffmpeg-encode-hevc-vaapi   = ["ffmpeg"]  # P1
ffmpeg-decode-hevc-vaapi   = ["ffmpeg"]  # P2
ffmpeg-decode-hevc-d3d11va = ["ffmpeg"]  # P3
ffmpeg-encode-hevc-qsv     = ["ffmpeg"]  # P4
ffmpeg-encode-hevc-amf     = ["ffmpeg"]  # P5
ffmpeg-encode-hevc-nvenc   = ["ffmpeg"]  # P0 scaffold-only, not for prod
```

Each backend additionally performs a **runtime probe** via
`avcodec_find_encoder_by_name` / `avcodec_find_decoder_by_name` before
declaring itself available; a missing codec at runtime degrades to the
next fallback rather than panicking.

### Low-latency presets (mandatory across backends)

| Knob | Value | Why |
|---|---|---|
| `bf` (B-frames) | `0` | No reorder buffer → no DTS-vs-PTS skew |
| `rc-lookahead` | `0` | Eliminates encoder-side lookahead latency |
| `zerolatency` | `1` (where supported) | NVENC / x265-style flag |
| `async_depth` | `1` (QSV / VAAPI) | One in-flight surface only |
| `forced-idr` | `1` | Explicit IDR on `RequestIdr` |
| GOP | `30` or `60` (fixed, no scenecut) | Bounded recovery latency |

### Bitstream normalization

The wire format already carries Annex-B NAL units (cf. `media-vaapi/annexb.rs`).
FFmpeg encoders may emit HVCC/AVCC depending on the muxer; we apply the
`hevc_mp4toannexb` bitstream filter (or equivalent) to **all** FFmpeg encoder
output before it hits `EncodedFrame::nal_units`. The Annex-B helper is promoted
from `media-vaapi` to `media-core` (or a new `media-bitstream` util crate) to
avoid `media-ffmpeg → media-vaapi` direction-inverted dependency.

### Capability negotiation

`ControlMessage::Hello` / `HelloAck` are extended (in a follow-up protocol bump,
**not in P1**) to carry an HEVC profile descriptor:

```rust
pub struct HevcCapability {
    pub profile: HevcProfile,   // Main, Main10, ...
    pub bitdepth: u8,           // 8 | 10
    pub chroma: ChromaSubsampling, // Yuv420, ...
}
```

P1 ships with `HevcCapability::main_8bit_yuv420()` hard-coded on both ends to
defer the protocol bump until 10-bit / HDR work begins.

## Drivers

1. **Linux HEVC parity.** No code path today produces HEVC on Linux; this is the
   single biggest functional gap relative to the Windows host. Adding
   `hevc_vaapi` via FFmpeg covers Intel + AMD + NVIDIA Linux hosts in one
   implementation.
2. **Non-NVIDIA Windows HEVC encode.** QSV (Intel) and AMF (AMD) HEVC encode
   are otherwise three separate SDK integrations. FFmpeg collapses them to
   one trait impl per backend file.
3. **Maintenance surface vs. industry alignment.** Sunshine/Moonlight already
   uses native NVENC + FFmpeg for non-NVIDIA fallbacks. Cloning that model is
   strictly less risky than diverging from it.

## Alternatives considered

| Option | Adopted? | Reason |
|---|---|---|
| **A. `crates/media-ffmpeg` (this ADR)** | **Yes** | Cross-vendor HW HEVC coverage with one wrapper. |
| B. Three native SDK integrations (oneVPL + AMF + extend VAAPI) | No | 3× ongoing maintenance, three sets of build dependencies, three sets of CI runners. We may absorb individual native SDKs *later* if FFmpeg latency or stability is unacceptable on a specific backend. |
| C. GStreamer | No | Larger dependency, opinionated pipeline model fights the existing `Producer/Consumer` traits, and most HW codec elements wrap libavcodec anyway. |
| D. Microsoft Media Foundation HEVC (extend existing path) | No | Windows-only and requires the user-installable HEVC Video Extensions; does not address the Linux gap. We keep MF as a decode option but do not extend it for encode. |
| E. FFmpeg as a *replacement* for native NVENC | No | Replacing the existing zero-copy NVENC path produces no user-visible improvement and risks regressing the verified low-latency Windows NVIDIA pipeline. The first-PR scope (P1) deliberately does **not** touch the Windows NVENC path. |
| F. FFmpeg for SW HEVC encode/decode | No | LGPL bundling cost is not justified when OpenH264 (BSD-2, statically vendored) covers SW fallback. Reaffirms prior ADR. |

## Zero-copy contract

The existing native NVENC zero-copy path on Windows **must not regress**.
The FFmpeg encoder/decoder paths therefore:

- Construct `AVHWDeviceContext` from the existing `ID3D11Device` (Windows) or
  `VADisplay` (Linux) rather than letting FFmpeg create its own — preventing
  hidden cross-device copies.
- Pin `AVCodecContext::pix_fmt` to `AV_PIX_FMT_D3D11` (Windows) /
  `AV_PIX_FMT_VAAPI` (Linux) and **fail** rather than silently fall through
  to `AV_PIX_FMT_NV12` (which would force a CPU readback).
- **Forbid** the `hwdownload` / `swscale` / `hwupload` filters anywhere in the
  capture→encode→wire→decode→render chain. CI has a grep-level guard against
  these filter names appearing in `crates/media-ffmpeg/src/`.
- Emit a structured `tracing` event on first frame:
  `INFO video.pipeline codec=h265 backend=ffmpeg-vaapi zero_copy=true`.

## Phasing

| Phase | Scope | Risk | ROI |
|---|---|---|---|
| **P0** | Scaffolding only: `crates/media-ffmpeg` crate exists, `rusty_ffmpeg` builds in dev container, `hevc_nvenc` smoke test on Linux NVIDIA (not wired into host) | Low | Validates build/CI shape |
| **P1** | **Linux HEVC encode via `hevc_vaapi`**, wired into `prdt-host` as `--encoder ffmpeg-vaapi-hevc`. Decode side stays unchanged (viewer keeps existing path) | Med | **Fills the Linux H.265 gap (top priority)** |
| **P2** | Linux HEVC decode via `vaapi`, wired into viewer on Linux | Med | Symmetric Linux HEVC |
| **P3** | Windows HEVC decode via `d3d11va`, removes MF HEVC Extension dependency | Med | Removes Microsoft Store install step from prerequisites |
| **P4** | Intel `hevc_qsv` encode (Windows + Linux iGPU coverage) | Med | iGPU host coverage |
| **P5** | AMD `hevc_amf` encode (Windows) | Med | AMD GPU host coverage |
| —   | Windows NVENC via FFmpeg | **Skipped** | No ROI; existing native NVENC is the verified primary path |

## Out of scope (explicit)

- **10-bit HEVC / Main10 / HDR / BT.2020** — defer to a follow-up ADR after P1 lands. P1 ships 8-bit Main 4:2:0 only.
- **Dynamic resolution change mid-session** — current behavior (renegotiate on size change) is unchanged.
- **VideoToolbox / macOS** — no current macOS support; do not pre-wire.
- **Audio (Opus) routing through FFmpeg** — Opus stays on its current path.
- **Static FFmpeg builds** — dynamic link only for P1–P5.
- **Replacing NVENC native on Windows** — see Alternatives row E.

## Risks

1. **Hidden CPU readback if `pix_fmt` is not pinned** — mitigated by the format-pin
   check above and the no-filter CI guard. Detection: a trace event named
   `video.pipeline.warning.cpu_roundtrip` is emitted whenever a frame leaves
   GPU memory; presence in P1 smoke logs is a release blocker.
2. **HEVC patent exposure** — same posture as the OpenH264 ADR: defer the
   MPEG-LA question until cumulative installs make it material. HEVC HW
   encoders typically pass the patent burden to the GPU vendor whose driver
   we are calling.
3. **MSI signing complexity from bundled FFmpeg DLLs** — WiX
   ComponentGroup keeps the FFmpeg DLLs in a separate component, signed in
   the same pass as our binaries. If signing tooling chokes, fall back to a
   companion install step (documented but discouraged).
4. **CI cost** — GPU runners are not in the current CI matrix. P1 acceptance
   runs on a self-hosted Linux VAAPI runner (Intel iGPU on the dev machine)
   triggered manually, not on every PR.
5. **`rustfmt` drift between Windows CI and the dev container** — `media-ffmpeg`
   is Linux-only for P0–P2, so the existing `cargo fmt --all` pre-push
   discipline (per `CLAUDE.md`) is sufficient. Re-evaluate when P3 introduces
   Windows code paths.

## Acceptance — P1 only

| Criterion | Threshold | Verification |
|---|---|---|
| Linux HEVC e2e_p99 latency | `< 35 ms` (at 1080p60, 30 Mbps, same harness as OpenH264 ADR) | `prdt-bench-matrix --encoders ffmpeg-vaapi-hevc --decoders openh264` (decode side TBD until P2) |
| Linux HEVC bandwidth advantage | `bps_p50` reduced ≥ 25 % vs. OpenH264 SW H.264 at equal visual quality | Same harness |
| Zero hidden CPU readback | Zero `video.pipeline.warning.cpu_roundtrip` events in a 5-minute smoke | grep on smoke log |
| MSI size delta (Windows) | N/A in P1 (Linux-only) | — |
| `cargo fmt --all` clean | mandatory pre-push (per `CLAUDE.md`) | CI |

## Consequences

- **+** Linux hosts gain HEVC encode at last; bandwidth at equal quality drops vs. H.264 SW.
- **+** Future HW backends (`hevc_qsv`, `hevc_amf`, `hevc_d3d11va`) become single-file additions.
- **+** Reduces drift from industry-standard streaming-server architecture (Sunshine).
- **−** New external runtime dependency on system `libavcodec` (Linux) / bundled FFmpeg DLLs (Windows P3+).
- **−** LGPL compliance documentation must be added to `docs/` and to the MSI's license file.
- **−** CCG/Codex flagged: `rusty_ffmpeg` is a thinner wrapper → more `unsafe` blocks in our crate. We accept this for HW-control precision; balanced by keeping the surface small (one trait impl per backend file).

## P1.5 — NVENC variant added

**Status:** Proposed (unchanged — hardware smoke not yet run)

A second hardware encode backend `HevcNvencFfmpegEncoder` was added in `crates/media-ffmpeg`
gated by the `ffmpeg-encode-hevc-nvenc` feature triplet (default ABI `ffmpeg6` to match the
Ubuntu 24.04 smoke runner, diverging temporarily from VAAPI's `ffmpeg5` default — see F6).

### `auto` preference policy

When both VAAPI and NVENC compile in, `--encoder auto` prefers VAAPI (Intel iGPU is the more
common deployment). The `PRDT_PREFER_NVENC` env-var override flips the preference for users
on dGPU-equipped hosts:

```
PRDT_PREFER_NVENC=1   # accepted: 1, true, yes, on (case-insensitive); anything else = unset
```

A structured `tracing::info!` line at the resolution site records which backend was chosen
and why (`selected_by`, `reason` fields).

### Annex-B asymmetry

`hevc_nvenc` emits Annex-B by default — **no** `hevc_mp4toannexb` BSF chain on the NVENC
side. The VAAPI BSF chain remains unchanged. This asymmetry is documented in the encoder
doc-comment and in the smoke doc.

### Minimum driver requirement

NVENC path requires NVIDIA driver **≥ 535** for reliable HEVC NVENC on Pascal/Turing/Ampere.

### Follow-up F4 — CPU BGRA→NV12 conversion urgency

CUDA NPP BGRA→NV12 is **materially more urgent for NVENC than VAAPI** because NVENC encode
latency is much lower (~1–2 ms on modern GPUs vs. ~3–6 ms for VAAPI on iGPU), so CPU-side
BGRA→NV12 conversion is a larger fraction of the per-frame budget. Tracked separately as F4.

---

## Decode side (P2)

P2 plugs three HEVC decode backends into the Linux viewer (`crates/viewer`) behind the
same disjoint Cargo feature shape as the encode side.

### Backend selection (R3 — deliberate inversion vs. encode)

The `--decoder auto` resolution order for H.265 on Linux is **VAAPI → NVDEC → SW**,
deliberately inverted from the encode side's NVENC-first order. Rationale: decode is
power-bound on hybrid laptops; an Intel/AMD iGPU draws ~5 W at 1080p60 decode versus
~25 W for a discrete NVIDIA GPU at the same workload. Waking the dGPU for decode also
disables panel self-refresh and adds PCIe traversal for the CPU readback — net cost is
disproportionate for a workload the iGPU handles trivially.

`PRDT_PREFER_NVDEC=1` (truthy: `{1,true,yes,on}`, case-insensitive; mirrors
`PRDT_PREFER_NVENC` spec verbatim) flips to NVDEC-first for users on desktops or
always-plugged-in machines. Reason strings in the structured log (`preferred-over-nvdec`,
`preferred-over-vaapi-by-env`) make the inversion auditable.

### SW HEVC 4K60 disclosure (R7)

> **SW HEVC backend handles 1080p60 within the latency budget on a modern CPU
> (i7-12700 / Ryzen 7700 or better). 4K60 SW decode is functional but consumes
> 70–100% of a core per stream and exceeds the per-frame latency target; users on
> 4K60 should select VAAPI (Intel/AMD iGPU) or NVDEC (NVIDIA dGPU).**

### NV12 carrier

All three decode backends output NV12 8-bit (the codec's native format after
`av_hwframe_transfer_data` readback for the HW backends; the SW backend pins
`pix_fmt = AV_PIX_FMT_NV12` directly). `PlatformFrame` on Linux gains an `Nv12`
variant alongside the existing `I420` variant; the renderer adds a parallel
`nv12_to_bgra` blit. The OpenH264 H.264 `I420` path is byte-for-byte unchanged.

### Regression-safety (A12)

The P2 destructure surgery at `viewer/src/lib.rs:2137` rewrote the irrefutable
`let PlatformConsumer::Openh264 { .. } = &mut *c;` into a full `match &mut *c` to
accommodate the three new variants. A12.b provides the regression guard: a unit test
in `crates/viewer/src/platform/linux.rs` encodes a 320×240 I420 IDR with
`Openh264Encoder` and feeds the NAL units through the rewritten
`PlatformConsumer::Openh264` match arm, asserting `latest` becomes
`Some(Arc<I420Frame>)` with correct plane dimensions. No winit/softbuffer surface is
required; the test exercises only the decoder arm.

### Follow-ups (P2)

- **F1-P2 (P2.5)** — GPU-to-GPU zero-copy on decode: extend the Linux renderer to
  consume `AV_PIX_FMT_VAAPI` and `AV_PIX_FMT_CUDA` surfaces directly, removing the
  per-frame `hw_download` readback.
- **F2-P2 (release tag)** — NVDEC + VAAPI HW decode smoke on a runner with real HW
  (A4/A11 deferrals from P2).
- **F3-P2 (release tag)** — Bench-matrix Linux port: measure HEVC-decode latency
  contribution at 1080p60 and 4K60 for all three backends.
- **F4-P2** — Windows D3D11VA HEVC decode via FFmpeg as a fallback for non-NVIDIA
  Windows boxes without MF HEVC Extensions (P3).

---

## P2.5 — NPP encode path (CUDA NPP BGRA→NV12, closes F4)

**Status:** Proposed (2026-05-16, plan `.omc/plans/2026-05-16-p2-5-cuda-npp-bgra-nv12.md`)

### Decision

Add an opt-in CUDA NPP BGRA→NV12 conversion path for the Linux NVENC encoder
behind a new `ffmpeg-encode-hevc-nvenc-npp{,-ffmpeg5,-ffmpeg7}` Cargo feature
family (4 features — each NPP variant transitively enables the matching NVENC
ABI variant via Cargo's additive feature unification). Implemented as a separate
`HevcNvencNppFfmpegEncoder` type in
`crates/media-ffmpeg/src/hevc_nvenc_npp_encoder.rs`, with a thin hand-rolled
`extern "C"` CUDA runtime API + NPP wrapper in `crates/media-ffmpeg/src/cuda_npp.rs`.
Shared bookkeeping extracted into `crates/media-ffmpeg/src/nvenc_common.rs`
(R6 synthesis — eliminates bookkeeping drift by construction).

**F4 CLOSED.** GPU-side BGRA→NV12 for NVENC is now implemented. VAAPI VPP
BGRA→NV12 remains explicitly out of scope (VAAPI encode latency is larger than
its CPU-conversion savings; see plan §6 Option B).

### Drivers

1. **CPU budget on NVENC hosts (F4).** At 1080p60, two CPU pixel passes
   (`bgra_to_i420` + `i420_to_nv12_into`) consume ~1–3 ms per frame; NVENC
   encode itself runs in ~2–5 ms. NPP moves both passes to the GPU at ~0.3 ms.
2. **`auto` policy stability.** NPP is explicit opt-in only — `--encoder auto`
   is byte-stable from P1.6. Use `--encoder ffmpeg-nvenc-hevc-npp` (or alias
   `--encoder nvenc-npp`) to opt in.
3. **A13 behavioral regression invariance.** The R6 refactor is mechanical;
   existing P1.5 NVENC unit tests prove encoded-bytestream equivalence.

### Runtime semantics (R5)

| Build features | Host hardware | `--encoder ffmpeg-nvenc-hevc-npp` | `--encoder auto` |
|---|---|---|---|
| `nvenc + npp` | NVIDIA dGPU + libnppicc loaded | NPP path; log emits `convert_path="npp"` | Picks plain NVENC (NPP never auto-selected) |
| `nvenc + npp` | Non-NVIDIA or libnppicc absent | `HevcNvencNppFfmpegEncoder::new` fails with `FfmpegError::HwDevice` — explicit failure, not silent fallback | Falls through `auto` chain normally |
| `nvenc` only (no npp) | Any | `normalize_encoder` warns; falls back to openh264 | Plain NVENC selected normally |

### Prerequisites

- **NVIDIA driver ≥ 535** — matches P1.5 A8; provides CUDA 12.x forward-compat.
- **CUDA runtime ≥ 12.0** — `CudaNppContext::new` probes `cudaDriverGetVersion`
  and rejects drivers reporting < 12000 with a clear error.
- **`libcudart.so.12` + `libnppicc.so.12`** at runtime — not linked at build
  time; binary passes the ldd portability guard.

### A10 carve-out (O1)

`hevc_nvenc_npp_encoder.rs` contains **zero** `hw_(upload|download)()` call
sites. The NPP path uploads BGRA via `cudaMemcpy2D` (HtoD) and writes NV12
directly into the AVFrame's CUDA planes — the deliberate by-design replacement
for `av_hwframe_transfer_data`. New NPP-style sites carry
`// ci-allow: cuda-direct`. CI grep guard explicitly asserts zero matches in
the NPP encoder file.

### A11 — Feature-flag ABI unification

`rusty_ffmpeg`'s ABI features form an additive cascade (`ffmpeg6 = ["ffmpeg5"]`,
`ffmpeg7 = ["ffmpeg6"]`). Cargo unifies features additively; enabling
`ffmpeg-encode-hevc-nvenc-npp-ffmpeg5` + `ffmpeg-encode-hevc-nvenc-ffmpeg7`
simultaneously resolves `rusty_ffmpeg` to `ffmpeg7` (highest cascade). Verified
empirically by CI cell A11.b (`cargo metadata` Python assertion).

### R6 architectural note

`nvenc_common.rs` was extracted as a "second variant pushed the extract"
refactor — aligned with P1.5's principle. Both encoder files are thin shells
over shared helpers; drift is impossible by construction.

### F10 tripwire

`cuda_npp.rs` binds **8 symbols** (CUDA runtime API `cuda*` — implementation
deviation from plan's driver API `cu*`; see module doc). If symbol count grows
past **12**, re-evaluate `cust`/`cudarc` crate dep (F10). File header carries
`// CURRENT SYMBOL COUNT: 8` as the human-readable tripwire.

### Follow-ups (P2.5)

- **F5 (next ralplan, HDR)** — P010/10-bit Main10 NPP variant.
- **F6 (P2.6, optional)** — NPP async + pinned host memory + per-encoder CUDA stream overlap.
- **F7 (P3)** — Decoder-side GPU zero-copy via wgpu viewer migration; feasibility doc at
  `.omc/plans/2026-05-16-p3-decoder-zerocopy-feasibility.md`.
- **F8** — `PRDT_PREFER_NPP=1` env override for `--encoder auto` NPP selection; deferred.
- **F10** — Symbol count tripwire; see above.

---

## Follow-ups

- **F1** — When a third backend lands (P4 `hevc_qsv`), revisit `HwDevice<Kind>` generic and/or
  `HwBackend` trait extraction; design against three concrete backend constraints.
- **F2** — NVIDIA hardware smoke (A4) on a developer's NVIDIA Linux box; log in the smoke doc.
- **F3** — Consider exposing `cuda_device_index` as a CLI flag once multi-GPU Linux hosts become
  a real user complaint.
- **F4** — ~~GPU-side BGRA→NV12 (CUDA NPP for NVENC; VAAPI `vpp` for VAAPI).~~ **CLOSED in P2.5**
  (NVENC NPP path shipped; VAAPI VPP explicitly out of scope per plan §6 Option B).
- **F5** — Bench-matrix Linux port (separate side project) gates any perf-regression CI for
  either backend.
- **F6** — Reconcile default-ABI divergence: flip VAAPI default from `ffmpeg5` to `ffmpeg6` in
  a separate cleanup PR. Separate concern from NVENC-add; deserves its own changelog entry.
- **F7** — `zero_copy=true` log-line naming is misleading (path is CPU-NV12-upload); fix in the
  broader log-line audit when F4 adds a real zero-copy path.
- **F8** — P1.6 PR for legacy `--encoder nvenc` alias rerouting to `ffmpeg-nvenc-hevc`.
- **Promote `media-vaapi/annexb.rs` → `media-core`** (or a `media-bitstream` util) so `media-ffmpeg` does not depend on `media-vaapi`.
- **`HevcCapability` protocol bump** — required when 10-bit / HDR work begins (post-P1).
- **GPU-accel CI lane** — self-hosted runner with Intel iGPU for VAAPI; NVIDIA runner for `hevc_nvenc`-via-FFmpeg if P0 scaffold proves valuable.
- **macOS VideoToolbox** — deferred until macOS host/viewer scope is committed.
- **Quiescent re-baseline of NVENC native** — referenced by the OpenH264 ADR as still pending; landing P1 is a good opportunity to schedule it.
