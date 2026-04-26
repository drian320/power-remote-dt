# Bench matrix (Plan 4 B1)

The `prdt-bench-matrix` bin sweeps the cartesian product of
**resolutions × bitrates × decoders × fps** through the in-process
loopback NVENC + MF/NVDEC pipeline. Each config records per-frame raw
samples and aggregates to a single row in `summary.csv`.

## Quick start

```bash
# Default 60-config sweep (3 res × 5 bitrates × 2 decoders × 2 fps,
# 10s each, ~15-20 min total on RTX 3070 Ti).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/2026-04-26/

# Custom subset (e.g. only NVDEC at 60fps).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/nvdec-only/ \
    --decoders nvdec \
    --fps 60

# Dry-run (print configs, don't execute).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` + `per-frame/` go here. Overwrites existing files. |
| `--resolutions <heights>` | `1080,1440,2160` | 16:9 width auto-derived |
| `--bitrates <mbps>` | `5,10,20,30,50` | Comma-separated Mbps |
| `--decoders <list>` | `mf,nvdec` | Choices: `mf`, `nvdec` |
| `--fps <list>` | `60,120` | Comma-separated |
| `--duration <humantime>` | `10s` | Per-config bench length |
| `--dry-run` | off | List configs, exit |

## Output layout

```
bench-results/<date>/
  summary.csv                  # 1 header + N config rows
  per-frame/
    1080p60-5mbps-mf.csv       # 1 header + ~600 frame rows
    1080p60-5mbps-nvdec.csv
    ...
    2160p120-50mbps-nvdec.csv
```

`config_id` format: `{height}p{fps}-{bitrate}mbps-{decoder}` —
ASCII, filesystem-safe, used as both the per-frame filename and
the leftmost column of `summary.csv`.

## summary.csv schema

```
config_id,resolution,bitrate_mbps,decoder,fps,sent,received,loss_ppm,
arrival_p50_us,arrival_p95_us,arrival_p99_us,
decode_p50_us,decode_p95_us,decode_p99_us,
e2e_p50_us,e2e_p95_us,e2e_p99_us
```

- `arrival_lag = recv_us - capture_us` (post-encode → arrived at the receive end)
- `decode_lag = decode_done_us - recv_us`
- `e2e_lag = decode_done_us - capture_us` — proxy for glass-to-glass; a
  true present-time stamp requires Plan 4 M3 camera measurement.

A skipped config (NVENC init failure, decoder unsupported, etc.)
emits a row with `loss_ppm = 1000000` and all percentiles = 0. The
log will say `config failed; skip row will be emitted`.

## per-frame/<config_id>.csv schema

```
seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us
```

The lag columns are pre-computed for analysis convenience.

## Sample interpretation

A row reading

```
1080p60-30mbps-nvdec,1920x1080,30,nvdec,60,600,600,0,1234,1890,2100,420,580,720,4500,7200,9100
```

means: 600 frames sent and all 600 received (loss_ppm=0); transport
arrival p95 was 1.89ms, decode p95 was 0.58ms, end-to-end p95 was
7.2ms (p99 9.1ms). Compared with the same config under MF decoder,
expect NVDEC to be lower across all three stages thanks to the
zero-copy CUDA→D3D11 path (Plan 2d zerocopy).

## Limitations

- **No GPU adapter / non-NVIDIA**: bin fails fast on first config with
  "no GPU adapter" (NVENC requires NVIDIA).
- **Single-process loopback**: encode and decode share the same GPU,
  same monotonic clock. Real 2-machine LAN behaviour will differ
  (clock-offset correction needed via Plan 4 M3 ping/pong).
- **No present_us**: the bin renders nothing; `e2e_lag` ends at
  `decode_done_us`. True glass-to-glass requires Plan 4 M3.
- **Resume not supported**: a run that crashes mid-sweep loses progress
  beyond what's already written under `per-frame/`. Re-run with a
  reduced axis subset to fill in.
- **Inter-config delay (250 ms)** is inserted between configs so the
  previous NVENC/NVDEC/CUDA context teardown completes before the next
  config rebuilds them. Without this, an occasional config (observed:
  `2160p60-30mbps-nvdec` after `2160p60-20mbps-nvdec`) initialises
  with state still leaking from the previous run and produces
  `sent=1 received=0`. Total sweep wall-time grows by `250 ms × (N-1)`
  (15 s for the 60-config default) -- negligible.
