# Plan 4 B8 Stability Bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Python time-series analysis script (`scripts/analyze-stability.py`) that buckets `prdt-bench-matrix` per-frame CSV by minute, plus a 30-minute manual run procedure documented in `docs/stability-bench.md`.

**Architecture:** Pure Python 3.12 + pandas + numpy script that mirrors `scripts/analyze-bench-matrix.py` style. No new Rust code — leverages existing `prdt-bench-matrix --duration 30m` for raw data collection. Produces `minute-buckets.csv` with one row per time bucket, plus drift-detection summary printed to stdout.

**Tech Stack:** Python 3.12, pandas (already installed during B1), numpy (pandas dep), unittest stdlib for tests.

**Spec:** `docs/superpowers/specs/2026-04-26-plan4-b8-stability-design.md`

---

## File Structure

**Created files:**

```
scripts/
  analyze-stability.py            new: time-bucketed analysis with embedded
                                   unittest module (pytest-compatible)
docs/
  stability-bench.md              new: usage + schema + 30-min run results
                                   + interpretation
```

**Modified files:**

```
docs/superpowers/STATUS.md        update tag list + B8 row + 残タスク closure
```

**Manual run output (NOT committed):**

```
bench-results/stability-30m/
  per-frame/1080p60-30mbps-nvdec.csv    (existing format, ~108k rows)
  summary.csv                            (existing format, 1 row)
  minute-buckets.csv                     (new, 30 rows + header)
```

---

## Reference: B1 per-frame CSV columns (verified at `crates/latency-bench/src/lib.rs:184-202`)

```
seq,capture_us,encode_done_us,recv_us,decode_done_us,arrival_lag_us,decode_lag_us,e2e_lag_us
```

The Rust writer emits these in this order. The Python script reads
the same column names via `pd.read_csv` + dict access — no
positional dependency.

`capture_us` is from `prdt_protocol::now_monotonic_us()` which
starts ~0 at process start. Bucketing by
`capture_us / (bucket_seconds * 1_000_000)` gives the bucket index.

---

## Task 1: `analyze-stability.py` core (loader, bucketing, percentiles)

**Files:**
- Create: `scripts/analyze-stability.py`

- [ ] **Step 1: Write the failing tests**

Create `scripts/analyze-stability.py`:

