# Plan 4 B6 FEC Sweep Bench — Design Spec

**Date:** 2026-04-26
**Tag (on completion):** `plan4-b6-fec-bench-complete`
**Scope:** Pure-CPU FEC algorithm benchmark — measure how `(k, m, drop_ppm)` affect frame recovery rate and reconstruction overhead.

## Goal

Add a `prdt-fec-bench` bin that exercises the existing `prdt_transport`
FEC + packetize + assembler stack with controlled per-packet drop
simulation, then reports per-config recovery rate and reconstruction
latency. Sweeps `k × m × drop_ppm` and writes a CSV summary alongside
the existing bench-matrix outputs.

## Why a separate bin (vs. extending bench-matrix)

`InProcTransport` (used by `prdt-bench-matrix`) bypasses FEC entirely —
it ships full messages over a tokio `mpsc` channel and only supports
whole-frame drop simulation. Extending it to run frames through
packetize/assemble adds complexity without benefit, since FEC
effectiveness is a property of the algorithm + drop pattern, not of
the GPU pipeline. The `fec-bench` bin tests the algorithm directly:
no GPU, no async, no transport, no encryption. ~30 s for the default
30-config sweep.

## Non-goals

- Latency under FEC at runtime (covered by future B-class bench using
  real `CustomUdpTransport` with `tc`-style drop injection)
- Network jitter / reorder / duplicate packets (only random independent
  drop is simulated; bursty patterns are out of scope)
- Drop adversary models beyond uniform random (no targeted attack on
  parity shards)
- AV1 / different codec — frame is a synthetic byte buffer, codec
  agnostic
- Variable per-frame size (single `--frame-bytes` axis, not a
  distribution)

## Architecture

```
crates/latency-bench/
  src/
    bin/
      fec-bench.rs           ← new prdt-fec-bench bin
                              CLI + matrix expand + trial loop +
                              CSV writer
    lib.rs                   (no changes; re-exports stay as-is)
docs/
  fec-bench.md               ← new usage guide
```

The new bin is self-contained: it depends on `prdt-transport`'s public
`FecCodec`, `packetize`, `FrameAssembler`, plus `prdt-protocol` types.
It does not call into `full_pipeline` or any GPU code.

### Public API used (existing)

- `prdt_transport::FecCodec::new(k, m) -> Result<FecCodec>`
- `prdt_transport::FecCodec::reconstruct(shards) -> Result<Vec<u8>>`
- `prdt_transport::packetize::packetize(frame: &EncodedFrame, fec: &FecCodec, chunk_payload_len: usize) -> Result<Vec<VideoPacket>>`
- `prdt_transport::assembler::FrameAssembler::new(...)`
- `prdt_transport::assembler::FrameAssembler::feed(pkt, fec) -> Result<FeedResult>`
- `prdt_transport::DEFAULT_K = 8`, `DEFAULT_M = 2`

The exact shape of these is verified during plan-writing; if the
public API differs, the plan is the place to adapt.

## CLI

```
--out-dir <path>                              # required
--ks 8,32,64                                  # data shards (k)
--ms 2,6                                      # parity shards (m)
--drops 0,10000,50000,100000,200000           # per-packet drop ppm (0%, 1%, 5%, 10%, 20%)
--frame-bytes 60000                           # synthetic frame size (default ~60KB)
--chunk-payload-len 1200                      # MTU-friendly default
--trials 1000                                 # frames per config (default 1000)
--seed 4242                                   # rng seed (default 4242 for reproducibility)
--dry-run                                     # list configs only
```

Default sweep: 3 × 2 × 5 = **30 configs**, 1000 trials each = 30,000
synthetic frames. Estimated wall time: ~30 s on a modern x86 box (FEC
reconstruction is ~10-100 µs per frame at k=64, packetize is ~20 µs).

## Trial loop

For each config `(k, m, drop_ppm)`:

