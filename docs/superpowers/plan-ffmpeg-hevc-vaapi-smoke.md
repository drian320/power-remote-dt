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

## Get the binary

Choose ONE of:

### Option 1 — Download the smoke-build artifact from GitHub Actions (recommended)

1. Open <https://github.com/drian320/power-remote-dt/actions/workflows/smoke-build-ffmpeg-hevc.yml>.
2. Click **Run workflow** → select the `feat/ffmpeg-hevc-vaapi-p1` branch (or the release tag) → **Run workflow**.
3. When the run finishes (≈ 5 min), open it and scroll to **Artifacts**. Download `prdt-linux-x86_64-ffmpeg-hevc.zip`. It contains:
   - `prdt-linux-x86_64-ffmpeg-hevc` — the `prdt` binary with `ffmpeg-encode-hevc-vaapi` compiled in
   - `prdt-linux-x86_64-ffmpeg-hevc.sha256`
4. On the iGPU host:
   ```bash
   unzip prdt-linux-x86_64-ffmpeg-hevc.zip
   sha256sum -c prdt-linux-x86_64-ffmpeg-hevc.sha256
   chmod +x prdt-linux-x86_64-ffmpeg-hevc
   mv prdt-linux-x86_64-ffmpeg-hevc /usr/local/bin/prdt   # or any PATH dir
   prdt --version
   ```

Runtime deps (apt): `libavcodec59 libavutil57 libavfilter8 libavformat59 libva2 libva-drm2 libva-x11-2` (or whatever your distro names them). On a Mesa-shipping desktop these are usually pre-installed.

### Option 2 — Build locally via the dev container

```bash
cd /home/ubuntu/project/power-remote-dt
./scripts/dev-container.sh bash -c '
  export FFMPEG_DLL_PATH=/usr/lib/x86_64-linux-gnu/libavcodec.so
  export FFMPEG_INCLUDE_DIR=/usr/include/x86_64-linux-gnu
  cargo build -p prdt-client --features ffmpeg-encode-hevc-vaapi --release --target x86_64-unknown-linux-gnu
'
```

Binary lands at:
```
./target-docker/x86_64-unknown-linux-gnu/release/prdt
```

> Note: the dev container produces a binary linked against bookworm's libavcodec; if the iGPU host runs a different distro (e.g. Ubuntu 22.04 with libavcodec58), prefer Option 1 instead.

---

## Start host

Run on the Linux host machine:

```bash
prdt host \
    --encoder ffmpeg-vaapi-hevc \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless
```

(If you built locally via Option 2, substitute `./target-docker/x86_64-unknown-linux-gnu/release/prdt host` for `prdt host`.)

Redirect logs to file for post-run assertions:
```bash
prdt host \
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
.\prdt.exe connect --host <linux-host-ip>:9000 --host-pubkey <pubkey> --codec h265 --decoder auto
```

(`prdt.exe` is the unified Windows binary from the `release.yml` workflow — `prdt-windows-x86_64.exe` on the GitHub Release page. No FFmpeg feature needed on the viewer side; the existing MF / NVDEC HEVC paths are used.)

Replace `<linux-host-ip>` with the LAN IP of the Linux host (e.g. from `ip addr`),
and `<pubkey>` with the base64 string printed by the host above.

