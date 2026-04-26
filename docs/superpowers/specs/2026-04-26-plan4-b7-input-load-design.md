# Plan 4 B7 Input-under-load Bench — Design Spec

**Date:** 2026-04-26
**Tag (on completion):** `plan4-b7-input-load-bench-complete`
**Scope:** Software-only `prdt-input-load-bench` measuring InputEvent
send→recv lag while a concurrent synthetic video stream consumes
transport bandwidth.

## Goal

Provide a single-process bench that quantifies how InputEvent
delivery latency degrades when the same `InProcTransport` is also
busy carrying video frames. Sweep `(input_rate_hz × video_rate_fps)`
and report per-event lag p50/p95/p99 + loss rate.

## Why this is "B7 software-only"

True `B7` (input round-trip including screen response) requires a
camera or external timing apparatus (`Plan 4 M3`). Without that
hardware we measure the **protocol/transport portion only**: from
`Transport::send_input(InputEvent)` returning to the matching
`recv() -> ReceivedMessage::Input(_)` completing on the other side.
This is one-way send-to-recv lag. Real-world RTT ≈ 2× this number;
glass-to-glass is RTT + capture overhead + display refresh
(unmeasurable from software).

The bench is most useful for catching regressions where input
queueing under video load suddenly explodes (e.g. an unbounded
channel filling up).

## Non-goals

- Round-trip including a host-side echo (keeps protocol unchanged)
- Real input injection via `SendInputInjector` (no value here; the
  injector is sync and ~µs)
- Camera-based glass-to-glass measurement (Plan 4 M3)
- Video encode via NVENC (synthetic frame bytes are sufficient
  for transport load)
- Variable per-event payload (InputEvent is fixed-size; only the
  per-frame video size axis varies)
- Bursty input patterns (uniform-rate sender)

## Architecture

```
crates/latency-bench/
  src/bin/
    input-load-bench.rs        ← new prdt-input-load-bench bin
                                 CLI + matrix expand + per-config
                                 sender/receiver tasks + aggregate
                                 + summary CSV
docs/
  input-load-bench.md          ← usage + schema + interpretation
```

The bin reuses:
- `prdt_transport::loopback::{InProcTransport, LoopbackOptions}`
- `prdt_transport::Transport` trait (`send_input`, `send_video`, `recv`)
- `prdt_transport::ReceivedMessage::{Input, Video, Control, Audio}`
- `prdt_protocol::InputEvent::MouseMove { x, y, absolute }`
- `prdt_protocol::EncodedFrame` (synthetic NAL bytes)
- `prdt_protocol::now_monotonic_us`
- `prdt_latency_bench::percentiles`

No GPU, no async runtime beyond what `tokio::main` provides, no
crypto, no real input injection. Cross-platform — no
`#![cfg(windows)]` gate.

### Verified APIs

```rust
// prdt_transport
pub enum ReceivedMessage {
    Video(EncodedFrame),
    Audio(AudioPacket),
    Input(InputEvent),
    Control(ControlMessage),
}

pub struct LoopbackOptions { pub drop_ppm: u32, pub latency: Option<Duration> }

impl InProcTransport {
    pub fn pair(opts: LoopbackOptions) -> (Self, Self);
}

#[async_trait]
pub trait Transport {
    async fn send_input(&self, ev: InputEvent) -> Result<(), TransportError>;
    async fn send_video(&self, frame: EncodedFrame) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<ReceivedMessage, TransportError>;
}

// prdt_protocol
pub enum InputEvent {
    MouseMove { x: i32, y: i32, absolute: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    Key { scancode: u32, pressed: bool },
}
```

`InputEvent` has **no timestamp field**. The bench works around this
by having the input sender push `sent_us: u64` into a side
`tokio::sync::mpsc` queue, and the receiver pops one timestamp per
`ReceivedMessage::Input(_)` it sees. Since `InProcTransport` is FIFO
and `drop_ppm=0` for this bench, ordering is preserved.

## CLI

```
--out-dir <path>                              # required
--input-rates 100,500,1000,5000               # Hz, comma-separated
--video-rates 0,60,120                        # fps, 0 = no concurrent video
--video-frame-bytes 50000                     # synthetic frame size, ~30Mbps@60fps
--duration 5s                                 # per config
--inter-config-delay-ms 250                   # spacing between configs
--dry-run
```

Default sweep: 4 input × 3 video = **12 configs**, 5 s each + 250 ms
spacing = ~63 s wall time.

## Trial flow per config

1. Build `(host_side, viewer_side) = InProcTransport::pair(LoopbackOptions::default())`.
2. Build a `tokio::sync::mpsc::unbounded_channel<u64>()` named `sent_ts_tx / sent_ts_rx`. The viewer-side sender pushes `sent_us` per event; the host-side receiver pops one per `Input(_)`.
3. `let cancel = tokio_util::sync::CancellationToken::new();`
4. `let deadline = Instant::now() + cfg.duration;`
5. Spawn three tasks:
   - **input_sender**: every `1/input_rate_hz` interval until deadline:
     1. `let now = now_monotonic_us();`
     2. `sent_ts_tx.send(now).ok();`
     3. `viewer_side.send_input(InputEvent::MouseMove { x: 0, y: 0, absolute: false }).await.ok();`
     4. counter `input_sent += 1`
   - **video_sender** (only if `video_rate_fps != 0`): every `1/video_rate_fps` interval until deadline, send a synthetic `EncodedFrame { seq, timestamp_host_us: now_monotonic_us(), is_keyframe: seq % 30 == 0, nal_units: Bytes::from(vec![0; cfg.video_frame_bytes]), width: 1920, height: 1080, codec: Codec::H265 }`. counter `video_sent`.
   - **receiver**: loops `host_side.recv().await`. On `Ok(ReceivedMessage::Input(_))`, pop `sent_us` from `sent_ts_rx` and record `lag_us = now_monotonic_us() - sent_us`. On `Ok(ReceivedMessage::Video(_))`, just discard. On error or other variants, continue. Exit on cancel.
