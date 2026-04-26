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


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--unittest":
        sys.argv.pop(1)
        unittest.main()
    else:
        # The CLI main() is wired in Task 3; this stub is replaced.
        print("Use --unittest for tests; CLI not yet implemented (Task 3)", file=sys.stderr)
        sys.exit(2)