```python
"""Plan 4 B8 — bucket a `prdt-bench-matrix` per-frame CSV by minute and
emit per-bucket percentile stats + drift detection.

Usage:
    python scripts/analyze-stability.py <per-frame.csv> \
        [--bucket-seconds N] [--out <minute-buckets.csv>]
"""

from __future__ import annotations

import argparse
import sys
import unittest
from pathlib import Path

import numpy as np
import pandas as pd

REQUIRED_COLUMNS = [
    "capture_us",
    "arrival_lag_us",
    "decode_lag_us",
    "e2e_lag_us",
]

OUTPUT_COLUMNS = [
    "bucket_idx",
    "bucket_start_s",
    "frames_in_bucket",
    "arrival_p50_us",
    "arrival_p95_us",
    "arrival_p99_us",
    "decode_p50_us",
    "decode_p95_us",
    "decode_p99_us",
    "e2e_p50_us",
    "e2e_p95_us",
    "e2e_p99_us",
]


def percentile_round(values: np.ndarray, p: float) -> int:
    """Round-style percentile picking, compatible with Rust's
    `prdt_latency_bench::percentiles`. Returns the value at index
    `round((n - 1) * p)` from the sorted input."""
    if values.size == 0:
        return 0
    sorted_values = np.sort(values)
    idx = int(round((sorted_values.size - 1) * p))
    return int(sorted_values[idx])


def bucket_frames(frame_df: pd.DataFrame, bucket_seconds: int) -> pd.DataFrame:
    """Bucket frames by `capture_us` and emit per-bucket percentile rows.

    Each bucket spans `bucket_seconds` seconds of `capture_us`.
    The first frame's `capture_us` defines bucket 0's start.
    """
    if frame_df.empty:
        return pd.DataFrame(columns=OUTPUT_COLUMNS)

    missing = [c for c in REQUIRED_COLUMNS if c not in frame_df.columns]
    if missing:
        raise ValueError(f"missing columns in input CSV: {missing}")

    bucket_us = bucket_seconds * 1_000_000
    first_capture_us = int(frame_df["capture_us"].min())
    bucket_idx = (
        (frame_df["capture_us"] - first_capture_us) // bucket_us
    ).astype(int)

    rows = []
    for idx, group in frame_df.groupby(bucket_idx):
        arrival = group["arrival_lag_us"].to_numpy()
        decode = group["decode_lag_us"].to_numpy()
        e2e = group["e2e_lag_us"].to_numpy()
        rows.append(
            {
                "bucket_idx": int(idx),
                "bucket_start_s": int(idx) * bucket_seconds,
                "frames_in_bucket": int(len(group)),
                "arrival_p50_us": percentile_round(arrival, 0.50),
                "arrival_p95_us": percentile_round(arrival, 0.95),
                "arrival_p99_us": percentile_round(arrival, 0.99),
                "decode_p50_us": percentile_round(decode, 0.50),
                "decode_p95_us": percentile_round(decode, 0.95),
                "decode_p99_us": percentile_round(decode, 0.99),
                "e2e_p50_us": percentile_round(e2e, 0.50),
                "e2e_p95_us": percentile_round(e2e, 0.95),
                "e2e_p99_us": percentile_round(e2e, 0.99),
            }
        )
    return pd.DataFrame(rows, columns=OUTPUT_COLUMNS)


# === unittest module (run via `python -m unittest scripts.analyze_stability`
# or `pytest scripts/analyze-stability.py`) ===


class TestPercentileRound(unittest.TestCase):
    def test_percentile_round_picking(self):
        # 1..=100 — same shape as Rust tests
        v = np.array(range(1, 101), dtype=np.uint64)
        # p50: round((100-1) * 0.5) = round(49.5) = 50 -> v[50] = 51
        self.assertEqual(percentile_round(v, 0.50), 51)
        # p95: round(99 * 0.95) = round(94.05) = 94 -> v[94] = 95
        self.assertEqual(percentile_round(v, 0.95), 95)
        # p99: round(99 * 0.99) = round(98.01) = 98 -> v[98] = 99
        self.assertEqual(percentile_round(v, 0.99), 99)

    def test_percentile_round_empty(self):
        self.assertEqual(percentile_round(np.array([], dtype=np.uint64), 0.5), 0)


class TestBucketFrames(unittest.TestCase):
    def test_buckets_by_capture_us(self):
        # 100 frames spanning 180 s of capture_us in bucket size 60 s.
        # Frame i has capture_us = i * 1_800_000 (1.8 s apart) so we
        # expect 100 frames distributed across 3 buckets:
        #   bucket 0: frames at 0, 1.8s, 3.6s, ..., 58.2s (33 frames)
        #   bucket 1: frames at 60s, 61.8s, ..., 118.2s (34 frames)
        #   bucket 2: frames at 120s, 121.8s, ..., 178.2s (33 frames)
        frames = pd.DataFrame(
            {
                "seq": list(range(100)),
                "capture_us": [i * 1_800_000 for i in range(100)],
                "encode_done_us": [i * 1_800_000 + 100 for i in range(100)],
                "recv_us": [i * 1_800_000 + 200 for i in range(100)],
                "decode_done_us": [i * 1_800_000 + 300 for i in range(100)],
                "arrival_lag_us": [200] * 100,
                "decode_lag_us": [100] * 100,
                "e2e_lag_us": [300] * 100,
            }
        )
        out = bucket_frames(frames, bucket_seconds=60)
        self.assertEqual(len(out), 3)
        # Check bucket sizes
        self.assertEqual(int(out.loc[0, "frames_in_bucket"]), 34)
        self.assertEqual(int(out.loc[1, "frames_in_bucket"]), 33)
        self.assertEqual(int(out.loc[2, "frames_in_bucket"]), 33)
        # Bucket starts
        self.assertEqual(int(out.loc[0, "bucket_start_s"]), 0)
        self.assertEqual(int(out.loc[1, "bucket_start_s"]), 60)
        self.assertEqual(int(out.loc[2, "bucket_start_s"]), 120)
        # All e2e percentiles are 300 (constant)
        self.assertTrue((out["e2e_p50_us"] == 300).all())
        self.assertTrue((out["e2e_p95_us"] == 300).all())
        self.assertTrue((out["e2e_p99_us"] == 300).all())

    def test_empty_frame_returns_empty_buckets(self):
        empty = pd.DataFrame(columns=REQUIRED_COLUMNS)
        out = bucket_frames(empty, bucket_seconds=60)
        self.assertEqual(len(out), 0)
        self.assertEqual(list(out.columns), OUTPUT_COLUMNS)

    def test_missing_required_column_raises(self):
        # Drop arrival_lag_us
        frames = pd.DataFrame(
            {
                "capture_us": [0],
                "decode_lag_us": [10],
                "e2e_lag_us": [20],
            }
        )
        with self.assertRaises(ValueError) as cm:
            bucket_frames(frames, bucket_seconds=60)
        self.assertIn("arrival_lag_us", str(cm.exception))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--unittest":
        sys.argv.pop(1)
        unittest.main()
    else:
        # The CLI main() is wired in Task 3; this stub is replaced.
        print("Use --unittest for tests; CLI not yet implemented (Task 3)", file=sys.stderr)
        sys.exit(2)
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cd /e/project/rust-desktop/power-remote-dt
python scripts/analyze-stability.py --unittest 2>&1 | tail -10
```

