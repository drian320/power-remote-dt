# Network profile bench (Plan 4 B4)

The `prdt-net-profile-bench` bin sweeps `(latency_ms × drop_ppm)`
profiles using `LoopbackOptions` to inject one-way delay and
message-level drop on top of `InProcTransport`. It reports per-
profile InputEvent + Video send-to-recv lag and loss.

## Quick start

```bash
# Default 20-config sweep (5 latencies x 4 drops), ~105 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile/

# Custom subset (only LAN/metro latencies, no drop sweep).
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir bench-results/net-profile-low/ \
    --latencies-ms 1,5,10 --drops-ppm 0

# Dry-run (list configs).
cargo run --release -p prdt-latency-bench --bin prdt-net-profile-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--latencies-ms <list>` | `0,1,10,50,200` | One-way delay in milliseconds. |
| `--drops-ppm <list>` | `0,1000,10000,50000` | Per-message drop ppm. |
| `--input-rate-hz <N>` | `1000` | Fixed input rate (CLI override only). |
| `--video-rate-fps <N>` | `60` | Fixed video rate (CLI override only). |
| `--video-frame-bytes <N>` | `50000` | Synthetic frame size. |
| `--duration <humantime>` | `5s` | Per-config bench length. |
| `--inter-config-delay-ms <N>` | `250` | Spacing between configs. |
| `--dry-run` | off | List configs and exit. |

## Suggested profile presets

| Profile | `--latencies-ms` | `--drops-ppm` |
|---|---|---|
| localhost | `0` | `0` |
| LAN | `1` | `0` |
| metro | `10` | `1000` (0.1%) |
| WAN | `50` | `10000` (1%) |
| satellite | `600` | `50000` (5%) |
| lossy WiFi | `10` | `100000` (10%) |

To run a single profile, specify both axes with one value each.

## summary.csv schema

```
config_id,latency_ms,drop_ppm,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us,video_sent,video_received,video_loss_ppm
```

`config_id` format: `lat{latency_ms}ms-drop{drop_ppm}ppm`, e.g.
`lat0ms-drop0ppm`, `lat200ms-drop50000ppm`.

- `input_sent` / `input_received` / `video_sent` / `video_received`:
  message counts. Loss is the difference (silent drop in the
  transport layer).
- `*_loss_ppm`: `(sent - received) * 1_000_000 / sent`.
- `input_p50_us` / `_p95_us` / `_p99_us`: send-to-recv lag in
  microseconds for `Input` events only. Round-style percentile
  picking. Zero when no input lags were captured.

## Sample interpretation

```
lat50ms-drop10000ppm,50,10000,1000,60,5000,500,495,10000,50012,50034,50061,300,297,10000
```

means: 50 ms latency + 1% drop, 1000 Hz input + 60 fps video for
5 s. The 50 ms latency blocks the sender, so 5000 events would
take 250 s — only ~500 events fit in 5 s wall time. 1% of those
were dropped (`input_loss_ppm = 10000`). Input p50/p95/p99 sit
around 50 ms (the injected delay), with a few hundred µs of
overhead. Video at 60 fps × 5 s = 300 frames, 1% lost = 297
received.

## What this measures (and what it does NOT)

This bench measures how the **application sees** simulated network
profiles via `LoopbackOptions::latency` and `LoopbackOptions::drop_ppm`.

It does NOT measure:
- **Packet-level loss + FEC interaction**: `InProcTransport` ships
  whole `EncodedFrame` messages; FEC is not exercised. For FEC under
  loss see B6 (`prdt-fec-bench`).
- **Real UDP / network stack overhead**: no `CustomUdpTransport`,
  no socket layer.
- **Jitter / reorder / duplicate packets**: latency is a single
  fixed delay per message, not a distribution.
- **Bandwidth limit**: messages deliver in full byte size with
  no rate cap.
- **TURN-relay overhead**: external TURN server required.
- **Real glass-to-glass display latency** (Plan 4 M3 territory).

## Caveats

- **`latency` blocks the sender**: each `send_*` call sleeps for
  `latency` before completing, capping per-task throughput at
  `1 / latency`. A 1000 Hz input sender at 200 ms latency produces
  ~5 events per second. Counters reflect this; high-latency rows
  will have small `input_sent` and noisy percentiles.
- **`drop_ppm` is per-message, not per-packet**: dropping a video
  frame means the entire frame is missing from the receiver. There
  is no FEC opportunity in this bench.
- **`Bytes::from(vec![0u8; N])` allocations** per video frame: at
  60 fps × 50_000 bytes × 5 s × 20 configs = 600 MB of allocation
  churn over a full sweep. Cheap on modern hardware.