Redirect viewer logs:
```powershell
.\prdt.exe connect --host <linux-host-ip>:9000 --host-pubkey <pubkey> --codec h265 --decoder auto 2>&1 | Tee-Object viewer.log
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
   top -p $(pgrep -f 'prdt host')
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

---

## NVENC variant (P1.5 — NVIDIA GPU hosts)

This section covers the `ffmpeg-encode-hevc-nvenc` encoder path. It mirrors the VAAPI
procedure above but targets a Linux host with a consumer or prosumer NVIDIA GPU.

### Prerequisites (NVENC)

**Minimum NVIDIA driver: ≥ 535** (required for reliable HEVC NVENC on Pascal/Turing/Ampere
and later consumer GPUs; older drivers may have session-count limits or missing codec support).

Verify:
```bash
nvidia-smi --query-gpu=name,driver_version --format=csv,noheader
# Expected: <GPU name>, 535.x.x or higher
```

Check that `libnvidia-encode.so.1` is present on the host (loaded lazily by ffmpeg at runtime):
```bash
ldconfig -p | grep libnvidia-encode
# Expected: at least one line containing libnvidia-encode.so.1
```

If absent, install the matching NVIDIA driver package (e.g. `nvidia-driver-535` on Ubuntu).

### `auto` policy and `PRDT_PREFER_NVENC` override

When the binary is compiled with **both** `ffmpeg-encode-hevc-vaapi` and
`ffmpeg-encode-hevc-nvenc`, the `--encoder auto` policy prefers VAAPI by default
(Intel iGPU is the more common deployment). To flip the preference to NVENC, set the
environment variable before starting the host:

```bash
export PRDT_PREFER_NVENC=1   # accepted truthy values: 1, true, yes, on (case-insensitive)
                              # any other value (including empty) is treated as unset
prdt host --encoder auto ...
```

The startup log will confirm which backend was selected:
```
INFO video encoder selected encoder="ffmpeg-nvenc-hevc" selected_by="auto" reason="preferred-over-vaapi-by-env"
```

Without the override, the log will show:
```
INFO video encoder selected encoder="ffmpeg-vaapi-hevc" selected_by="auto" reason="preferred-over-nvenc"
```

On a binary compiled with **only** `ffmpeg-encode-hevc-nvenc` (no VAAPI), `auto` resolves
to NVENC unconditionally:
```
INFO video encoder selected encoder="ffmpeg-nvenc-hevc" selected_by="auto" reason="only-backend-compiled"
```

### Get the binary (NVENC)

Download the `prdt-linux-x86_64-ffmpeg-nvenc-hevc` artifact from the
`smoke-build-ffmpeg-hevc.yml` workflow run (same run as the VAAPI artifact).

Runtime deps (apt): `libavcodec60 libavutil58 libavfilter9 libavformat60` (Ubuntu 24.04 names).
`libnvidia-encode.so.1` is loaded lazily by ffmpeg — it does **not** appear in `ldd` output
of the binary itself; it is resolved at runtime from the NVIDIA driver installation.

### Start host (NVENC)

```bash
prdt host \
    --encoder ffmpeg-nvenc-hevc \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless \
    2>&1 | tee host-nvenc.log
```

### Monitor GPU utilization

While the session runs, confirm NVENC encode is active:

```bash
nvidia-smi dmon -s u -d 2
# Expected: non-zero values in the `enc` column for your GPU index
```

### Common failure modes (NVENC)

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `EncoderNotFound("hevc_nvenc")` in log | `libnvidia-encode.so.1` not found at runtime | Install NVIDIA driver ≥ 535 |
| `av_hwdevice_ctx_create(CUDA) returned -1` | No `/dev/nvidia0` device node or CUDA not initialized | `sudo modprobe nvidia`; verify with `nvidia-smi` |
| `HwDevice` error at startup, `nvidia-smi` shows GPU | Driver/CUDA version mismatch | Reinstall driver matching CUDA runtime |
| Consumer GPU session limit hit (silent drop) | Some consumer GPUs limit concurrent NVENC sessions | Close other NVENC consumers (OBS, browser HW accel) |
| High CPU usage despite NVENC | CPU-side BGRA→NV12 conversion (tracked as ADR follow-up F4) | Expected in P1.5; CUDA NPP path is out of scope here |

### NVENC run log

**Date:** `<YYYY-MM-DD>`
**GPU SKU:** `<RTX ...>`
**Driver version:** `<535.x.x>`
**Viewer:** `<machine>`
**Result:** PASS / FAIL
**Encoder selected log line confirmed:** Y / N
**`nvidia-smi dmon enc` non-zero:** Y / N
**CPU readback warnings:** `<N>`
**Notes:**

---

## Linux↔Linux HEVC viewer (P2)

This section covers Linux viewer decode of an HEVC stream from a Linux host. All three
decode backends (SW, VAAPI, NVDEC) are deferred HW smoke (A4/A11); this procedure is
the **manual pre-merge regression-guard** for the OpenH264 H.264 path (A12) and the
viewer dispatch wiring (A12.a).

> **SW HEVC backend handles 1080p60 within the latency budget on a modern CPU
> (i7-12700 / Ryzen 7700 or better). 4K60 SW decode is functional but consumes
> 70–100% of a core per stream and exceeds the per-frame latency target; users on
> 4K60 should select VAAPI (Intel/AMD iGPU) or NVDEC (NVIDIA dGPU).**

### Build the viewer with a decode backend

Choose one backend. All examples use the dev-container (bookworm, ffmpeg5 headers):

```bash
# SW HEVC decode (universal fallback, no GPU required)
./scripts/dev-container.sh bash -c '
  cargo build -p prdt-client --features ffmpeg-decode-hevc-sw-ffmpeg5 --release --target x86_64-unknown-linux-gnu