Expected: 5 tests pass — `test_percentile_round_picking`,
`test_percentile_round_empty`, `test_buckets_by_capture_us`,
`test_empty_frame_returns_empty_buckets`,
`test_missing_required_column_raises`.

If tests fail because `pandas` / `numpy` not installed, install:

```bash
pip install --quiet pandas
```

(numpy is a pandas dep so installs together.)

- [ ] **Step 3: Commit**

```bash
git add scripts/analyze-stability.py
git commit -m "scripts: analyze-stability.py core (percentile_round + bucket_frames + 5 tests)"
```

---

## Task 2: Drift detection helpers

**Files:**
- Modify: `scripts/analyze-stability.py`

- [ ] **Step 1: Write the failing tests**

Append to the test classes section of `scripts/analyze-stability.py`,
just before `if __name__ == "__main__":`:

```python
class TestDriftDetection(unittest.TestCase):
    def test_drift_slope_zero_for_constant_lag(self):
        # 5 buckets all with e2e_p50=300
        buckets = pd.DataFrame(
            {
                "bucket_idx": [0, 1, 2, 3, 4],
                "bucket_start_s": [0, 60, 120, 180, 240],
                "frames_in_bucket": [60] * 5,
                "arrival_p50_us": [200] * 5,
                "arrival_p95_us": [200] * 5,
                "arrival_p99_us": [200] * 5,
                "decode_p50_us": [100] * 5,
                "decode_p95_us": [100] * 5,
                "decode_p99_us": [100] * 5,
                "e2e_p50_us": [300] * 5,
                "e2e_p95_us": [300] * 5,
                "e2e_p99_us": [300] * 5,
            }
        )
        slope = e2e_p50_slope_us_per_minute(buckets)
        self.assertEqual(slope, 0.0)

    def test_drift_slope_positive_for_monotonic_increase(self):
        buckets = pd.DataFrame(
            {
                "bucket_idx": list(range(5)),
                "bucket_start_s": [0, 60, 120, 180, 240],
                "frames_in_bucket": [60] * 5,
                "arrival_p50_us": [200] * 5,
                "arrival_p95_us": [200] * 5,
                "arrival_p99_us": [200] * 5,
                "decode_p50_us": [100] * 5,
                "decode_p95_us": [100] * 5,
                "decode_p99_us": [100] * 5,
                "e2e_p50_us": [300, 350, 400, 450, 500],
                "e2e_p95_us": [400] * 5,
                "e2e_p99_us": [500] * 5,
            }
        )
        slope = e2e_p50_slope_us_per_minute(buckets)
        # 200 us increase per 4 buckets = 50 us per bucket = 50 us/minute
        self.assertEqual(slope, 50.0)

    def test_outlier_buckets_flags_high_p99(self):
        buckets = pd.DataFrame(
            {
                "bucket_idx": [0, 1, 2, 3, 4],
                "bucket_start_s": [0, 60, 120, 180, 240],
                "frames_in_bucket": [60] * 5,
                "arrival_p50_us": [200] * 5,
                "arrival_p95_us": [200] * 5,
                "arrival_p99_us": [200] * 5,
                "decode_p50_us": [100] * 5,
                "decode_p95_us": [100] * 5,
                "decode_p99_us": [100] * 5,
                "e2e_p50_us": [300] * 5,
                "e2e_p95_us": [400] * 5,
                "e2e_p99_us": [500, 510, 1500, 520, 530],  # bucket 2 outlier
            }
        )
        outliers = outlier_buckets(buckets, threshold_factor=2.0)
        self.assertEqual(list(outliers["bucket_idx"]), [2])

    def test_outlier_buckets_empty_when_all_within_threshold(self):
        buckets = pd.DataFrame(
            {
                "bucket_idx": [0, 1, 2],
                "bucket_start_s": [0, 60, 120],
                "frames_in_bucket": [60] * 3,
                "arrival_p50_us": [200] * 3,
                "arrival_p95_us": [200] * 3,
                "arrival_p99_us": [200] * 3,
                "decode_p50_us": [100] * 3,
                "decode_p95_us": [100] * 3,
                "decode_p99_us": [100] * 3,
                "e2e_p50_us": [300] * 3,
                "e2e_p95_us": [400] * 3,
                "e2e_p99_us": [500, 510, 520],
            }
        )
        outliers = outlier_buckets(buckets, threshold_factor=2.0)
        self.assertEqual(len(outliers), 0)
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
python scripts/analyze-stability.py --unittest 2>&1 | tail -15
```

