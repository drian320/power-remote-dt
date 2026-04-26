# FEC sweep bench (Plan 4 B6)

The `prdt-fec-bench` bin tests the Reed-Solomon FEC algorithm
directly: synthetic frame -> packetize -> per-packet drop ->
FrameAssembler. No transport, no GPU, no async. Sweeps
`(k × m × drop_ppm)` and writes a recovery-rate + reconstruction-
latency CSV.

## Quick start

```bash
# Default 30-config sweep, ~30 s wall time.
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec/

# Custom subset (only k=64, sweep drop rates).
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir bench-results/fec-k64/ \
    --ks 64 --ms 2,6 --drops 0,50000,100000,200000,300000

# Dry-run (list configs).
cargo run --release -p prdt-latency-bench --bin prdt-fec-bench -- \
    --out-dir /tmp/dry --dry-run
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `--out-dir <path>` | (required) | `summary.csv` goes here. |
| `--ks <list>` | `8,32,64` | Data shards per frame. |
| `--ms <list>` | `2,6` | Parity shards per frame. |
| `--drops <ppm>` | `0,10000,50000,100000,200000` | Per-packet drop probability in ppm. |
| `--frame-bytes <N>` | `5000` | Synthetic frame size; must fit in `k * chunk_payload_len`. |
| `--chunk-payload-len <N>` | `1200` | Per-packet payload size (MTU-aware). |
| `--trials <N>` | `1000` | Frames per config. |
| `--seed <u64>` | `4242` | RNG seed (any non-zero). |
| `--dry-run` | off | List configs and exit. |

## summary.csv schema

```
config_id,k,m,drop_ppm,frame_bytes,trials,complete_no_fec,complete_with_fec,lost,recovery_rate_ppm,reconstruct_p50_us,reconstruct_p95_us
```

`config_id` format: `k{K}m{M}-drop{ppm}` (e.g. `k8m2-drop50000`).

- `complete_no_fec`: trials where all source shards arrived; FEC was
  not exercised even if parity packets were also delivered.
- `complete_with_fec`: trials where at least one source shard was
  dropped, but enough total shards (>= k) arrived; FEC reconstructed.
- `lost`: trials where fewer than k shards arrived; unrecoverable.
- `recovery_rate_ppm`: `(complete_no_fec + complete_with_fec) /
  trials` in ppm.
- `reconstruct_p50_us` / `_p95_us`: reconstruction latency over
  `complete_with_fec` trials only. Zero when no trials triggered FEC.

## Sample interpretation

```
k8m2-drop100000,8,2,100000,5000,1000,432,521,47,953000,18,42
```

means: 1000 trials at k=8 m=2 with 10% per-packet drop. 432 trials
arrived clean, 521 needed FEC reconstruction, 47 lost. Overall
953,000 ppm = 95.3% recovery rate. Median FEC reconstruction took
18 µs, p95 was 42 µs.

## Limitations

- **Independent random drop only**: no bursty patterns, no targeted
  attack on parity shards.
- **Uniform frame size**: `--frame-bytes` is fixed per run; codec
  output in real life is bursty (IDR vs P-frame).
- **No latency bench**: the bin measures FEC algorithm overhead,
  not transport latency under FEC. A future bench using
  `CustomUdpTransport` with packet-level drop injection would
  cover that case.
- **Frame-bytes must fit in `k * chunk_payload_len`**: with the
  defaults, the largest frame is `8 * 1200 = 9600` bytes for
  k=8 configs. A frame larger than that would error in `packetize`
  and be reported as `lost`.
- **Reproducibility scope**: `--seed` makes the per-packet drop
  decisions and trial-frame contents bit-identical run-to-run, so
  the count columns (`complete_no_fec`, `complete_with_fec`,
  `lost`, `recovery_rate_ppm`) are reproducible. The
  `reconstruct_p50_us` / `reconstruct_p95_us` columns are
  `Instant::now()` wall-clock measurements and will jitter run-to-run
  by tens of microseconds even with the same seed.