'

# VAAPI HEVC decode (Intel/AMD iGPU)
./scripts/dev-container.sh bash -c '
  cargo build -p prdt-client --features ffmpeg-decode-hevc-vaapi-ffmpeg5 --release --target x86_64-unknown-linux-gnu
'

# NVDEC HEVC decode (NVIDIA GPU)
./scripts/dev-container.sh bash -c '
  cargo build -p prdt-client --features ffmpeg-decode-hevc-nvdec-ffmpeg5 --release --target x86_64-unknown-linux-gnu
'
```

Binary lands at `./target-docker/x86_64-unknown-linux-gnu/release/prdt`.

### Start the Linux host (VAAPI HEVC encode)

```bash
prdt host \
    --encoder ffmpeg-vaapi-hevc \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless \
    2>&1 | tee host-linux.log
```

### Start the Linux viewer

Replace `<host-ip>`, `<pubkey>`, and `<decoder>` with the actual values:

```bash
# Explicit decoder selection:
./target-docker/x86_64-unknown-linux-gnu/release/prdt connect \
    --host <host-ip>:9000 \
    --host-pubkey <pubkey> \
    --codec h265 \
    --decoder ffmpeg-vaapi-hevc   # or ffmpeg-sw-hevc / ffmpeg-nvdec-hevc / auto
```

Expected line in viewer log:
```
INFO video.pipeline event="decoder_ready" backend="ffmpeg-vaapi-hevc" codec="h265"
```

### `auto` decode policy and `PRDT_PREFER_NVDEC` override

When both VAAPI and NVDEC are compiled in, `--decoder auto` picks **VAAPI first** (power
budget: iGPU ~5 W vs. dGPU ~25 W at the same 1080p60 decode workload). To flip to NVDEC:

```bash
export PRDT_PREFER_NVDEC=1   # accepted: 1, true, yes, on (case-insensitive)
prdt connect --decoder auto ...
```

Log confirms the choice:
```
INFO video.pipeline decoder="ffmpeg-vaapi-hevc" selected_by="auto" reason="preferred-over-nvdec"
# or with PRDT_PREFER_NVDEC=1:
INFO video.pipeline decoder="ffmpeg-nvdec-hevc" selected_by="auto" reason="preferred-over-vaapi-by-env"
```

### Linux↔Linux run log

**Date:** `<YYYY-MM-DD>`
**Host:** `<iGPU/dGPU SKU>`
**Viewer decoder:** `<ffmpeg-vaapi-hevc / ffmpeg-sw-hevc / ffmpeg-nvdec-hevc>`
**Result:** PASS / FAIL
**decoder_ready log line confirmed:** Y / N
**Notes:**

---

## NVENC + NPP variant (P2.5 — NVIDIA GPU hosts, GPU-side BGRA→NV12)

This section covers the `ffmpeg-encode-hevc-nvenc-npp` encoder path (P2.5). It
replaces the CPU `bgra_to_i420` + `i420_to_nv12_into` chain with a single CUDA
NPP conversion on the GPU, uploading BGRA once via `cudaMemcpy2D` and writing
NV12 directly into the NVENC hwframe surface.

### Prerequisites (NVENC + NPP)

**Minimum NVIDIA driver: ≥ 535** (same floor as P1.5 NVENC plain path; matches
CUDA 12.x forward-compat requirement).

**CUDA runtime: ≥ 12.0** — verified at startup via `cudaDriverGetVersion`.
`libcudart.so.12` and `libnppicc.so.12` must be present on the host at runtime:

```bash
nvidia-smi --query-gpu=name,driver_version --format=csv,noheader
# Expected: <GPU name>, 535.x.x or higher