Expected: NameError / AttributeError on `e2e_p50_slope_us_per_minute`
and `outlier_buckets` (functions not yet defined).

- [ ] **Step 3: Implement the helpers**

In `scripts/analyze-stability.py`, after the `bucket_frames` function
(before the `# === unittest module ===` comment), add:

```python
def e2e_p50_slope_us_per_minute(buckets: pd.DataFrame) -> float:
    """Linear regression slope of e2e_p50_us across buckets, in
    µs per minute (assuming 60 s buckets). Returns 0.0 for ≤1 row."""
    n = len(buckets)
    if n <= 1:
        return 0.0
    x = np.array(buckets["bucket_idx"], dtype=np.float64)
    y = np.array(buckets["e2e_p50_us"], dtype=np.float64)
    slope, _intercept = np.polyfit(x, y, 1)
    return float(slope)


def outlier_buckets(
    buckets: pd.DataFrame, threshold_factor: float = 2.0
) -> pd.DataFrame:
    """Return buckets whose `e2e_p99_us` exceeds
    `threshold_factor * median_p99` across all buckets."""
    if buckets.empty:
        return buckets.iloc[0:0]
    median = float(buckets["e2e_p99_us"].median())
    return buckets[buckets["e2e_p99_us"] > threshold_factor * median]
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
python scripts/analyze-stability.py --unittest 2>&1 | tail -10
```

Expected: 9 tests pass total (5 prior + 4 new drift tests).

- [ ] **Step 5: Commit**

```bash
git add scripts/analyze-stability.py
git commit -m "scripts: analyze-stability.py drift slope + outlier_buckets (4 tests)"
```

---

## Task 3: CLI + main() wiring

**Files:**
- Modify: `scripts/analyze-stability.py`

- [ ] **Step 1: Replace the stub `__main__` block**

In `scripts/analyze-stability.py`, find the existing `if __name__ == "__main__":`
block. Replace its body with:

