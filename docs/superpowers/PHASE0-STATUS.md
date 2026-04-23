# Phase 0 Status — Functionally Complete

**Date:** 2026-04-22
**Tag:** `phase0-complete`

## What works

End-to-end live remote desktop between two Windows machines (or
same-machine loopback):

- DXGI Desktop Duplication capture at the host's monitor resolution
- NVENC H.265 (HEVC) hardware encoding via NVIDIA Video Codec SDK 13.x
- Custom UDP transport with Reed-Solomon FEC (k=64, m=6 defaults)
- Media Foundation hardware H.265 decoding with `IMFDXGIBuffer` zero-copy
- `ID3D11VideoProcessor` NV12→BGRA conversion
- `winit` window with DXGI flip-model swapchain
- Mouse (absolute and buttons) + keyboard (scancode passthrough) round-trip
- Hello / HelloAck / Ping / Pong / RequestIdr / Bye control messages
- 86 automated tests (unit + integration across 4 crates)

**Smoke-tested on:** Windows 11 + RTX 3070 Ti + 4K (3840×2160) monitor,
single-machine loopback at 20 Mbps H.265.

## What's deferred from spec §7 (Phase 0 Exit Criteria)

Formally declared NOT done but the user accepted this as a functional
completion. These are tracked as known work if/when Phase 0 needs strict
sign-off:

- ~~**M1 instrumentation**~~ — **Partially done in plan4-m1.** The viewer
  now records per-frame arrival / decode-done / present timestamps and
  logs a 1 Hz `info` line with p50/p95/p99 for each stage. Producer and
  transport emit timestamps on a shared process-wide monotonic clock
  (`prdt_protocol::now_monotonic_us`), so in-process loopback (M2) and
  cross-process same-machine runs produce directly-comparable numbers.
  Cross-machine clock-offset correction via Ping/Pong is still deferred.
- ~~**M2 in-process end-to-end bench**~~ — **Done in plan4-m2 +
  plan4-m2-full-pipeline.** `prdt-latency-bench --mode in-process`
  measures transport-only (p95 ≈ 40 µs at 1080p60). `--mode
  full-pipeline-win --consumer {mf,nvdec}` measures the full
  synthetic-BGRA → NVENC → transport → decode loop with per-stage
  breakdown. Spot checks on this dev machine (RTX 3070 Ti):

  | 1080p60 | encode p50 | decode p50 | e2e p50 | e2e p95 |
  |---------|------------|------------|---------|---------|
  | MF      | 14.5 ms    | **0.20 ms**| 14.7 ms | 17.5 ms |
  | NVDEC   | 14.6 ms    | 1.80 ms    | 16.4 ms | 18.2 ms |

  MF is currently faster because it has an internal zero-copy
  IMFDXGIBuffer path; our NVDEC (Plan 2d step 2c) does a CPU bounce
  (cuMemcpy2D DtoH + UpdateSubresource) that adds ~1.6 ms. Next
  optimization: dual R8 + R8G8 CUDA-D3D11 interop textures for true
  zero-copy (tracked in project memory).

  Meets spec §7.5 B5 target "M1 p95 < 22ms" at 1080p either way.
- **M3a camera glass-to-glass measurement**: not attempted. Requires a
  240 fps smartphone camera and manual counting.
- **B1–B8 benchmark scenarios** (spec §7.5): not run. Would require M2/M3
  harness complete first.
- **F6 DXGI AccessLost recovery** (UAC / mode change): host binary logs
  and continues but does not re-acquire the duplication; current behavior
  stalls video until the process is restarted.
- **1-hour soak test** (spec §7.6 B8): not run.
- **Manual smoke checklist** (spec §7.7, 6 items): not run end-to-end;
  only the basic "picture + cursor" check was done.

## Known limitations (carry to Phase 1+ as appropriate)

0. ~~**File transfer unidirectional**~~ — **Fixed in
   phase3c-bidirectional-filetransfer.** `prdt-filetransfer` crate
   factors out send/receive. Host polls `--outgoing-dir` (default
   `prdt-outgoing/`) every 2s and streams any files found, moving them
   to `sent/` on success. Viewer accepts incoming files via the same
   state machine into `--recv-dir` (default `prdt-received/`).
   viewer→host drag-drop still works.

1. ~~**Single-monitor capture only**~~ — **Fixed in phase3c-multimonitor.**
   HelloAck now carries `host_monitor_rect` + `host_virtual_desktop_rect`
   (virtual-desktop coords); the viewer maps window-local cursor positions
   into the host's virtual desktop, and the injector uses
   `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`. The host still
   captures a single monitor at a time — simultaneous multi-monitor
   streaming remains deferred.

2. **Static FEC k** — `fec_k=64` is a static default. Frames exceeding
   64 × 1200 = 76.8 KB produce `FrameTooLarge` and are dropped. Spec §5.3
   requires dynamic FEC sizing based on frame size; this is Plan 4 work.

3. **No authentication** — session ID is hardcoded `0xDEADBEEF`. Any host
   on the LAN can be reached by anyone with the port. Auth/encryption
   land in Phase 3 per spec §5.10.

4. **HEVC Video Extensions dependency** — viewer requires Microsoft's
   "HEVC Video Extensions" store app for MF decode. Friendly startup
   error message is missing.

5. **HDR / multi-monitor / game-fullscreen** — not supported. Covered
   by spec §1.2 deferrals list.

6. **Keyboard layout assumption** — scancode passthrough works when
   viewer and host use the same physical keyboard layout.

7. **No MediaError variant for DXGI_ERROR_DEVICE_REMOVED** — folds into
   generic `MediaError::D3D11` / `MediaError::Dxgi`. Proper device
   re-creation on TDR is F7 recovery (deferred).

## What's next

The project is in a useful state for:
- **Phase 1**: Linux capture (`PipeWire`/`KMS`) + Wayland, expand
  `VideoProducer` / `VideoConsumer` backends. The trait layer in
  `prdt-protocol::video_pipeline` is already in place.
- **Phase 2**: NAT traversal, ID-based signaling, WAN readiness.
- **Phase 3**: E2E encryption (Noise/QUIC), audio, clipboard, file
  transfer, multi-monitor.
- **Phase 4**: Official benchmarking sweep → strict exit criteria. Good
  time to build the measurement harness and run B1–B8.
- **Plan 2d**: cuvid/NVDEC direct as a performance optimization (drop MF
  decode latency by 5–10 ms) — revisit when Phase 4 shows MF is the
  bottleneck.

## Tags in git

- `phase0-plan1-complete`
- `phase0-plan2a-complete`
- `phase0-plan2b-complete`
- `phase0-plan2c-complete`
- `phase0-plan3-complete`
- `phase0-complete` ← this document