ldconfig -p | grep -E 'libcudart|libnppicc'
# Expected: at least one line each for libcudart.so.12 and libnppicc.so.12
```

If `libnppicc.so.12` is absent, install the matching CUDA NPP runtime package.
On Ubuntu 22.04 with the NVIDIA apt repo:

```bash
sudo apt-get install libnpp-12-4
```

### Build (Arch FFmpeg 7 — recommended for NVIDIA hosts running Arch Linux)

```bash
# ffmpeg-encode-hevc-nvenc-npp transitively enables ffmpeg-encode-hevc-nvenc.
cargo build -p prdt-client --release \
  --no-default-features \
  --features ffmpeg-encode-hevc-nvenc-npp-ffmpeg7 \
  --target x86_64-unknown-linux-gnu
```

Or download the `prdt-linux-x86_64-ffmpeg-hevc-encode-npp` artifact from the
`smoke-build-ffmpeg-hevc.yml` workflow run (built with ffmpeg6 default ABI +
VAAPI + all 3 decoders).

### Start host (NVENC + NPP)

```bash
prdt host \
    --encoder ffmpeg-nvenc-hevc-npp \
    --bind 0.0.0.0:9000 \
    --monitor 0 \
    --bitrate-mbps 8 \
    --key-file host-key.bin \
    --silent-allow --headless \
    2>&1 | tee host-npp.log
```

Expected startup log line (confirms NPP path active):

```
INFO video.pipeline event="encoder_ready" backend="ffmpeg-nvenc-hevc-npp" codec="h265" profile="main" bitdepth=8
INFO video.pipeline event="first_frame_emitted" codec="hevc_nvenc" hw_path="cuda" convert_path="npp" ...
```

The `convert_path="npp"` field distinguishes the NPP path from plain NVENC
(`convert_path="sw"`).

### Runtime semantics (R5 — build feature × host hardware matrix)

| Build features | Host hardware | `--encoder ffmpeg-nvenc-hevc-npp` | `--encoder auto` |
|---|---|---|---|
| `nvenc + npp` | NVIDIA dGPU + libnppicc loaded | NPP path runs; log emits `convert_path="npp"` | Picks plain NVENC (`auto` never selects NPP — opt-in only per Step 4 decision) |
| `nvenc + npp` | Non-NVIDIA host or libnppicc absent | `HevcNvencNppFfmpegEncoder::new` fails; exits with `FfmpegError::HwDevice` — **NOT** silent fallback | Falls through `auto` chain normally (VAAPI > NVENC > openh264) |
| `nvenc` only (no npp) | Any | `normalize_encoder` warns and falls back to openh264; log emits `WARN ffmpeg-nvenc-hevc-npp not compiled in` | Plain NVENC selected normally |

`--encoder auto` **never** selects NPP — the policy is byte-stable from P1.6.
Use `--encoder ffmpeg-nvenc-hevc-npp` (or the legacy alias `--encoder nvenc-npp`)
to explicitly opt in. A `PRDT_PREFER_NPP` env override is deferred as ADR F8.

### Monitor GPU utilization (NVENC + NPP)

```bash
# NVENC encoder utilization (enc column should be non-zero):
nvidia-smi dmon -s u -d 2