```python
def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Bucket a prdt-bench-matrix per-frame CSV by minute "
        "and emit per-bucket percentile stats + drift detection."
    )
    parser.add_argument("input_csv", type=Path, help="per-frame.csv path")
    parser.add_argument(
        "--bucket-seconds",
        type=int,
        default=60,
        help="bucket width in seconds (default 60)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="output CSV path (default: <input>.buckets.csv)",
    )
    args = parser.parse_args(argv)

    if not args.input_csv.is_file():
        print(f"input file not found: {args.input_csv}", file=sys.stderr)
        return 2

    try:
        frames = pd.read_csv(args.input_csv)
    except Exception as e:
        print(f"failed to read {args.input_csv}: {e}", file=sys.stderr)
        return 2

    try:
        buckets = bucket_frames(frames, args.bucket_seconds)
    except ValueError as e:
        print(str(e), file=sys.stderr)
        return 2

    out_path = args.out or args.input_csv.with_suffix(".buckets.csv")
    buckets.to_csv(out_path, index=False)

    # Drift summary to stdout
    total_frames = int(frames.shape[0]) if not frames.empty else 0
    print(f"input: {args.input_csv}")
    print(f"output: {out_path}")
    print(f"total frames: {total_frames}")
    print(f"buckets: {len(buckets)}")
    if buckets.empty:
        print("no frames; nothing to analyze")
        return 0

    slope = e2e_p50_slope_us_per_minute(buckets)
    e2e_max = int(buckets["e2e_p50_us"].max())
    e2e_min = int(buckets["e2e_p50_us"].min())
    frames_var = float(buckets["frames_in_bucket"].var(ddof=0))
    print(f"e2e_p50_us slope: {slope:.2f} us/bucket")
    print(f"e2e_p50_us max-min: {e2e_max - e2e_min} us")
    print(f"frames_in_bucket variance: {frames_var:.1f}")

    outs = outlier_buckets(buckets, threshold_factor=2.0)
    if outs.empty:
        print("outlier buckets: none (e2e_p99 > 2x median)")
    else:
        print(f"outlier buckets ({len(outs)}): e2e_p99 > 2x median")
        print(outs[["bucket_idx", "bucket_start_s", "e2e_p99_us"]].to_string(index=False))
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--unittest":
        sys.argv.pop(1)
        unittest.main()
    else:
        sys.exit(main())
```

- [ ] **Step 2: Verify unit tests still pass**

```bash
python scripts/analyze-stability.py --unittest 2>&1 | tail -5
```

Expected: 9 tests pass.

- [ ] **Step 3: Smoke the CLI on a tiny synthetic input**

Create a temporary CSV in /tmp and run the script:

```bash
python -c "
import pandas as pd
df = pd.DataFrame({
    'seq': list(range(10)),
    'capture_us': [i * 1_000_000 for i in range(10)],
    'encode_done_us': [i * 1_000_000 + 100 for i in range(10)],
    'recv_us': [i * 1_000_000 + 200 for i in range(10)],
    'decode_done_us': [i * 1_000_000 + 300 for i in range(10)],
    'arrival_lag_us': [200] * 10,
    'decode_lag_us': [100] * 10,
    'e2e_lag_us': [300] * 10,
})
df.to_csv('/tmp/synthetic.csv', index=False)
"
python scripts/analyze-stability.py /tmp/synthetic.csv --bucket-seconds 5
cat /tmp/synthetic.buckets.csv
```

Expected: prints 2 buckets (10 s span / 5 s buckets = 2), CSV file exists with 2 data rows + header.

- [ ] **Step 4: Verify error path on missing file**

```bash
python scripts/analyze-stability.py /tmp/does-not-exist.csv 2>&1
echo "exit=$?"
```

Expected: stderr "input file not found: /tmp/does-not-exist.csv", exit 2.

- [ ] **Step 5: Commit**

```bash
git add scripts/analyze-stability.py
git commit -m "scripts: analyze-stability.py CLI main() + drift summary stdout"
```

---

## Task 4: Manual 30-min run + docs + tag

**Files:**
- Create: `docs/stability-bench.md`
- Modify: `docs/superpowers/STATUS.md`

- [ ] **Step 1: Run 30-minute bench**

