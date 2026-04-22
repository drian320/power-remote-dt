# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [x] Plan 2b: `media-win` DXGI capture + NVENC H.265 encoder
- [x] Plan 2c: `media-win` Media Foundation decode + VideoProducer/VideoConsumer traits
- [ ] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria
- [ ] Plan 2d (optional): cuvid/NVDEC direct for lower-latency decode

## Building

Requires Rust stable (>= 1.78), Windows 11 + NVIDIA GPU.

### Plan 1 (no GPU required)
```
cargo test -p prdt-protocol -p prdt-transport
cargo run -p prdt-latency-bench --release -- --duration 2s
```

### Plan 2a/2b/2c (full pipeline)
Requires:
- NVIDIA Video Codec SDK 12.x+ (set `NV_CODEC_SDK_PATH` env var)
- LLVM for Windows (for bindgen — set `LIBCLANG_PATH` or add to PATH)
- HEVC Video Extensions (Microsoft Store, for MF decode)

Build and test:
```
NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37" \
  LIBCLANG_PATH="C:/Program Files/LLVM/bin" \
  cargo test -p prdt-media-win
```

If `NV_CODEC_SDK_PATH` is unset, NVENC modules build with empty stub bindings
(the rest of `media-win` still works).

## Architecture (Phase 0 current state)

```
[host] DXGI Desktop Duplication → NVENC H.265 encode → UDP (transport)
                                                           ↓
[viewer] MF H.265 decode ← NV12 → (Plan 3: D3D11 swapchain present) ← UDP
```

`VideoProducer` / `VideoConsumer` primary traits live in `prdt-protocol`;
concrete Windows impls are `DxgiNvencProducer` and `MfD3d11Consumer` in
`prdt-media-win::pipeline`.
