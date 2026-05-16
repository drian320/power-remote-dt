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

## Follow-ups

- **Promote `media-vaapi/annexb.rs` → `media-core`** (or a `media-bitstream` util) so `media-ffmpeg` does not depend on `media-vaapi`.
- **`HevcCapability` protocol bump** — required when 10-bit / HDR work begins (post-P1).
- **GPU-accel CI lane** — self-hosted runner with Intel iGPU for VAAPI; NVIDIA runner for `hevc_nvenc`-via-FFmpeg if P0 scaffold proves valuable.
- **macOS VideoToolbox** — deferred until macOS host/viewer scope is committed.
- **Quiescent re-baseline of NVENC native** — referenced by the OpenH264 ADR as still pending; landing P1 is a good opportunity to schedule it.
