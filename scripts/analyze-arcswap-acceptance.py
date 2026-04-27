#!/usr/bin/env python3
"""ralplan iteration-4 acceptance analysis: N=5 baseline vs N=5 arcswap.

Reads bench-out/{baseline,arcswap}-{1..5}/summary.csv (single-row each — only
one (1080,30,nvdec,nvenc,60) config per run), extracts the relevant metrics,
computes median + sample stdev (df=4), and judges acceptance per the
nvdec-arcswap-complete ADR.

Run from repo root: python scripts/analyze-arcswap-acceptance.py
"""

import csv
import statistics as st
import sys
from pathlib import Path

# Repo root is the parent of scripts/. Bench results live at <repo>/bench-out/.
ROOT = Path(__file__).resolve().parent.parent / "bench-out"
METRICS = [
    "e2e_p50_us",
    "e2e_p95_us",
    "e2e_p99_us",
    "decode_p50_us",
    "decode_p95_us",
    "decode_p99_us",
    "loss_ppm",
]
PRIMARY = ["e2e_p99_us", "decode_p99_us"]


def load_run(name: str) -> dict[str, float]:
    p = ROOT / name / "summary.csv"
    with p.open() as f:
        reader = csv.DictReader(f)
        row = next(reader)
    return {m: float(row[m]) for m in METRICS}


def collect(prefix: str) -> dict[str, list[float]]:
    out = {m: [] for m in METRICS}
    for i in range(1, 6):
        run = load_run(f"{prefix}-{i}")
        for m in METRICS:
            out[m].append(run[m])
    return out


def stats(values: list[float]) -> tuple[float, float]:
    return (st.median(values), st.stdev(values))  # df = N-1


def main() -> int:
    base = collect("baseline")
    arc = collect("arcswap")

    print("# ralplan acceptance analysis (N=5)")
    print()

    rows = []
    fail = []
    for m in METRICS:
        b_med, b_sigma = stats(base[m])
        a_med, _ = stats(arc[m])
        threshold = b_med + 2 * b_sigma
        if m in PRIMARY:
            ok = a_med <= b_med
            verdict = "PASS (improved)" if ok else "FAIL (no improvement)"
        else:
            ok = a_med <= threshold
            verdict = "PASS (within +2σ)" if ok else "FAIL (regression)"
        if not ok:
            fail.append(m)
        rows.append((m, b_med, b_sigma, threshold, a_med, verdict))

    fmt = "| {:>16} | {:>14} | {:>10} | {:>14} | {:>14} | {} |"
    hdr = fmt.format("metric", "baseline_med", "sigma", "threshold+2σ", "arcswap_med", "verdict")
    print(hdr)
    print("|" + "|".join(["-" * (len(s) + 2) for s in hdr.split("|")[1:-1]]) + "|")
    for m, bm, bs, th, am, v in rows:
        bm_s = f"{bm:.1f}"
        bs_s = f"{bs:.1f}"
        th_s = f"{th:.1f}"
        am_s = f"{am:.1f}"
        print(fmt.format(m, bm_s, bs_s, th_s, am_s, v))

    print()
    if fail:
        print(f"OVERALL: FAIL ({len(fail)} metric(s) below threshold: {', '.join(fail)})")
        return 1
    print("OVERALL: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
