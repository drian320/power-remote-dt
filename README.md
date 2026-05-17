# power-remote-dt

Ultra-low-latency cross-platform remote desktop.

**Status:** **Phase 0 functionally complete** — pipeline end-to-end verified
on Windows 11 + NVIDIA. Formal benchmark sign-off (spec §7 exit criteria)
deferred. See `docs/superpowers/PHASE0-STATUS.md` for the full accounting.

**Phase 3a complete** — all host ↔ viewer UDP traffic is now end-to-end
encrypted with Noise_NK (Curve25519 + ChaCha20-Poly1305 + BLAKE2s). See
`docs/superpowers/phase3a-smoke.md` for the encrypted-pipeline smoke test.

## Phase 0 Progress

- [x] Plan 1: Foundation (`protocol` + `transport` + `latency-bench` skeleton)
- [x] Plan 2a: `media-win` D3D11 foundation (device, texture, MMCSS)
- [x] Plan 2b: `media-win` DXGI capture + NVENC H.265 encoder
- [x] Plan 2c: `media-win` Media Foundation decode + VideoProducer/VideoConsumer traits
- [x] Plan 3: `input-win` + `host` + `viewer` binaries
- [ ] Plan 4 (deferred): formal benchmarks + Exit Criteria sign-off
- [ ] Plan 2d (optional): cuvid/NVDEC direct for lower-latency decode

## Phase 3 Progress

- [x] Phase 3a: E2E encryption (Noise_NK + Curve25519 + ChaCha20-Poly1305)
- [ ] Phase 3b: Audio (Opus), clipboard sync
- [ ] Phase 3c: File transfer, multi-monitor
- [x] Phase 3d: Authentication hardening (handshake timeout, known-hosts, rekey support)

## Linux — Install via AppImage (no apt needed)

The AppImage bundles FFmpeg 6 + VA-API client + PipeWire client + the
GTK 3 stack, so you can run `prdt` on any modern glibc desktop distro
(Ubuntu 22.04+, Debian 12+, Fedora 39+, etc.) without `apt install`-ing
anything beyond `libfuse2`:

```sh
curl -L -o prdt.AppImage https://github.com/drian320/power-remote-dt/releases/download/<tag>/prdt-<tag>-x86_64.AppImage
chmod +x prdt.AppImage
./prdt.AppImage --help
```

**Hardware encode/decode**: VA-API works out of the box on Intel + AMD
GPUs (you need `mesa-va-drivers` or `intel-media-va-driver` installed —
most desktop distros ship them by default). NVIDIA encode/decode requires
the proprietary NVIDIA driver installed on the host (the AppImage does not
bundle proprietary NVIDIA libs).

**What's NOT in the AppImage**: CUDA NPP (GPU BGRA→NV12) is excluded to
keep the AppImage under 150 MB. NPP users should build from source until a
separate `prdt-cuda-x86_64.AppImage` variant ships (tracked as
F-AppImage-1).

**AppImage vs bare ELF (`prdt-linux-x86_64`)**:

| Aspect | AppImage | Bare ELF |
|---|---|---|
| Filename | `prdt-<ver>-x86_64.AppImage` (~145 MB) | `prdt-linux-x86_64` (~25 MB) |
| Feature set | B-3 (VAAPI + NVENC + NVDEC + Main10 HEVC) | default (`vaapi-h264` only) |
| Runtime deps user must install | only `libfuse2` (or use `--appimage-extract-and-run` if no FUSE) | full apt list: `libasound2t64`, `libavcodec*`, `libva*`, `libpipewire-0.3-0`, etc. |
| glibc floor | 2.35 (Ubuntu 22.04) | ubuntu-24.04 build |
| Distro coverage | Ubuntu 22.04+, Debian 12+, Fedora 39+, Arch, openSUSE | glibc-based only — Alpine/musl excluded for AppImage |

**FUSE-less hosts**: if `libfuse2` isn't available (container, locked
sysadmin policy, some WSL setups), run
`./prdt.AppImage --appimage-extract-and-run host …` — squashfs is
extracted to /tmp at launch, ~1.5 s slower startup but no FUSE needed.

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

On the host machine:
```powershell
$env:NV_CODEC_SDK_PATH = "C:\SDK\Video_Codec_SDK_13.0.37"
.\target\release\prdt-host.exe --bind 0.0.0.0:9000 --monitor 0 `
    --bitrate-mbps 30 --key-file host-key.bin
# Copy the "Host public key: ..." line.
```

On the viewer machine:
```powershell
.\target\release\prdt-viewer.exe --host <host-ip>:9000 `
    --host-pubkey <paste-pubkey-from-host>
```

All traffic between host and viewer is now Noise_NK encrypted end-to-end.

### Using a known-hosts file

Instead of pasting `--host-pubkey` on every run, maintain a known-hosts file:

```text
# known_hosts.txt - one entry per line:
#   <host:port> <base64-pubkey>
192.168.1.5:9000 pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0
127.0.0.1:9000 pBfwMy6qXBDbEyY0nwzoDyFOtJHbWtTNqZxdUjQD9C0
```

And launch:
```powershell
.\target\release\prdt-viewer.exe --host 127.0.0.1:9000 --known-hosts known_hosts.txt
```

See `docs/superpowers/plan3-manual-smoke.md` for the Phase 0 smoke test
procedure and `docs/superpowers/phase3a-smoke.md` for the Phase 3a
encrypted-pipeline smoke test.

## Architecture (Phase 0)

```
[host machine]
  DXGI Desktop Duplication
    → D3D11 BGRA texture
    → NVENC H.265 encode (zero-copy via shared texture)
    → CustomUdpTransport send
                                  ↓ UDP (custom protocol, Noise_NK encrypted)
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
cargo test -p prdt-protocol -p prdt-transport -p prdt-crypto
# With GPU + SDK:
cargo test -p prdt-media-win -p prdt-input-win
```

All 94+ tests pass.
