# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [x] Plan 2b: `media-win` DXGI capture + NVENC H.265 encoder
- [x] Plan 2c: `media-win` Media Foundation decode + VideoProducer/VideoConsumer traits
- [x] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria
- [ ] Plan 2d (optional): cuvid/NVDEC direct for lower-latency decode

## Building

Requires Rust stable (>= 1.78), Windows 11 + NVIDIA GPU, NVIDIA Video
Codec SDK, LLVM (for bindgen), and Microsoft HEVC Video Extensions
(Microsoft Store).

```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build --release
```

## Running (two machines, LAN)

On the host machine (the one whose desktop is shared):
```powershell
.\target\release\prdt-host.exe --bind 0.0.0.0:9000 --monitor 0 --bitrate-mbps 30
```

On the viewer machine:
```powershell
.\target\release\prdt-viewer.exe --host <host-ip>:9000
```

See `docs/superpowers/plan3-manual-smoke.md` for full smoke test procedure.

## Architecture (Phase 0)

```
[host machine]
  DXGI Desktop Duplication
    → D3D11 BGRA texture
    → NVENC H.265 encode (zero-copy via shared texture)
    → CustomUdpTransport send
                                  ↓ UDP (custom protocol)
[viewer machine]
  CustomUdpTransport recv
    → MF H.265 decode (zero-copy via IMFDXGIBuffer)
    → ID3D11VideoProcessor NV12→BGRA
    → DXGI swapchain present (winit window)

[inputs, reverse direction]
  winit WindowEvent → input_tx mpsc → UDP → host SendInput
```

## Testing

```powershell
cargo test -p prdt-protocol -p prdt-transport
# With GPU + SDK:
cargo test -p prdt-media-win -p prdt-input-win
```

All 84+ tests pass.
