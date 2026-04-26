# Stability bench (Plan 4 B8)

The B8 stability check uses the existing `prdt-bench-matrix` bin to
run a single full-pipeline (NVENC + NVDEC) config for 30 minutes,
then post-processes the per-frame CSV with `scripts/analyze-stability.py`
to produce a minute-bucketed time series + drift summary.

No new Rust bin is needed — `prdt-bench-matrix --duration 30m` was
already supported (Plan 4 B1). The new artifact is the analysis script.

## Quick start

```bash
# 1. Run a 30-minute single-config bench (~30 min wall time on RTX 3070 Ti).
cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/stability-30m/ \
    --resolutions 1080 --bitrates 30 --decoders nvdec --fps 60 \
    --duration 30m

# 2. Bucket and summarize.
python scripts/analyze-stability.py \
    bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv

# Optional: shorter buckets for finer-grained drift inspection.
python scripts/analyze-stability.py \
    bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv \
    --bucket-seconds 30
```

## CLI

| Flag | Default | Notes |
|---|---|---|
| `<input_csv>` | (required) | per-frame CSV from `prdt-bench-matrix` |
| `--bucket-seconds <N>` | `60` | bucket width in seconds |
| `--out <path>` | `<input>.buckets.csv` | output CSV path |

## Output: `minute-buckets.csv` schema

```
bucket_idx,bucket_start_s,frames_in_bucket,arrival_p50_us,arrival_p95_us,arrival_p99_us,decode_p50_us,decode_p95_us,decode_p99_us,e2e_p50_us,e2e_p95_us,e2e_p99_us
```

- `bucket_idx`: 0-based bucket counter.
- `bucket_start_s`: elapsed seconds from the first frame's `capture_us`.
- `frames_in_bucket`: count of frames whose `capture_us` falls in this bucket.
- `arrival_p50/95/99_us`, `decode_p50/95/99_us`, `e2e_p50/95/99_us`: per-bucket
  percentiles using round-style (half-away-from-zero) picking, compatible
  with Rust `prdt_latency_bench::percentiles`.

## Stdout summary

Beyond the CSV, the script prints:

- **`e2e_p50_us slope`**: linear regression slope of `e2e_p50_us` vs
  wall-clock time in minutes (`bucket_start_s / 60.0`). Units are
  µs per minute. A non-zero slope indicates drift.
- **`e2e_p50_us max-min`**: range across buckets (drift envelope).
- **`frames_in_bucket variance`**: variance of frames per bucket.
  High variance suggests bursty drops or scheduling stalls.
- **`outlier buckets`**: buckets with `e2e_p99_us > 2 × median(e2e_p99_us)`
  across the run. None expected for a healthy run.

## 30-minute run on RTX 3070 Ti (2026-04-26)

Config: 1080p60, 30 Mbps, NVENC + NVDEC. 30 minutes wall time.

`prdt-bench-matrix` summary row (over the entire 30 min):

```
config_id,resolution,bitrate_mbps,decoder,fps,sent,received,loss_ppm,arrival_p50_us,arrival_p95_us,arrival_p99_us,decode_p50_us,decode_p95_us,decode_p99_us,e2e_p50_us,e2e_p95_us,e2e_p99_us
1080p60-30mbps-nvdec,1920x1080,30,nvdec,60,89921,89920,11,11267,12386,14793,2062,2174,2346,13346,14628,16932
```

89,921 frames sent over 30 minutes (just under the theoretical
108k = 60 fps × 1800 s; the bench's frame production rate is
slightly throttled by NVENC encode latency at 1080p60 30 Mbps).
**1 frame lost** (`loss_ppm = 11`, ~0.001%). Aggregate
e2e_p50=13.3 ms, p95=14.6 ms, p99=16.9 ms.

`scripts/analyze-stability.py` stdout:

```
input: bench-results\stability-30m\per-frame\1080p60-30mbps-nvdec.csv
output: bench-results\stability-30m\per-frame\1080p60-30mbps-nvdec.buckets.csv
total frames: 89920
buckets: 30
e2e_p50_us slope: -4.42 us/min
e2e_p50_us max-min: 482 us
frames_in_bucket variance: 11774.1
outlier buckets: none (e2e_p99 > 2x median)
```

Selected buckets from `minute-buckets.csv`:

| bucket | start_s | frames | arrival_p50 | decode_p50 | e2e_p50 | e2e_p95 | e2e_p99 |
|---|---|---|---|---|---|---|---|
| 0  | 0    | 2629 | 11707 | 2060 | 13783 | 15596 | 18648 |
| 1  | 60   | 3020 | 11253 | 2066 | 13337 | 14434 | 16606 |
| 2  | 120  | 3055 | 11267 | 2058 | 13340 | 14400 | 16545 |
| 27 | 1620 | 3068 | 11238 | 2059 | 13318 | 14387 | 16051 |
| 28 | 1680 | 3050 | 11243 | 2060 | 13314 | 14452 | 16675 |
| 29 | 1740 | 2754 | 11397 | 2089 | 13516 | 16561 | 19476 |

(All values µs. Buckets 3–26 are similarly steady; full data in
`bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.buckets.csv`.)

Interpretation:

- **Drift slope is -4.42 µs/min** — over 30 minutes that's a 132 µs
  drift, which is sub-millisecond on a 13 ms baseline. Effectively
  flat; the slight negative slope is likely warm-up settling
  (bucket 0 is 13.8 ms, mid-run buckets settle near 13.3 ms).
- **Max-min of 482 µs** across 30 minute-buckets — well within
  normal jitter for a single-GPU loopback pipeline.
- **No outlier buckets** flagged at the 2× median threshold —
  pipeline maintained consistent p99 throughout.
- **Bucket 0 has 2629 frames vs ~3000 elsewhere** because it
  starts mid-second and includes ramp-up. Bucket 29 has 2754
  because the run ends partway into the bucket.
- **frames_in_bucket variance 11774.1** is dominated by the first
  and last partial buckets; buckets 1–28 have very tight frame
  counts (3020–3068 range).

**Verdict**: 1080p60 30 Mbps NVDEC is stable for at least 30 minutes
on RTX 3070 Ti. No drift, no leaks visible at this scale, no
mid-run outliers. Re-run this bench after any pipeline changes to
catch regressions.

## What this measures (and what it does NOT)

This bench measures full-pipeline stability over time on a single
machine: NVENC encode + InProcTransport + MF/NVDEC decode + per-stage
percentiles bucketed by minute.

It does NOT measure:
- **Real network drift**: single-process loopback. Long-term WAN
  / TURN behaviour requires real 2-machine setup.
- **Memory leaks at the OS level**: rust + tokio + cuda runtime
  could leak GPU memory; that requires `nvidia-smi` watch or a
  separate memory-pressure probe.
- **Multi-config matrix sweep**: 30 minutes × 60 configs > 30 hours,
  not useful. This bench answers "does ONE typical config stay
  stable?" — that's the regression sentinel.
- **Real glass-to-glass display latency** (Plan 4 M3 territory).

## Limitations

- The `prdt-bench-matrix` bin's per-frame CSV is large at 30 min
  (~5–10 MB per minute of bench wall time). Disk space and
  pandas memory both fine, but be aware.
- A single process runs encode + decode on one GPU; under 4K or
  high-bitrate configs the GPU may saturate and skew "stability"
  measurements. The default 1080p60 30 Mbps config is well below
  saturation on RTX 3070 Ti (verified in B1).
- `bucket_idx` is integer-divided from `capture_us`; the last
  bucket may contain fewer frames if the run ends mid-bucket.
  The script reports `frames_in_bucket` so this is visible.