6. `tokio::select!` on the deadline to cancel.
7. Drain remaining receiver events for ~50 ms to capture in-flight messages.
8. `aggregate(cfg, sent, received, &lags) -> ConfigStats`.

## Aggregation

```rust
struct ConfigStats {
    config_id: String,
    input_rate_hz: u32,
    video_rate_fps: u32,
    duration_ms: u64,
    input_sent: u64,
    input_received: u64,
    input_loss_ppm: u64,
    input_p50_us: u64,
    input_p95_us: u64,
    input_p99_us: u64,
}
```

- `input_loss_ppm = (input_sent - input_received) * 1_000_000 / max(1, input_sent)`
- Empty `lags` → emit zeros for percentiles.
- Use `prdt_latency_bench::percentiles(&mut lags)` and extract
  positions (p50, p95, p99) from the 5-tuple.

## Output `summary.csv`

```
config_id,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us
```

`config_id`: `in{rate}hz-vid{fps}fps`, e.g. `in100hz-vid0fps`,
`in5000hz-vid120fps`. ASCII, filesystem-safe.

## Tests (5 unit)

1. `expand_matrix_cartesian` — input_rate outer / video_rate inner
2. `config_id_format_canonical` — verifies the two example strings
3. `aggregate_empty_lags_emits_zeros` — empty `lags` produces 0 percentiles, but counts still recorded
4. `aggregate_full_lags_computes_percentiles` — known 100-element lag vector
5. `summary_csv_writer_emits_header_and_one_row` — exact header + 1 row content via tempfile

No NVENC, no async required for any of these (the bench's async paths
are not directly unit-tested; their behaviour is exercised by manual
smoke).

## Error handling

- `send_input` / `send_video` fail (transport closed): break sender
  loop. Receiver continues until deadline or empty drain.
- Receiver gets `Err(_)`: log warn, continue (next iteration may
  succeed or fail again — either way the deadline ends it).
- Empty `sent_ts_rx` when `Input` arrives (shouldn't happen at
  drop_ppm=0): record lag=0 and continue. This is a sentinel; if
  it ever fires we want to know.
- CSV write error: `anyhow::Result<()>` propagation from `main`.

## Progress logging

```
[ 1/12] running in100hz-vid0fps duration=5s
[ 1/12] done    in100hz-vid0fps sent=500 received=500 input_p95_us=8
[ 2/12] running in100hz-vid60fps duration=5s
...
```

## Risks & Notes

- **Inter-config delay**: B1 hit transient state-leak issues without a
  delay between configs. InProcTransport doesn't have GPU-state
  concerns, but a 250 ms delay (matching B1) is cheap insurance and
  also lets the receiver fully drain.
- **`tokio::sync::mpsc::unbounded_channel<u64>`** for sent_ts: backed
  by a Vec, no bounded slot pressure. Memory cost: 8 bytes × events
  ≤ 5000 Hz × 5 s = 25_000 entries × 8 = 200 KB. Fine.
- **`MouseMove { x: 0, y: 0, absolute: false }`** is the smallest
  encoded InputEvent variant (a few bytes wire). Choice is arbitrary
  but stable across runs.
- **Synthetic video frame**: `Bytes::from(vec![0; N])` is zero-filled.
  packetize at the transport layer would normally compress + FEC, but
  InProcTransport doesn't run packetize; it just shuttles the
  EncodedFrame as a single message via mpsc. So the cost on the
  channel is `O(N)` allocation per frame — for 50_000 bytes × 120 fps
  × 5 s = 30 MB of allocation churn per video=120 config. Acceptable.
- **`sent` counter reset between configs**: each config starts from
  zero. The send and receive counters are local to the per-config
  block.

## Exit criteria

1. `cargo build --release -p prdt-latency-bench --bin prdt-input-load-bench` clean
2. `cargo test -p prdt-latency-bench --bin prdt-input-load-bench` passes (5 new tests)
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
4. **Manual smoke**: `prdt-input-load-bench --out-dir bench-results/input-load-smoke/` runs and produces `summary.csv` with 12 rows. All `input_loss_ppm == 0` (or near-zero). `input_p95_us` for `vid=0` configs should be sub-100µs; for `vid=120` configs at high input rates it may rise into the ms range — that's the signal.
5. `docs/input-load-bench.md` includes usage + schema + sample interpretation
6. tag `plan4-b7-input-load-bench-complete` created

## Estimate

- spec (this doc): 0.25 d
- plan: 0.25 d
- implementation + tests: 0.5 d
- total: ~1 d
