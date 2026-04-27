# prdt-media-sw

Software H.264 encode/decode for `power-remote-dt` via [OpenH264](https://github.com/cisco/openh264).

The crate is the SW fallback for hosts and viewers without GPU acceleration. It is pure-Rust at the public API (no `windows`-crate dependency), so it builds on Linux today even though Linux capture is a follow-up phase.

See ADR: [`docs/adr/2026-04-27-software-codec-openh264.md`](../../docs/adr/2026-04-27-software-codec-openh264.md).

## Public API

- `Openh264Encoder` / `Openh264EncoderConfig` — H.264 baseline encoder for `I420Frame` input.
- `Openh264Decoder` — H.264 decoder, `I420Frame` output (owned buffers, safe past the next decode call).
- `I420Frame` — planar YUV 4:2:0.
- `bgra_to_i420(bgra, w, h, stride)` — BGRA8 → packed I420, BT.601 limited-range.
- `i420_to_nv12(i420)` — I420 → NV12 byte buffer (Y plane + interleaved UV).
- `MediaSwError` / `Result<T>`.
- Traits: `SwH264Encoder`, `SwH264Decoder`.

## Build modes

| Mode | Cargo | What it does | When to use |
|---|---|---|---|
| **`source` (default)** | `openh264 = { version = "0.9.3", default-features = false, features = ["source"] }` | Compiles OpenH264 from vendored C++ via `cc`. **No build-time network I/O.** License posture: BSD-2 source. | Default. CI-friendly. License-clean for redistribution. |
| `libloading` | `openh264 = { version = "0.9.3", default-features = false, features = ["libloading"] }` | At runtime downloads Cisco's official prebuilt H.264 binary from `ciscobinary.openh264.org`. Cisco pays MPEG-LA royalties on their binary. | Only when you need Cisco's royalty pass-through and the deployment environment has unimpeded outbound HTTPS. **Corp firewalls commonly block the Cisco CDN — do not rely on this in CI.** |

## Build prerequisites

### Required

- **C++ toolchain** — `openh264 features = ["source"]` invokes `cc` to compile vendored C++ at build time.
  - Windows: MSVC `cl.exe` from Visual Studio Build Tools or the Desktop development workload. Already required by the `windows` crate FFI used elsewhere in the project, so this is not an extra install on the existing dev/CI matrix.
  - Linux: `g++` or `clang++`.
- **Rust ≥ 1.85** — workspace MSRV. Bumped from 1.78 by `software-codec-openh264-complete`.

### Recommended

- **NASM ≥ 2.x (Windows, optional)** — when NASM is on `PATH` at build time, `openh264-sys2`'s build script compiles OpenH264's hand-written x86 assembly kernels and the encoder runs about 3× faster at 1080p60. Without NASM, the build falls back to C-only kernels and still succeeds; latency is acceptable at 1080p60 30 Mbps but you will spend more CPU.
  - Verify pickup: `cargo build -vv -p prdt-media-sw 2>&1 | grep -i nasm`.
  - Install: https://www.nasm.us/ (pick the Win64 `.exe` installer; `nasm.exe` should land in `C:\Program Files\NASM\` and be added to `PATH`).
  - On Linux: `apt install nasm` / `dnf install nasm`.

## Building

```bash
# Standalone
cargo build -p prdt-media-sw

# Validate no network I/O at build time
cargo build -p prdt-media-sw --offline

# Run unit tests
cargo test -p prdt-media-sw
```

## Encoder configuration

`Openh264Encoder::new(Openh264EncoderConfig)` wraps `openh264::encoder::Encoder` with the following defaults tuned for low-latency screen sharing:

| Knob | Value | Note |
|---|---|---|
| Profile | `Baseline` | Most decoder-compatible, lowest CPU. |
| Rate control | `Bitrate` | OpenH264's CBR-equivalent exposed by the public API. |
| Complexity | `Low` | Fastest encode kernel. |
| Usage type | `ScreenContentRealTime` | Tunes for sharp text and slow-moving regions. |
| Threads | `0` (auto) | OpenH264 picks based on the build's thread support. |
| Intra period | `0` | No periodic IDR; viewer / negotiation drives IDR via `force_idr`. |

The first encoded frame is always seeded as an IDR via `force_intra_frame()` regardless of the caller's `force_idr` flag, so the decoder always has SPS+PPS+IDR to start.

## License

Apache-2.0 OR MIT, mirroring the workspace.

`openh264` and `openh264-sys2` are BSD-2-Clause. The vendored OpenH264 C++ source compiled by `features = ["source"]` ships under BSD-2-Clause; that is the license that ends up inside the produced binary, with no runtime download path.

If your deployment requires Cisco's MPEG-LA royalty pass-through, switch to `features = ["libloading"]` — the binary is then dynamically linked against Cisco's signed download. This project does not build that mode in CI.