```bash
cd /e/project/rust-desktop/power-remote-dt
export NV_CODEC_SDK_PATH="C:/SDK/Video_Codec_SDK_13.0.37"
export LIBCLANG_PATH="C:/Program Files/LLVM/bin"
export CUDA_PATH="C:/Program Files/NVIDIA GPU Computing Toolkit/CUDA/v13.2"
RUST_LOG=info cargo run --release -p prdt-latency-bench --bin prdt-bench-matrix -- \
    --out-dir bench-results/stability-30m/ \
    --resolutions 1080 \
    --bitrates 30 \
    --decoders nvdec \
    --fps 60 \
    --duration 30m
```

Expected: ~30 minutes wall time. Produces `bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv` (~108k rows = 60fps × 30min × 60s).

The bench may be run in the background — see `run_in_background: true` in tooling.

- [ ] **Step 2: Run analysis**

```bash
python scripts/analyze-stability.py \
    bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv
```

Expected stdout:
- `input:`, `output:`, `total frames:` lines
- `buckets: 30` (or 29-30 depending on exact timing)
- `e2e_p50_us slope: ` value (close to 0 means stable)
- `e2e_p50_us max-min: ` (drift range)
- `outlier buckets: ` (none expected for healthy run)

Output CSV: `bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.buckets.csv` with 30 rows.

- [ ] **Step 3: Create `docs/stability-bench.md` with real results**

Replace the placeholders in this template with the actual numbers
from Step 2's stdout:

```markdown
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
- `arrival_p50/95/99_us`, `decode_p50/95/99_us`, `e2e_p50/95/99_us`: per-bucket percentiles using round-style picking (compatible with Rust `prdt_latency_bench::percentiles`).

## Stdout summary

Beyond the CSV, the script prints:

- **`e2e_p50_us slope`**: linear regression slope of `e2e_p50_us` vs `bucket_idx`. Units are µs per bucket. A non-zero slope indicates drift.
- **`e2e_p50_us max-min`**: range across buckets (drift envelope).
- **`frames_in_bucket variance`**: variance of frames per bucket. High variance suggests bursty drops or scheduling stalls.
- **`outlier buckets`**: buckets with `e2e_p99_us > 2 × median(e2e_p99_us)` across the run. None expected for a healthy run.

## 30-minute run on RTX 3070 Ti (2026-04-26)

Config: 1080p60, 30 Mbps, NVENC + NVDEC.

[INSERT actual numbers from Step 2 stdout here.]

```
input: bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.csv
output: bench-results/stability-30m/per-frame/1080p60-30mbps-nvdec.buckets.csv
total frames: <ACTUAL>
buckets: <ACTUAL>
e2e_p50_us slope: <ACTUAL> us/bucket
e2e_p50_us max-min: <ACTUAL> us
frames_in_bucket variance: <ACTUAL>
outlier buckets: <ACTUAL>
```

Interpretation:

- Slope close to 0 → no significant drift over 30 minutes.
- Max-min of a few hundred µs is normal jitter.
- Outlier buckets at the start (warm-up) are expected; mid-run outliers indicate transient stalls worth investigating.

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
  (~5-10 MB per minute of bench wall time). Disk space and
  pandas memory both fine, but be aware.
- A single process runs encode + decode on one GPU; under 4K or
  high-bitrate configs the GPU may saturate and skew "stability"
  measurements. The default 1080p60 30Mbps config is well below
  saturation on RTX 3070 Ti (verified in B1).
- `bucket_idx` is integer-divided from `capture_us`; the last bucket
  may contain fewer frames if the run ends mid-bucket. The script
  reports `frames_in_bucket` so this is visible.
```

After running Step 2 above, replace each `<ACTUAL>` placeholder in
the markdown with the matching value from the script's stdout.

- [ ] **Step 4: Update STATUS.md**

In `docs/superpowers/STATUS.md`, find these lines and update them.

Replace:

```markdown
**Latest tag:** `plan4-b4-net-profile-bench-complete`
**Branch state:** master (all phase work merged) — **Phase 4 + Plan 4 B1 + B4 + B6 + B7 完了**
**Test count:** 305 automated tests across the workspace, all passing
```

with:

```markdown
**Latest tag:** `plan4-b8-stability-bench-complete`
**Branch state:** master (all phase work merged) — **Phase 4 + Plan 4 B1 + B4 + B6 + B7 + B8 完了 (B3 のみ HW ブロック保留)**
**Test count:** 305 automated tests across the workspace, all passing
```

