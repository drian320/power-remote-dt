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
    # int(x + 0.5) replicates Rust's f64::round() (half-away-from-zero)
    # rather than Python's banker's rounding (half-to-even). Required for
    # exact agreement with prdt_latency_bench::percentiles output.
    idx = int((sorted_values.size - 1) * p + 0.5)
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
    for idx, group in frame_df.groupby(bucket_idx, sort=True):
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


def e2e_p50_slope_us_per_minute(buckets: pd.DataFrame) -> float:
    """Linear regression slope of e2e_p50_us against wall-clock time,
    expressed in µs per minute. Uses `bucket_start_s` (seconds since
    first frame) divided by 60 as the x-axis so the unit is correct
    regardless of `bucket_seconds`. Returns 0.0 for ≤1 row."""
    n = len(buckets)
    if n <= 1:
        return 0.0
    x_minutes = np.array(buckets["bucket_start_s"], dtype=np.float64) / 60.0
    y = np.array(buckets["e2e_p50_us"], dtype=np.float64)
    slope, _intercept = np.polyfit(x_minutes, y, 1)
    # Round to 2 decimals to suppress polyfit FP noise (~1e-14 for
    # constant input) so test assertions can use assertEqual exactly.
    return round(float(slope), 2)


def outlier_buckets(
    buckets: pd.DataFrame, threshold_factor: float = 2.0
) -> pd.DataFrame:
    """Return buckets whose `e2e_p99_us` exceeds
    `threshold_factor * median_p99` across all buckets."""
    if buckets.empty:
        return buckets.iloc[0:0]
    median = float(buckets["e2e_p99_us"].median())
    return buckets[buckets["e2e_p99_us"] > threshold_factor * median]


# === unittest module (run via `python scripts/analyze-stability.py --unittest`) ===


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

    def test_percentile_round_half_away_from_zero(self):
        # 2-element bucket, p=0.5 -> idx = round(1 * 0.5) = round(0.5).
        # Rust's f64::round(0.5) = 1.0 (half-away-from-zero).
        # Python's round(0.5) = 0 (banker's rounding).
        # We want Rust behavior: return sorted[1] = 200.
        v = np.array([100, 200], dtype=np.uint64)
        self.assertEqual(percentile_round(v, 0.5), 200)

        # 4-element, p=0.5 -> idx = round(3 * 0.5) = round(1.5).
        # Rust: 2.0; Python's round: 2 (banker rounds half-to-even, which
        # for 1.5 is 2). Agreement here is coincidental.
        v = np.array([10, 20, 30, 40], dtype=np.uint64)
        self.assertEqual(percentile_round(v, 0.5), 30)


class TestBucketFrames(unittest.TestCase):
    def test_buckets_by_capture_us(self):
        # 100 frames spanning 180 s of capture_us in bucket size 60 s.
        # Frame i has capture_us = i * 1_800_000 (1.8 s apart) so we
        # expect 100 frames distributed across 3 buckets:
        #   bucket 0: frames at 0, 1.8s, 3.6s, ..., 58.2s (34 frames at indices 0..=33)
        #   bucket 1: frames at 60s, 61.8s, ..., 118.2s (33 frames at indices 34..=66)
        #   bucket 2: frames at 120s, 121.8s, ..., 178.2s (33 frames at indices 67..=99)
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
        # 200 us increase per 4 buckets = 50 us per bucket
        self.assertEqual(slope, 50.0)

    def test_drift_slope_per_minute_with_30s_buckets(self):
        # Same 200us increase, but spread over 5 buckets at 30s apart.
        # Total time: 4 * 30 = 120 seconds = 2 minutes.
        # Expected slope: 200 us / 2 min = 100 us/min.
        # If the implementation incorrectly used bucket_idx as x-axis,
        # it would compute 200 / 4 = 50 us/(bucket-step) and label it
        # as us/min — wrong by 2x.
        buckets = pd.DataFrame(
            {
                "bucket_idx": list(range(5)),
                "bucket_start_s": [0, 30, 60, 90, 120],
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
        # 200 us / 2 min = 100 us/min
        self.assertEqual(slope, 100.0)

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
    print(f"e2e_p50_us slope: {slope:.2f} us/min")
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
