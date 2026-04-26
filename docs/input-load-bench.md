# Input-under-load bench (Plan 4 B7)

The `prdt-input-load-bench` bin measures one-way send-to-recv lag for
`InputEvent` messages while a concurrent synthetic video stream
shares the same `InProcTransport`. Used to spot regressions where
input queueing under video load suddenly explodes (e.g. an unbounded
channel filling up).

## Quick start

```bash
# Default 12-config sweep (4 input_rates x 3 video_rates), ~63 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load/

# Custom subset (only 1000 Hz at various video rates).
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir bench-results/input-load-1k/ \
    --input-rates 1000 --video-rates 0,60,120,240

# Dry-run.
cargo run --release -p prdt-latency-bench --bin prdt-input-load-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--input-rates <list>` | `100,500,1000,5000` | Hz, comma-separated. |
| `--video-rates <list>` | `0,60,120` | fps, 0 = no video. |
| `--video-frame-bytes <N>` | `50000` | Synthetic frame size. |
| `--duration <humantime>` | `5s` | Per-config bench length. |
| `--inter-config-delay-ms <N>` | `250` | Spacing between configs. |
| `--dry-run` | off | List configs and exit. |

## summary.csv schema

```
config_id,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us
```

`config_id` format: `in{rate}hz-vid{fps}fps`, e.g. `in100hz-vid0fps`,
`in5000hz-vid120fps`.

- `input_sent`, `input_received`: counts of `InputEvent::MouseMove`
  messages. Loss should normally be 0 (InProcTransport does not
  drop with `LoopbackOptions::default()`).
- `input_loss_ppm`: `(sent - received) * 1_000_000 / max(1, sent)`.
- `input_p50_us` / `_p95_us` / `_p99_us`: send-to-recv lag in
  microseconds, computed via `prdt_latency_bench::percentiles`
  (round-style picking). Zero when `input_received == 0`.

## What this measures (and what it does NOT)

This bench measures **only** the protocol/transport portion of input
event delivery: `Transport::send_input(...)` returning to the
matching `recv() -> ReceivedMessage::Input(_)` arriving on the host
side. Both sides share `prdt_protocol::now_monotonic_us`, so the
subtraction is exact.

It does NOT measure:
- Capture overhead (RawInput callback to `send_input`)
- Real network RTT (this is single-process)
- Host-side `SendInputInjector::inject` (the bench skips injection)
- Display refresh / driver latency (Plan 4 M3 territory)

To approximate two-way RTT, double the `input_p50_us` etc.

## Sample interpretation

```
in1000hz-vid60fps,1000,60,5000,5000,5000,0,12,28,45
```

means: 1000 Hz input + 60 fps video for 5 s, all 5000 InputEvents
delivered (loss 0), median lag 12 µs, p95 28 µs, p99 45 µs.

If a future change makes the receive task slower under video load,
the p95 / p99 values will balloon — the loss column is the
secondary signal (events queueing past their deadline, eventually
exceeding mpsc capacity).

## Limitations

- **Single-process only**: real network adds queueing delay,
  reorder, and loss not modelled here.
- **Synthetic video frame**: zero-filled bytes, no NVENC, no FEC.
  Transport layer just shuttles the EncodedFrame as-is.
- **Uniform input rate**: real users emit bursty inputs; this bench
  is a steady-state measurement.
- **MouseMove only**: other InputEvent variants are similar in size;
  the choice is for stability, not generality.