(B8 adds Python tests, not Rust workspace tests, so the count stays.)

In the Plan 4 table after the `plan4-b4-net-profile-bench-complete` row, append a new row:

```markdown
| `plan4-b8-stability-bench-complete` | 30 分長時間安定性。新 Rust コードなし — 既存 `prdt-bench-matrix --duration 30m --resolutions 1080 --bitrates 30 --decoders nvdec --fps 60` を実機で 1 回走らせ、新 `scripts/analyze-stability.py` で per-frame CSV を分単位 bucket に変換 + drift / outlier 検出。`bucket_frames` / `e2e_p50_slope_us_per_minute` / `outlier_buckets` + 9 unit tests(pandas + numpy)。`docs/stability-bench.md` に schema + 実 30-min run 結果 + interpretation。Out of scope: real network drift、OS-level memory leaks、multi-config matrix sweep(時間爆発)、glass-to-glass(M3)。 |
```

In the residual A1 section, change `B8` from pending to done. Find:

```markdown
- **B8: 30 分長時間安定性(30分連続接続でのレイテンシ・パケットロス推移)** — host/viewer 本体に bench mode 追加
```

Replace with:

```markdown
- ~~**B8: 30 分長時間安定性**~~ ✅ (2026-04-26、`plan4-b8-stability-bench-complete`、`scripts/analyze-stability.py` で 30-min bench-matrix 出力を分単位 bucket 解析、drift / outlier 検出)
```

- [ ] **Step 5: Commit docs**

```bash
git add docs/stability-bench.md docs/superpowers/STATUS.md
git commit -m "docs(plan4-b8): stability-bench.md + STATUS.md update with real 30-min results"
```

- [ ] **Step 6: Tag**

```bash
git tag -a plan4-b8-stability-bench-complete -m "$(cat <<'EOF'
Plan 4 B8 stability bench complete

Adds Python time-series analysis script that buckets prdt-bench-matrix
per-frame CSV by minute, plus a documented 30-minute manual run procedure.

- scripts/analyze-stability.py: bucket_frames + percentile_round
  (compatible with Rust prdt_latency_bench::percentiles round-style)
  + e2e_p50_slope_us_per_minute (linear regression) + outlier_buckets
  (e2e_p99 > 2x median) + CLI main() with stdout drift summary
- 9 unittest tests via `python scripts/analyze-stability.py --unittest`
- docs/stability-bench.md with usage + schema + actual 30-min run
  results from RTX 3070 Ti + interpretation
- No new Rust code — leverages prdt-bench-matrix --duration 30m

Out of scope: real network drift, OS-level memory leaks, multi-config
matrix (would take ~30h), glass-to-glass display latency (Plan 4 M3).
EOF
)"
git tag | grep plan4-b
```

Expected: lists `plan4-b1-bench-matrix-complete`, `plan4-b4-net-profile-bench-complete`, `plan4-b6-fec-bench-complete`, `plan4-b7-input-load-bench-complete`, `plan4-b8-stability-bench-complete`.

- [ ] **Step 7: Final summary report**

Report:
- Files added: `scripts/analyze-stability.py`, `docs/stability-bench.md`
- File modified: `docs/superpowers/STATUS.md`
- 9 Python unit tests pass
- Manual 30-min run completed successfully
- Drift summary numbers (slope, max-min, outliers)
- Tag listing

## Self-review checklist

- [ ] `scripts/analyze-stability.py` exists with `bucket_frames`, `percentile_round`, `e2e_p50_slope_us_per_minute`, `outlier_buckets`, `main()`
- [ ] 9 unittests pass via `--unittest` flag
- [ ] CLI smoke (synthetic 10-frame input, 5s buckets) produces 2-row CSV
- [ ] Missing-file error path returns exit 2 with stderr message
- [ ] 30-min `prdt-bench-matrix` run produces ~108k-row per-frame CSV
- [ ] `analyze-stability.py` on real data produces ~30 buckets
- [ ] `docs/stability-bench.md` contains real 30-min results (not `<ACTUAL>` placeholders)
- [ ] STATUS.md updated (latest tag, B8 entry in Plan 4 table, residual A1 marked complete)
- [ ] tag `plan4-b8-stability-bench-complete` created

