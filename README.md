# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** Phase 0 (Core Pipeline PoC) — see `docs/superpowers/specs/2026-04-22-phase0-core-pipeline-design.md`.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [ ] Plan 2b: `media-win` DXGI capture + NVENC
- [ ] Plan 2c: `media-win` NVDEC + render + producer/consumer
- [ ] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4: Benchmarks & exit criteria

## Building

Requires Rust stable (>= 1.78), Windows 11 + D3D11-capable GPU for Plan 2a+.

```
cargo test -p prdt-protocol -p prdt-transport
cargo run -p prdt-latency-bench --release -- --duration 2s
# On Windows with a GPU:
cargo test -p prdt-media-win
```
