# H.265 Encoder Backends

power-remote-dt ships two H.265 encoder backends selectable at runtime.

---

## Backend comparison

| | `nvenc` | `mf` |
|---|---|---|
| **Hardware required** | NVIDIA GPU (Kepler+) | Any DXGI adapter (AMD, Intel, NVIDIA) |
| **API** | NVIDIA Video Codec SDK 13 (NVENC) | Windows Media Foundation MFT |
| **Latency (typical, 1080p60)** | ~5–8 ms encode | ~10–20 ms encode |
| **Quality** | Very good (CBR, low-latency profile) | Good (H.265 HW MFT) |
| **Input format** | BGRA8 D3D11 texture (converted internally via CUDA) | BGRA8 → NV12 via D3D11 VideoProcessor |
| **Min Windows version** | Windows 10 (with NVIDIA driver) | Windows 10 1703+ (HEVC extensions from MS Store) |
| **Availability check** | `NvEncLibrary::load()` success | `MFTEnumEx` finds an H.265 HW MFT |

---

## Auto-selection rules (`--encoder auto`)

The host binary (and `prdt-bench-matrix --encoder auto`) picks a backend in this order:

1. If the capture adapter is NVIDIA **and** `nvenc` initialises without error → use `nvenc`.
2. Otherwise, attempt `mf`. If no H.265 MFT is found, return an error.

To force a specific backend, override with `--encoder nvenc` or `--encoder mf`.

---

## Host CLI

```
prdt-host.exe [OPTIONS]

Options:
  --encoder <auto|nvenc|mf>   H.265 encoder backend [default: auto]
```

Examples:

```powershell
# Default: auto-select based on adapter
.\prdt-host.exe

# Force NVENC (fails if no NVIDIA GPU)
.\prdt-host.exe --encoder nvenc

# Force Media Foundation (works on AMD/Intel, slower on NVIDIA)
.\prdt-host.exe --encoder mf
```

The setting is also persisted in `%APPDATA%\prdt\config.toml` under `[host].encoder`.

---

## Latency bench CLI

```
prdt-latency-bench.exe --mode full-pipeline-win --encoder <nvenc|mf> [OPTIONS]
```

```powershell
# NVENC encoder + MF decoder, 5 s, 1080p60
.\prdt-latency-bench.exe --mode full-pipeline-win --encoder nvenc --consumer mf

# MF encoder + NVDEC decoder
.\prdt-latency-bench.exe --mode full-pipeline-win --encoder mf --consumer nvdec
```

---

## Bench matrix CLI

```
prdt-bench-matrix.exe --out-dir results --encoders nvenc,mf [OPTIONS]
```

Adding `--encoders mf` doubles the matrix: for each (resolution, bitrate, fps) configuration, both the `nvenc` and `mf` encoder paths are exercised against each decoder.

```powershell
# Full 4-way sweep: NVENC+MF encoders × MF+NVDEC decoders
.\prdt-bench-matrix.exe --out-dir results\full-sweep --encoders nvenc,mf --decoders mf,nvdec
```

The summary CSV (`results/summary.csv`) has an `encoder` column (`nvenc` / `mfenc`) and a `decoder` column (`mfdec` / `nvdec`). Config IDs use the format `{h}p{fps}-{bitrate}mbps-enc{enc}-dec{dec}`, e.g. `1080p60-30mbps-encnvenc-decmfdec`.

---

## Manual smoke test

```powershell
# 1. Build
$env:NV_CODEC_SDK_PATH = "C:/SDK/Video_Codec_SDK_13.0.37"
$env:LIBCLANG_PATH     = "C:/Program Files/LLVM/bin"
$env:CUDA_PATH         = "C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
cargo build --release -p prdt-host -p prdt-latency-bench

# 2. Transport-only bench (no GPU needed)
.\target\release\prdt-latency-bench.exe --fps 60 --duration 3s

# 3. Full-pipeline NVENC smoke
.\target\release\prdt-latency-bench.exe --mode full-pipeline-win --encoder nvenc --consumer mf --duration 5s

# 4. Full-pipeline MF encoder smoke
.\target\release\prdt-latency-bench.exe --mode full-pipeline-win --encoder mf --consumer mf --duration 5s

# 5. Host with MF encoder (start then Ctrl-C)
.\target\release\prdt-host.exe --encoder mf --headless
```

Expected in logs:
- Step 2: `lag_p95_us` < 200 µs
- Steps 3–4: `e2e_p95_us` < 50 ms; no "encode failed" / "init failed" lines
- Step 5: `encoder=mf backend` log line, then normal frame-send loop

---

## HEVC extensions requirement (MF backend)

The `mf` encoder path calls `MFTEnumEx` with `MFT_ENUM_FLAG_HARDWARE`. This requires the **HEVC Video Extensions** package installed from the Microsoft Store (or pre-installed by the GPU driver on AMD/Intel systems). If the package is absent, `MfH265Encoder::new()` returns `MediaError::Other("no H.265 MFT found")` and the host falls back to an error (or NVENC if `--encoder auto`).

---

## Known limitations

### NVIDIA's MF HEVC MFT does not honor bitrate hints

On NVIDIA hardware, Windows' MF H.265 encoder MFT (provided by the NVIDIA
driver) appears to ignore `ICodecAPI` rate-control attributes
(`AVEncCommonRateControlMode`, `AVEncCommonMeanBitRate`,
`AVEncCommonMaxBitRate`, `AVEncMPVGOPSize`). At a 20 Mbps target, observed
frame sizes:

| frame | NVENC (reference) | MF MFT on NVIDIA |
|---|---|---|
| IDR | ~50 KB | ~470 KB (uncapped) |
| P   | ~5 KB  | ~100–300 KB |

This causes the FEC packetizer (per-frame budget ~75 KB) to drop most
frames after the initial IDR. Use `--encoder nvenc` on NVIDIA hardware
for production. This is the default auto-select behavior — `--encoder mf`
is intended for non-NVIDIA fallback only.

### AMD / Intel hardware: untested

The implementation conforms to the Microsoft MFT contract (async event
protocol, NV12 input, Annex-B output) and should work on AMD VCN and
Intel QuickSync HEVC encoders. End-to-end smoke testing on those
platforms has not been performed and may surface additional
driver-specific quirks.

---

## Future work

- Live bitrate reconfiguration for `nvenc` (currently a no-op warn; bitrate is set at construction).
- AV1 backend (`EncoderBackend::Av1Nvenc`) — requires Ada Lovelace (RTX 40-series) GPU.
- D3D12 Video Encode path (lower CPU overhead on recent drivers).
- AMD / Intel end-to-end smoke test to verify bitrate compliance on non-NVIDIA MF MFTs.