1. `let fec = FecCodec::new(k, m)?;`
2. For `trial_idx in 0..trials`:
   1. Build a synthetic `EncodedFrame` of `frame_bytes` bytes (filled
      with deterministic-but-varied content based on `seed + trial_idx`
      so packetize doesn't accidentally collapse identical chunks).
   2. `let packets = packetize(&frame, &fec, chunk_payload_len)?;` —
      yields `k + m` packets per frame.
   3. For each packet: roll `drop_ppm / 1_000_000` per-packet to
      decide drop. RNG seeded with `seed + trial_idx + packet_idx`
      so each trial is reproducible.
   4. Feed surviving packets to a fresh `FrameAssembler`. Time the
      feed sequence (the elapsed time covers reconstruction when FEC
      kicks in).
   5. Classify outcome:
      - `Complete(f)` returned without FEC having been triggered
        (`packet_count >= k`, all from the first k indices) →
        `complete_no_fec`
      - `Complete(f)` returned but FEC was triggered (assembler had
        to call `fec.reconstruct`) → `complete_with_fec`
      - Never returned `Complete` (insufficient packets received) →
        `lost`
   6. Record `reconstruct_us` only for `complete_with_fec` outcomes.
3. Aggregate after all trials.

### Distinguishing "fec triggered" vs "no fec"

The current `FrameAssembler::feed` doesn't expose whether FEC was used.
Two viable approaches:

- **A. Inspect packet indices**: if every received packet has index
  `< k`, no FEC is needed. If at least one received packet has index
  `>= k` (parity), FEC is required.
- **B. Wrap the assembler internally**: track which call to `feed`
  produced the `Complete` and whether at that point all-data-shards
  were present.

Approach A is simpler and correct. Use it.

## Aggregation

Per config, accumulate:

- `complete_no_fec: u64`
- `complete_with_fec: u64`
- `lost: u64`
- `reconstruct_times_us: Vec<u64>` (only complete_with_fec)

Compute:

- `recovery_rate_ppm = (complete_no_fec + complete_with_fec) * 1_000_000 / trials`
- `reconstruct_p50_us`, `reconstruct_p95_us` via existing
  `prdt_latency_bench::percentiles`. If `complete_with_fec == 0`,
  emit zeros.

## Output: `summary.csv`

```
config_id,k,m,drop_ppm,frame_bytes,trials,complete_no_fec,complete_with_fec,lost,recovery_rate_ppm,reconstruct_p50_us,reconstruct_p95_us
```

`config_id` format: `k{K}m{M}-drop{ppm}`, e.g. `k8m2-drop0`,
`k64m6-drop200000`. ASCII, filesystem-safe.

No per-trial CSV (3000 rows × 30 configs = 90k rows of low-information
data; if needed for debugging, add `--write-trials` later).

## Test strategy

5 unit tests in `fec-bench.rs`'s `mod tests`:

1. `expand_fec_matrix` produces correct cartesian product (count + order)
2. `config_id` format
3. `simulate_one_trial(k=4, m=2, drop_ppm=0, ..)` always returns
   `complete_no_fec`
4. `simulate_one_trial(k=4, m=2, drop_ppm=1_000_000, ..)` (100% drop)
   always returns `lost`
5. CSV writer header + 1 row content (use `tempfile::tempdir`)

No external test deps beyond `tempfile` (already a dev-dep).

## Error handling

- `FecCodec::new(k, m)` fails (invalid params): emit a skip row with
  `recovery_rate_ppm = 0` and reconstructs = 0, log a warning, continue
  to next config. Skip rows can be detected by `complete_no_fec + complete_with_fec + lost == 0`.
- `packetize` fails (e.g. frame too large for k): emit skip row,
  log warning.
- Out-of-disk: propagate as an `anyhow::Error` from the bin's main —
  there's nothing useful to do mid-sweep.

## Progress logging

Use `tracing_subscriber::fmt::init()` and `info!`:

```
[ 1/30] running k8m2-drop0 trials=1000
[ 1/30] done    k8m2-drop0 recovery=1000000ppm reconstruct_p50_us=0
[ 2/30] running k8m2-drop10000 trials=1000
[ 2/30] done    k8m2-drop10000 recovery=999700ppm reconstruct_p50_us=18
...
```

## Risks & Notes

- **Synthetic frame content matters for packetize**: if the bytes
  collapse into trivially-recoverable shards (e.g. all-zero), the
  test is uninformative. Fill with `xor` of `seed + trial_idx` and
  byte index to ensure distinct shards.
- **`packetize` upper bound**: if `frame_bytes > k * chunk_payload_len`,
  packetize will reject. The default 60_000 fits within k=8 (8 *
  1200 = 9600 — too small). **Default frame_bytes vs default k=8 is
  inconsistent**. Resolution: default `frame_bytes = 8000` (within
  k=8 capacity); test plan adjusts `frame_bytes` proportionally to
  `k` if a config would exceed it (or just skip that config).
  Practical fix: default `frame_bytes = 5000` so all defaults fit
  within k=8 (~6 chunks × 1200B). Larger frames hit only at higher k.
- **Reproducibility**: all RNG decisions must derive from the seed.
  Use `rand_xoshiro` or just `rand` `StdRng::seed_from_u64`. Existing
  workspace uses `rand_core`; a small `XorShiftRng` rolled inline is
  fine and avoids new deps.
- **Trial cost**: ~30,000 packetize + drop + reassemble calls. At
  ~30 µs/trial with k=8, that's 900 ms total. At k=64 with
  reconstruction, ~100 µs/trial = 3 s. Total well under 30 s.

## Exit criteria

1. `cargo build --release -p prdt-latency-bench --bin prdt-fec-bench` clean
2. `cargo test -p prdt-latency-bench` passes (existing 8 + 5 new = 13
   tests minimum at lib level + main bin)
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
4. **Manual smoke**: `prdt-fec-bench --out-dir bench-results/fec-smoke/`
   produces `summary.csv` with 30 rows, all rows with non-zero
   trial counts (no skip rows for the default axes)
5. `docs/fec-bench.md` includes usage + sample output interpretation
6. tag `plan4-b6-fec-bench-complete` created

## Estimate

- spec: 0.25 d (this doc)
- plan: 0.25 d
- implementation + tests: 0.5 d
- total: ~1 d

Smaller than B1 because no GPU code, no library refactor, single bin,
re-uses fully existing transport API.
