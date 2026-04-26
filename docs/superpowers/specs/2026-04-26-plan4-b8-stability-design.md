# Plan 4 B8 Stability Bench — Design Spec

**Date:** 2026-04-26
**Tag (on completion):** `plan4-b8-stability-bench-complete`
**Scope:** Long-running stability check via existing `prdt-bench-matrix` + a new Python time-series analysis script.

## Goal

Detect drift / leaks / regressions across a 30-minute continuous
NVENC + NVDEC bench run by post-processing the per-frame CSV
that `prdt-bench-matrix` already produces.

## Why no new Rust bin

`prdt-bench-matrix` (Plan 4 B1) already supports `--duration 30m`
on a single config. Its per-frame CSV (`per-frame/<config_id>.csv`)
records every frame's `capture_us`, `recv_us`, `decode_done_us`.
That's enough raw data to bucket by minute and surface drift —
just not in CSV form yet.

Adding a new bin would duplicate the bench-matrix pipeline. A
Python analysis script reads the existing per-frame CSV and emits
the time-series view.

## Non-goals

- Transport-only stability (could be done with InProcTransport but
  no value: real production pipeline is GPU-driven)
- Multi-config matrix sweep (single config — sweep would take >10h
  and isn't the question)
- Memory profiling beyond what surfaces in the metrics
  (heaptrack / valgrind work is out-of-scope)
- Automated CI check (manual 30-min run, results documented)
- 2-machine LAN stability (different bench, would need B4 follow-on)

## Architecture

```
scripts/
  analyze-stability.py    ← new: per-frame CSV -> minute-bucketed
                            time-series CSV + summary stats
docs/
  stability-bench.md      ← new: usage + sample interpretation
                            + actual 30-min results
bench-results/
  stability-30m/          ← (manual run output, not committed)
    per-frame/1080p60-30mbps-nvdec.csv  (existing format from B1)
    summary.csv                          (existing format from B1)
    minute-buckets.csv                   (new, written by analyze-stability.py)
```

The Python script is a sibling to `scripts/analyze-bench-matrix.py`
(B1 era). Reads the same per-frame CSV format, emits one row per
minute bucket.

## Reference: B1 per-frame CSV schema

```
seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us
```

The bench's `now_monotonic_us` clock starts ~0 at process start.
Bucketing by `capture_us / 60_000_000` gives minute index.

## CLI (`analyze-stability.py`)

```
usage: analyze-stability.py <per-frame.csv> [--bucket-seconds N] [--out <minute-buckets.csv>]
```

- `<per-frame.csv>`: required positional argument
- `--bucket-seconds`: bucket width in seconds (default 60)
- `--out`: output CSV path (default: `<input>.buckets.csv`)

## Output: `minute-buckets.csv`

```
bucket_idx,bucket_start_s,frames_in_bucket,arrival_p50_us,arrival_p95_us,arrival_p99_us,decode_p50_us,decode_p95_us,decode_p99_us,e2e_p50_us,e2e_p95_us,e2e_p99_us
```

One row per `--bucket-seconds` window. `bucket_start_s` is the
elapsed seconds since first frame. `frames_in_bucket` exposes
loss/throughput drift directly (1800 expected at 60fps × 30s
bucket; less means frames missed within that window).

## Drift detection (analysis output to stdout)

Script also prints:
- Slope of `e2e_p50_us` over time(linear regression, µs / minute)
- Max - min `e2e_p50_us` across buckets
- Total frames vs expected (loss%)
- Frames-per-bucket variance across buckets
- Flag if any bucket has >2× the median `e2e_p99_us` (outlier signal)

## Manual run procedure

```bash
# 1. Run bench-matrix for 30 min on a single config.
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/stability-30m/ \
    --resolutions 1080 \
    --bitrates 30 \
    --decoders nvdec \
    --fps 60 \
    --duration 30m

# 2. Analyze.
python scripts/analyze-stability.py \
    bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv

# 3. Document findings in docs/stability-bench.md.
```

## Tests

Unit tests for the Python script:

1. **`test_buckets_by_capture_us`**: input 100 frames spanning
   180_000_000 µs (3 minutes) at varied `capture_us`; assert 3
   buckets with correct `frames_in_bucket` distribution
2. **`test_percentile_round_picking`**: bucket of 100 frames with
   known lag distribution → assert p50/p95/p99 match expected
   (compatible with Rust `percentiles` round-style)
3. **`test_empty_csv_emits_zero_rows`**: empty input produces
   only the header in output
4. **`test_drift_slope_zero_for_constant_lag`**: constant e2e
   over all buckets → slope ≈ 0
5. **`test_drift_slope_nonzero_for_increasing_lag`**: monotonically
   increasing lag → positive slope

Run via `pytest scripts/analyze-stability.py` (use `unittest`
since we already have pandas).

## Error handling

- Missing input file → script exits 2 with helpful message
- Malformed CSV (missing columns) → exit 2 with the missing
  column list
- Empty data file → header-only output, slope=0, "no frames"
  printed to stdout

## Exit criteria

1. `scripts/analyze-stability.py` created, 5 unit tests pass via
   `pytest scripts/`
2. Manual 30-minute `prdt-bench-matrix` run produces a per-frame
   CSV and the analysis script processes it without error
3. `docs/stability-bench.md` includes:
   - Usage
   - Schema
   - Real 30-min run results (slope, max-min, loss, outliers)
   - "What it does NOT measure" section
4. STATUS.md updated
5. tag `plan4-b8-stability-bench-complete` created

## Estimate

- spec (this doc): 0.1 d
- plan: 0.15 d
- implement script + tests: 0.2 d
- 30-min manual run + docs: 0.05 d
- total: ~0.5 d

The 30-minute bench run is wall-clock 30 min; not implementer time.
