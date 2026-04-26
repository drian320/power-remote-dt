# Plan 4 B4 Network Profile Bench — Design Spec

**Date:** 2026-04-26
**Tag (on completion):** `plan4-b4-net-profile-bench-complete`
**Scope:** Software-only `prdt-net-profile-bench` measuring InputEvent + Video send→recv lag and message-level loss across simulated `(latency, drop_ppm)` profiles.

## Goal

Provide a single-process bench that quantifies how the application
perceives concurrent input + video traffic under simulated network
profiles (LAN / metro / WAN / satellite / lossy), via
`InProcTransport::pair(LoopbackOptions { latency, drop_ppm })`.
Sweeps `(latency_ms × drop_ppm)` and reports per-profile lag
distribution + loss counts.

## Why this is "B4 software-only"

True B4 (LAN / loopback / TURN relay path comparison) requires:
- 2-machine LAN automation (real UDP, real network stack)
- External TURN server for the relay leg
- Possibly `tc netem` or platform equivalent for controlled drop /
  jitter / reorder

None of these are accessible from a single-process Rust bench. What
we CAN measure is how the application + InProcTransport responds to
**injected one-way delay and message-level drop** — a thin but
useful baseline.

## Non-goals (clearly carved out)

- **Packet-level loss + FEC interaction**: `InProcTransport` ships
  whole `EncodedFrame` messages over a tokio `mpsc`; FEC is not
  exercised. For FEC under loss see B6 (`prdt-fec-bench`).
- **Real UDP**: no `CustomUdpTransport`, no socket layer.
- **Jitter / reorder / duplicate**: `LoopbackOptions::latency` is a
  single fixed delay per message, not a distribution.
- **Bandwidth limit**: messages are delivered in full byte size
  with no rate cap.
- **TURN-relay overhead**: out of scope.
- **Real glass-to-glass** (Plan 4 M3).
- **Variable input/video rate axes**: kept fixed in the default
  matrix to avoid dimension explosion. Override via CLI if needed.

## Verified `LoopbackOptions` semantics

From `crates/transport/src/loopback.rs:13-75` (read 2026-04-26):

```rust
pub struct LoopbackOptions {
    pub drop_ppm: u32,
    pub latency: Option<Duration>,
}

async fn send_msg(&self, msg) -> Result<()> {
    if self.should_drop() { return Ok(()); }            // silent drop
    if let Some(d) = self.opts.latency {
        tokio::time::sleep(d).await;                    // sender blocks
    }
    self.send_tx.send(msg).map_err(|_| PeerClosed)?;
    Ok(())
}
```

Two implications matter:

1. **Drop is silent**: sender receives `Ok(())` even when the message
   was dropped. The receiver simply never sees it. This matches
   real UDP packet loss.
2. **Latency is per-send sender-side blocking**: each `send_*` call
   sleeps `latency` BEFORE completing. The sender's effective
   throughput is therefore capped at `1 / latency` per task. A
   1000 Hz input sender with `latency = 10 ms` produces
   ~100 events/sec, not 1000.

The bench must surface both effects in the output: `input_sent`
will be lower than the configured rate when latency is large, and
`input_received` will be lower than `input_sent` when drop > 0.

## Architecture

```
crates/latency-bench/src/bin/net-profile-bench.rs   ← new bin
                                                      Reuses B7's input/video/receiver
                                                      task pattern, but parameterizes
                                                      LoopbackOptions per config and
                                                      additionally tracks video
                                                      sent/received counts.
docs/
  net-profile-bench.md                              ← usage + schema + interpretation
```

The bin is a near-clone of `prdt-input-load-bench` (B7) with these
deltas:
- `LoopbackOptions { latency: Some(...), drop_ppm: ... }` populated
  per config (B7 uses default = no latency / no drop)
- Video sender's `seq` counter is reported in `RunStats` so the
  receiver can compute `video_received` and
  `video_loss_ppm = (sent - received) / sent`
