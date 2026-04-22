# Phase 0 Plan 3 — Manual Smoke Test Procedure

This document describes how to manually verify that the `prdt-host` and
`prdt-viewer` binaries together form a working remote desktop.

## Prerequisites

- Windows 11, NVIDIA GPU with HEVC encode (NVENC 6th gen+)
- NVIDIA Video Codec SDK installed (`NV_CODEC_SDK_PATH` env var set)
- LLVM / libclang available (`LIBCLANG_PATH` or on PATH)
- Microsoft HEVC Video Extensions installed (from Microsoft Store)
- Rust stable (>= 1.78)

## Setup

Build both binaries in release:
```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build --release -p prdt-host -p prdt-viewer
```

Binaries land at:
- `target/release/prdt-host.exe`
- `target/release/prdt-viewer.exe`

## Scenario 1: Single-machine loopback

Run both binaries on the same machine. Host captures the desktop and sends
to the viewer window on the same machine. Useful as a sanity check.

**Terminal 1 (host):**
```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
.\target\release\prdt-host.exe --bind 127.0.0.1:9000 --monitor 0 --bitrate-mbps 20
```

Expected log output:
```
INFO host starting monitor=0 device_name="\\\\.\\DISPLAY1" bitrate_mbps=20
INFO listening local=Ok(127.0.0.1:9000)
```

**Terminal 2 (viewer):**
```powershell
.\target\release\prdt-viewer.exe --host 127.0.0.1:9000
```

Expected:
1. A viewer window appears (1920x1080 default).
2. Host logs: `INFO handshake complete` with HelloRequest details.
3. Viewer window shows the host's desktop — but **warning**: this is
   recursive because the viewer window is ON the host's desktop; you will
   see the desktop-within-the-desktop effect. Moving the viewer window
   should show your desktop moving. Fine for visual verification but
   not for latency measurement.
4. Clicking/moving in the viewer window injects input events — the cursor
   on the host's desktop moves.

**Success criteria (Scenario 1):**
- [ ] Handshake completes
- [ ] Viewer window displays host's desktop
- [ ] Mouse in viewer moves host's cursor
- [ ] Keyboard input reaches host
- [ ] Closing viewer cleanly terminates host (no panic)

## Scenario 2: Two-machine LAN

One machine runs `prdt-host`, another runs `prdt-viewer`, connected via
wired LAN.

**Host machine:**
```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
.\target\release\prdt-host.exe --bind 0.0.0.0:9000 --monitor 0 --bitrate-mbps 30
```

Verify the host's LAN IP: `ipconfig` → look for the adapter you use.

**Viewer machine:**
```powershell
.\target\release\prdt-viewer.exe --host 192.168.x.y:9000
```

Replace `192.168.x.y` with the host machine's LAN IP.

**Success criteria (Scenario 2):**
- [ ] Handshake completes across machines
- [ ] Viewer displays host's desktop fluidly (no obvious freezes)
- [ ] Mouse and keyboard round-trip is responsive (latency < ~100ms feels OK for Phase 0; target <30ms is Plan 4 exit criterion)

## Known limitations (Phase 0)

1. **HDR displays**: Render may be wrong/washed-out on HDR output. Spec §1.2
   defers HDR to Phase 1.
2. **UAC prompts / Task Manager**: cannot be captured by DXGI Desktop
   Duplication — appear as black frames on viewer. Normal OS behavior.
3. **Full-screen exclusive games**: may cause DXGI AccessLost. Plan 4 adds
   F6 (duplication re-acquire) recovery.
4. **Multi-monitor host**: mouse coord mapping assumes single primary
   monitor. Use `--monitor 0` (or 1, 2) to pick one output; cross-monitor
   mouse movement will not work correctly.
5. **No authentication**: session ID is hardcoded; anyone on the network
   can connect to the host's UDP port. Plan 3 is PoC-only; auth/encryption
   lands in Phase 3 (per spec §5.10).
6. **Keyboard layout**: scancode passthrough assumes viewer and host use
   the same layout. Layout mismatch will produce wrong characters.
7. **Static IDR cadence**: gop_length is hardcoded to 60 frames. Manual
   IDR request via `ControlMessage::RequestIdr` works, but the viewer
   doesn't currently send one automatically on initial connect — relies
   on the host's first frame being IDR (which NvencEncoder does by
   default via the initial `idr_pending=true` flag).

## Debugging

If something misbehaves:

- `$env:RUST_LOG = "debug"` on either binary for detailed tracing.
- Host log `INFO listening local=...` must appear before you start the viewer.
- If viewer prints `handshake timeout`, check that the host IP is reachable
  (try `ping`) and UDP port 9000 is not firewalled.
- If viewer window is black but host is connected, the MF decoder may not
  be emitting output — check for `MF_E_TRANSFORM_STREAM_CHANGE` warnings;
  Plan 2c added re-negotiation but large input changes may still break.
