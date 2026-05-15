# FFmpeg HEVC VAAPI — Manual Smoke Test Procedure

> **PR description must include:** *"A4/A5/A7 ran on hardware? Y / N. If N, P1 release tag is blocked until run on `<host SKU>`."*

This document describes how to manually verify the FFmpeg HEVC VAAPI encoder path
(`ffmpeg-encode-hevc-vaapi` feature) on real hardware, end-to-end: Linux iGPU host →
Windows viewer over LAN.

---

## Prerequisites

### Host machine (Linux)

Supported iGPU SKUs (one of):

| SKU | Expected `lspci -nn | grep VGA` output |
|-----|----------------------------------------|
| Intel Core i5-12500 / i5-13500 (UHD 770) | `Intel Corporation Alder Lake-S GT1 [UHD Graphics 770] [8086:4680]` |
| AMD Ryzen 7000 APU (Radeon 680M / 760M) | `Advanced Micro Devices, Inc. [AMD/ATI] Phoenix3 [Radeon Graphics] [1002:15bf]` |

Run `lspci -nn | grep VGA` and confirm the output matches one of the above.

**Mesa version** (Intel) or **AMDGPU driver** must be Mesa ≥ 22.x:
```bash
vainfo 2>&1 | head -3
# Expected: libva info: VA-API version ... driver ... mesa ...
```

**`vainfo` must show HEVC Main encode entry point.** Example for Intel UHD 770:
```
vainfo: VA-API version: 1.19 (libva 2.19.0)
vainfo: Driver version: Intel iHD driver for Intel(R) Gen Graphics - 23.x.x
vainfo: Supported profile and entry points
      VAProfileHEVCMain               :	VAEntrypointEncSlice
      VAProfileHEVCMain               :	VAEntrypointEncSliceLP
```

Confirm both `VAEntrypointEncSlice` (or `VAEntrypointEncSliceLP`) and `VAProfileHEVCMain`
are present. If `vainfo` is missing, install `libva-utils`.

### Dev container

Dev container must be provisioned per `Dockerfile.dev` (all FFmpeg dev libs present):
```bash
./scripts/dev-container.sh bash -c 'pkg-config --modversion libavcodec'
# Expected: 59.x.x  (bookworm libavcodec5 = ffmpeg 5.1.x)
```

### Viewer machine (Windows 11)

One of:

- Windows 11 with NVIDIA GPU (NVDEC HEVC-capable, Maxwell or later), **OR**
- Windows 11 with **Microsoft HEVC Video Extensions** installed (from Microsoft Store)

Note the viewer machine name here at run time: **Viewer machine:** `<fill in>`

---

## Build commands

From the project root on the Linux host:

```bash
cd /home/ubuntu/project/power-remote-dt
./scripts/dev-container.sh bash -c 'cargo build -p prdt-host --features ffmpeg-encode-hevc-vaapi --release --target x86_64-unknown-linux-gnu'
```

Binary lands at:
```
./target-docker/x86_64-unknown-linux-gnu/release/prdt-host
```

---

## Start host

Run on the Linux host machine:

```bash
./target-docker/x86_64-unknown-linux-gnu/release/prdt-host \
    --encoder ffmpeg-vaapi-hevc \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless
```

Redirect logs to file for post-run assertions:
```bash
./target-docker/x86_64-unknown-linux-gnu/release/prdt-host \
    --encoder ffmpeg-vaapi-hevc \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless \
    2>&1 | tee host.log
```

Expected first 10 log lines must include **both**:
```
INFO  video.pipeline event="encoder_ready" backend="ffmpeg-vaapi-hevc" codec="h265" profile="main" bitdepth=8 gop=60
Host public key: <base64>   ← copy this for the viewer --host-pubkey argument
```

---

## Start viewer (Windows)

On the Windows viewer machine, open PowerShell:

```powershell
.\target\release\prdt-viewer.exe --host <linux-host-ip>:9000 --host-pubkey <pubkey> --codec h265 --decoder auto
```

Replace `<linux-host-ip>` with the LAN IP of the Linux host (e.g. from `ip addr`),
and `<pubkey>` with the base64 string printed by the host above.

Redirect viewer logs:
```powershell
.\target\release\prdt-viewer.exe --host <linux-host-ip>:9000 --host-pubkey <pubkey> --codec h265 --decoder auto 2>&1 | Tee-Object viewer.log
```

Expected line in viewer log:
```
negotiated_codec=H265
```

---

## Verification

Run the following assertions from the Linux host after the session has been running for
at least 5 minutes. All commands operate on `host.log` and `viewer.log` (or copy
`viewer.log` from the Windows machine first).

### Encoder ready (must be exactly 1)
```bash
grep -c 'video.pipeline event="encoder_ready"' host.log
# Expected: 1
```

### First frame emitted (must be exactly 1)
```bash
grep -c 'video.pipeline event="first_frame_emitted"' host.log
# Expected: 1
```

### No CPU readback warnings (must be exactly 0)
```bash
grep -c 'video.pipeline.warning.cpu_readback' host.log
# Expected: 0
```

### 5-min frame count (must be ≥ 17000)
```bash
grep -c 'frame_decoded seq=' viewer.log
# Expected: >= 17000  (≈ 57 fps × 300 s)
```

### Sequence-gap check (no dropped frames)
```bash
grep -oE 'frame_decoded seq=[0-9]+' viewer.log | awk -F= '{print $2}' | sort -n | \
  awk 'NR==1{min=$1} {max=$1; count++} END{if (max-min == count-1) print "OK"; else print "GAPS"}'
# Expected: OK
```

---

## 5-min soak

While the session runs:

1. Monitor host CPU usage — it should stay low (VA-API encode is GPU-side):
   ```bash
   top -p $(pgrep prdt-host)
   ```
   Expect: CPU% well below 50% for a single-core; spikes OK, sustained high usage is a
   regression signal.

2. Between runs (before re-starting), check driver state:
   ```bash
   vainfo -a
   ```
   No new errors should appear between runs.

---

## Run log

**Date:** `<YYYY-MM-DD>`
**Host SKU:** `<i5-... / Ryzen ...>`
**Viewer:** `<machine>`
**Result:** PASS / FAIL
**encoder_ready count:** `<N>`
**first_frame_emitted count:** `<N>`
**frame_decoded total at 5min:** `<N>`
**Sequence gap check:** OK / GAPS
**CPU readback warnings:** `<N>`
**Notes:**