- 2-axis matrix: `(latency_ms, drop_ppm)` instead of B7's
  `(input_rate, video_rate)`
- Different config_id format, different CSV columns

Reuses (no library code changes):
- `prdt_transport::InProcTransport`, `LoopbackOptions`, `Transport`
- `prdt_transport::ReceivedMessage::{Input, Video, ...}`
- `prdt_protocol::InputEvent::MouseMove`, `EncodedFrame`
- `prdt_protocol::now_monotonic_us`
- `prdt_latency_bench::percentiles`

## CLI

```
--out-dir <path>                              # required
--latencies-ms 0,1,10,50,200                  # one-way delay, ms
--drops-ppm 0,1000,10000,50000                # per-msg drop ppm
--input-rate-hz 1000                          # FIXED axis
--video-rate-fps 60                           # FIXED axis
--video-frame-bytes 50000                     # synthetic frame size
--duration 5s                                 # per-config bench length
--inter-config-delay-ms 250                   # spacing
--dry-run                                     # list configs and exit
```

Default sweep: 5 latencies × 4 drops = **20 configs**, 5 s each +
250 ms spacing = ~105 s wall time.

`input-rate-hz` and `video-rate-fps` are kept FIXED axes to limit
the matrix. Users can re-run with different rate values to explore
the rate dimension separately.

## Trial flow per config

1. `let opts = LoopbackOptions { drop_ppm: cfg.drop_ppm, latency: Some(Duration::from_millis(cfg.latency_ms)) };`
   (when `latency_ms == 0`, set `latency: None` to skip the
   sleep entirely — saves overhead in the no-delay baseline)
2. `(host_side, viewer_side) = InProcTransport::pair(opts);`
3. Spawn 3 tasks (B7 pattern): input sender, video sender, receiver.
4. Receiver counts both `Input` and `Video` arrivals, drains
   `sent_ts` for input only.
5. Cancel on deadline, drain, join, return `RunStats`.

`RunStats` adds `video_sent` and `video_received`:

```rust
struct RunStats {
    input_sent: u64,
    input_received: u64,
    input_lags: Vec<u64>,
    video_sent: u64,
    video_received: u64,
}
```

## Aggregation

```rust
struct ConfigStats {
    config_id: String,
    latency_ms: u32,
    drop_ppm: u32,
    input_rate_hz: u32,
    video_rate_fps: u32,
    duration_ms: u64,
    input_sent: u64,
    input_received: u64,
    input_loss_ppm: u64,
    input_p50_us: u64,
    input_p95_us: u64,
    input_p99_us: u64,
    video_sent: u64,
    video_received: u64,
    video_loss_ppm: u64,
}
```

- `*_loss_ppm = (sent - received) * 1_000_000 / max(1, sent)` for
  both input and video
- Empty `input_lags` → emit zeros for percentiles

## Output `summary.csv`

15 columns (header byte-exact):

```
config_id,latency_ms,drop_ppm,input_rate_hz,video_rate_fps,duration_ms,input_sent,input_received,input_loss_ppm,input_p50_us,input_p95_us,input_p99_us,video_sent,video_received,video_loss_ppm
```

`config_id` format: `lat{latency_ms}ms-drop{drop_ppm}ppm`, e.g.
`lat0ms-drop0ppm`, `lat200ms-drop50000ppm`. ASCII, filesystem-safe.

## Tests (5 unit)

1. `expand_matrix_cartesian` — latency outer, drop inner; asserts
   2×2 case order
2. `config_id_format_canonical` — verifies `lat0ms-drop0ppm` and
   `lat200ms-drop50000ppm`
3. `aggregate_with_video_loss` — synthetic RunStats with
   non-zero video loss → exact ppm match
4. `aggregate_empty_input_lags_emits_zero_percentiles` — empty
   `input_lags` → 0 for p50/p95/p99 but non-zero counts pass through
5. `summary_csv_writer_emits_header_and_one_row` — exact 15-column
   header + 1 row content via tempfile