---

## Risks & Notes for Implementer

- **`pandas` already installed**: B1 added it. If `python -c "import pandas"` fails, `pip install --quiet pandas`. `numpy` ships as a pandas dep.
- **Python 3.12+ syntax**: `list[str] | None` in `main()` requires 3.10+. Workspace machine has 3.12 (verified in B1). If older Python, wrap as `Optional[List[str]]` from `typing`.
- **`np.polyfit(x, y, 1)`**: returns `(slope, intercept)`. We unpack the tuple; if degree=1 produces `array([slope, intercept])` we still get scalar from `slope, _ = ...`.
- **Round-style percentile**: `int(round((n-1) * p))`. Python's `round()` rounds half-to-even (banker's rounding) while Rust's `f64::round` rounds half-away-from-zero. For our test cases (49.5 → 50, 94.05 → 94, 98.01 → 98), banker's rounding produces the same results because the half cases (49.5, 24.5) are not at .5 of an even/odd boundary that flips. Verified manually:
  - `round(49.5)` Python = 50 (rounds to even); Rust = 50 (rounds away from zero) — same
  - `round(94.05)` = 94 in both
  - `round(98.01)` = 98 in both
  Edge case: bucket size 1 — never happens in practice but `percentile_round` handles n=1 (sorted_values[0] for any p).
- **`buckets["bucket_idx"].astype(int)`**: pandas may return int64. Float-to-int conversion is fine.
- **Group-by ordering**: `frame_df.groupby(bucket_idx)` returns groups in sorted order by default in pandas. The output rows are in `bucket_idx` order — verified by the `test_buckets_by_capture_us` assertion order.
- **`with_suffix(".buckets.csv")`**: `Path("/foo/bar.csv").with_suffix(".buckets.csv")` produces `/foo/bar.buckets.csv` — replaces the existing `.csv` suffix with `.buckets.csv`. Verified.
- **30-min wall time**: Step 1 of Task 4 takes 30 minutes. Subagent should kick this off as a background task and proceed to write docs while waiting, then fill in real numbers when the run completes.

---

## Self-Review

**Spec coverage:**
- §Architecture (Python script, no new Rust) → Tasks 1-3 ✓
- §B1 per-frame CSV columns documented as input → Task 1 REQUIRED_COLUMNS ✓
- §CLI (positional + --bucket-seconds + --out) → Task 3 main() ✓
- §Output schema (12 columns) → Task 1 OUTPUT_COLUMNS + Task 1 row dict ✓
- §Drift detection (slope, max-min, frames variance, outliers) → Task 3 stdout ✓
- §Manual run procedure → Task 4 ✓
- §Tests (5 minimum) → 9 tests across Tasks 1+2 (3 percentile + 4 drift + 2 missing-col/empty) ✓
- §Error handling (missing file, malformed CSV, empty data) → Task 3 main() ✓
- §Exit criteria 5 items → Tasks 1-4 cover all ✓

**Placeholder scan:**
- The placeholder `<ACTUAL>` in `docs/stability-bench.md` is INTENTIONAL — Step 3 of Task 4 explicitly tells the implementer to replace these with real run numbers. Not a plan failure, it's a templating hole that Step 3 fills in.

**Type consistency:**
- `bucket_frames(frame_df, bucket_seconds) -> pd.DataFrame` — Task 1 def, Task 3 use ✓
- `percentile_round(values, p) -> int` — Task 1 def, used internally in `bucket_frames` ✓
- `e2e_p50_slope_us_per_minute(buckets) -> float` — Task 2 def, Task 3 use ✓
- `outlier_buckets(buckets, threshold_factor) -> pd.DataFrame` — Task 2 def, Task 3 use ✓
- `OUTPUT_COLUMNS` constant — Task 1, used by Task 3 to validate output schema ✓
- `REQUIRED_COLUMNS` constant — Task 1, used by `bucket_frames` ✓