# CUDA memory activity (correlated to fps × frame size):
nvidia-smi dmon -s mu -d 2

# CPU usage delta vs plain NVENC — bgra_to_i420 + i420_to_nv12_into
# should no longer appear in perf top:
perf top -p $(pgrep -f 'prdt host') 2>/dev/null | head -20
```

### 5-grep verification (after ≥ 5-min soak)

```bash
# 1. encoder= field confirms NPP backend
grep 'convert_path' host-npp.log | head -3

# 2. First frame used NPP path
grep -c 'convert_path="npp"' host-npp.log
# Expected: >= 1

# 3. No CPU readback warnings
grep -c 'cpu_readback' host-npp.log
# Expected: 0

# 4. Frame count (≥ 17000 for 5-min @ ~57 fps)
grep -c 'frame_decoded seq=' viewer.log
# Expected: >= 17000

# 5. Sequence gap check
grep -oE 'frame_decoded seq=[0-9]+' viewer.log | awk -F= '{print $2}' | sort -n | \
  awk 'NR==1{min=$1} {max=$1; count++} END{if (max-min == count-1) print "OK"; else print "GAPS"}'
# Expected: OK
```

### Per-frame CPU usage delta vs plain NVENC

Run back-to-back 5-min sessions: one with `--encoder ffmpeg-nvenc-hevc` and one
with `--encoder ffmpeg-nvenc-hevc-npp`. Capture `perf stat -e task-clock` for
both. At 1080p60 the NPP path eliminates ~1–3 ms of CPU conversion per frame;
expect `task-clock` to drop by ≥ 10% on the encode thread.

### Common failure modes (NVENC + NPP)

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `HwDevice("cudaDriverGetVersion returned ... not loadable")` | libcudart.so.12 absent | Install `cuda-cudart-12-4` runtime package |
| `HwDevice("CUDA driver too old: ... < 12000")` | Driver < 535 | Upgrade NVIDIA driver to ≥ 535 |
| `Transfer(-9999)` (NPP_NOT_IMPLEMENTED_ERROR) | libnppicc.so.12 stub loaded instead of real lib | Install `libnpp-12-4` runtime; check `ldconfig -p \| grep libnppicc` |
| `EncoderNotFound("hevc_nvenc")` | libnvidia-encode.so.1 absent | Install NVIDIA driver ≥ 535 |
| High CPU still in `perf top` | BGRA capture overhead (not conversion) | Expected; X11ShmCapturer CPU is separate from NPP conversion CPU |

### NVENC + NPP run log

**Date:** `<YYYY-MM-DD>`
**GPU SKU:** `<RTX ...>`
**Driver version:** `<535.x.x>`
**CUDA version:** `<12.x>`
**libnppicc version:** `<12.x>`
**Viewer:** `<machine>`
**Result:** PASS / FAIL
**convert_path="npp" confirmed:** Y / N
**`nvidia-smi dmon enc` non-zero:** Y / N
**CPU usage delta vs plain NVENC:** `<N>%`
**CPU readback warnings:** `<N>`
**Notes:**

---

## Pre-merge regression-guard (A12)

**Mandatory before any PR touching `crates/viewer/src/lib.rs` or
`crates/viewer/src/platform/linux.rs`.**

The unit test suite covers A12.a (dispatch) and A12.b (round-trip) automatically:

```bash
# A12.a + A12.b — runs in < 1 s, no display required
./scripts/dev-container.sh bash -c \
  'cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu build_consumer'

# A12.b specifically
./scripts/dev-container.sh bash -c \
  'cargo test -p prdt-viewer --lib --target x86_64-unknown-linux-gnu a12b_openh264_round_trip'
```

A12.c fallback (if the above proves infeasible due to renderer entanglement — currently
not needed since A12.b is a pure unit test):

```bash
cargo test -p prdt-viewer --features openh264-decode -- --ignored
```

All three tests must pass (zero failures) before the PR is merged.
