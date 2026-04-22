# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [x] Plan 2b: `media-win` DXGI capture + NVENC H.265 encoder
- [ ] Plan 2c: `media-win` NVDEC + render + producer/consumer
- [ ] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria

## Building

Requires Rust stable (>= 1.78), Windows 11 + NVIDIA GPU for Plan 2b+.

### Plan 1 (no GPU required)
```
cargo test -p prdt-protocol -p prdt-transport
cargo run -p prdt-latency-bench --release -- --duration 2s
```

### Plan 2a (D3D11 GPU)
```
cargo test -p prdt-media-win
```

### Plan 2b (NVENC)
Requires NVIDIA Video Codec SDK + LLVM:

1. Install NVIDIA Video Codec SDK from https://developer.nvidia.com/video-codec-sdk
   Set `NV_CODEC_SDK_PATH` environment variable to the extracted SDK root.
2. Install LLVM for Windows (https://github.com/llvm/llvm-project/releases),
   ensuring `libclang.dll` is on PATH or `LIBCLANG_PATH` is set.

Build:
```
NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37" \
  LIBCLANG_PATH="C:/Program Files/LLVM/bin" \
  cargo test -p prdt-media-win
```

If `NV_CODEC_SDK_PATH` is unset, NVENC modules build with empty stub bindings
(the rest of `media-win` still works).