Plus optionally a 6th async smoke test
(`run_one_config_with_latency_observes_blocked_throughput`) but
it adds wall-time pressure to the test suite. Decide during plan.

## Error handling

- `send_input` / `send_video` returns `Err(PeerClosed)` only on
  cancel propagation; treat as terminal in sender loops
- Receiver `Err(_)` breaks loop
- Empty `try_recv` on `sent_ts` (race shouldn't happen at drop=0
  but possible at drop>0 because the sender pushes timestamp
  BEFORE the silently-dropped send): in this case the receiver
  has no matching event arrival, so the orphan ts simply stays in
  the queue. Not a correctness bug; just means the lag vector
  has fewer entries than `input_sent`. Document this.

  **Better solution**: don't push timestamp into queue if the
  send was going to be dropped. But the sender doesn't know;
  drop happens inside `send_msg`. So the cleanest design:
  receiver pops one timestamp per arriving Input — orphan
  timestamps from dropped sends accumulate harmlessly in the
  queue. At end of run we discard the queue.

## Progress logging

```
[ 1/20] running lat0ms-drop0ppm duration=5s
[ 1/20] done    lat0ms-drop0ppm input=5000/5000 video=300/300 input_p95_us=8
[ 2/20] running lat0ms-drop1000ppm duration=5s
[ 2/20] done    lat0ms-drop1000ppm input=5000/4995 video=300/300 input_p95_us=10
...
```

## Risks & Notes

- **Latency-induced send blocking** is documented above; users
  should expect `input_sent` to drop as latency increases.
- **High-latency configs eat bench time disproportionately**:
  `latency=200ms` × `input_rate=1000` means sender produces only
  ~5 events per 1s wall, so 5s gives only ~25 events. The receiver
  task runs the full 5s anyway, so wall time is unchanged. p50/p95
  estimates from 25 events are noisy. The default ladder (0, 1, 10,
  50, 200 ms) keeps the measurement informative — at 200 ms users
  get the "yes the latency is detectable" signal but should not
  over-interpret the p99. Document this in `docs/`.
- **Dim explosion**: 5 lat × 4 drop = 20 already pushing 105s. Don't
  add more axes unless the user ups duration aggressively.
- **`drop_ppm` distribution semantics**: per-message Bernoulli with
  shared xorshift state across all sends (sender + receiver share
  the static `STATE` in `loopback.rs`). For 5000 messages × 20
  configs the entropy budget is fine.
- **Recommended profile presets**: documented in
  `docs/net-profile-bench.md` Tier table:
  | Profile | Latency | Drop |
  |---|---|---|
  | localhost | 0 ms | 0 |
  | LAN | 1 ms | 0 |
  | metro | 10 ms | 1000 (0.1%) |
  | WAN | 50 ms | 10_000 (1%) |
  | satellite | 600 ms | 50_000 (5%) |
  | lossy WiFi | 10 ms | 100_000 (10%) |

  Users can pick a subset via `--latencies-ms 1,10,50 --drops-ppm 0`.

## Exit criteria

1. `cargo build --release -p prdt-latency-bench --bin prdt-net-profile-bench` clean
2. `cargo test -p prdt-latency-bench --bin prdt-net-profile-bench` passes (5 new tests)
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
4. **Manual smoke**: `prdt-net-profile-bench --out-dir bench-results/net-profile-smoke/`
   produces `summary.csv` with 20 rows. `lat0ms-drop0ppm` row should
   show p50_us < 100 and `input_loss_ppm == 0`. `lat10ms-drop10000ppm`
   should show p50_us ≈ 10000 and `input_loss_ppm` near 10_000.
5. `docs/net-profile-bench.md` includes usage + schema + sample
   interpretation + profile presets table + clear "what this does
   NOT measure" section
6. Tag `plan4-b4-net-profile-bench-complete` created

## Estimate

- spec (this doc): 0.25 d
- plan: 0.25 d
- implementation + tests (B7 template clone + adjustments): 0.5 d
- total: ~1 d
