"""Analyze a `prdt-bench-matrix` run directory.

Reads `summary.csv` + every per-frame CSV under `per-frame/`, then prints:
- Skip / loss summary
- Per-stage latency (encode / transport / decode / e2e) descriptive stats
- Stability (stddev / coefficient of variation) per config
- Paired NVDEC vs MF comparison at matching (resolution, bitrate, fps)
- Outlier configs where p99/p50 > 5x

Usage: python scripts/analyze-bench-matrix.py <out-dir>
"""

from __future__ import annotations

import sys
from pathlib import Path

import pandas as pd


def load_run(out_dir: Path) -> tuple[pd.DataFrame, pd.DataFrame]:
    """Return (summary_df, per_frame_long_df).

    `per_frame_long_df` has one row per frame across every config_id, with a
    config_id column for grouping/filtering.
    """
    summary = pd.read_csv(out_dir / "summary.csv")
    frames: list[pd.DataFrame] = []
    for csv in sorted((out_dir / "per-frame").glob("*.csv")):
        df = pd.read_csv(csv)
        df["config_id"] = csv.stem
        frames.append(df)
    return summary, pd.concat(frames, ignore_index=True)


def section(title: str) -> None:
    print()
    print("=" * 70)
    print(title)
    print("=" * 70)


def loss_breakdown(summary: pd.DataFrame) -> None:
    section("Loss / skip summary")
    skipped = summary[summary["loss_ppm"] == 1_000_000]
    if not skipped.empty:
        print(f"Skipped configs (NVENC/decoder init failed): {len(skipped)}")
        for cid in skipped["config_id"]:
            print(f"  {cid}")
    else:
        print("Skipped configs: 0")
    real = summary[summary["loss_ppm"] != 1_000_000]
    print(f"\nLoss across {len(real)} successful configs:")
    print(f"  median loss_ppm: {int(real['loss_ppm'].median())}")
    print(f"  max    loss_ppm: {int(real['loss_ppm'].max())}")
    print(f"  by decoder:")
    for dec, g in real.groupby("decoder"):
        print(
            f"    {dec:5s}: median={int(g['loss_ppm'].median()):>5} "
            f"max={int(g['loss_ppm'].max()):>5} n={len(g)}"
        )


def per_stage_lags(per_frame: pd.DataFrame, summary: pd.DataFrame) -> None:
    """Add encode_lag column and dump descriptive stats per (decoder, stage)."""
    section("Per-stage latency descriptive stats (microseconds)")
    pf = per_frame.copy()
    pf["encode_lag_us"] = pf["encode_done_us"] - pf["capture_us"]
    pf["transport_lag_us"] = pf["recv_us"] - pf["encode_done_us"]
    # Inner-join to get decoder column on each frame.
    pf = pf.merge(summary[["config_id", "decoder", "resolution", "fps"]], on="config_id")

    for stage in ["encode_lag_us", "transport_lag_us", "decode_lag_us", "e2e_lag_us"]:
        print(f"\n{stage}:")
        rows = []
        for dec, g in pf.groupby("decoder"):
            rows.append({
                "decoder": dec,
                "count": int(len(g)),
                "mean": int(g[stage].mean()),
                "std": int(g[stage].std()),
                "p50": int(g[stage].quantile(0.50)),
                "p95": int(g[stage].quantile(0.95)),
                "p99": int(g[stage].quantile(0.99)),
                "max": int(g[stage].max()),
            })
        print(pd.DataFrame(rows).set_index("decoder").to_string())


def stability_table(per_frame: pd.DataFrame, summary: pd.DataFrame) -> None:
    section("Stability (e2e coefficient of variation = stddev/mean) per config")
    pf = per_frame.merge(summary[["config_id", "decoder"]], on="config_id")
    cv = pf.groupby("config_id")["e2e_lag_us"].agg(["mean", "std"]).reset_index()
    cv["mean"] = cv["mean"].astype(float)
    cv["std"] = cv["std"].astype(float)
    cv["cv"] = (cv["std"] / cv["mean"]).round(3)
    cv = cv.merge(summary[["config_id", "decoder"]], on="config_id")
    cv = cv.dropna(subset=["cv"])
    print("\nMost stable (lowest CV) -- top 5:")
    print(cv.nsmallest(5, "cv").to_string(index=False))
    print("\nLeast stable (highest CV) -- top 5:")
    print(cv.nlargest(5, "cv").to_string(index=False))
    print(f"\nMedian CV: {cv['cv'].median():.3f}")
    print(f"NVDEC median CV: {cv[cv['decoder']=='nvdec']['cv'].median():.3f}")
    print(f"MF    median CV: {cv[cv['decoder']=='mf']['cv'].median():.3f}")


def paired_nvdec_vs_mf(summary: pd.DataFrame) -> None:
    """Pair each (resolution, bitrate, fps) MF row with its NVDEC sibling."""
    section("Paired comparison: NVDEC vs MF at matching (res, bitrate, fps)")
    real = summary[summary["loss_ppm"] != 1_000_000]
    pivot = real.pivot_table(
        index=["resolution", "bitrate_mbps", "fps"],
        columns="decoder",
        values=["e2e_p50_us", "e2e_p95_us", "e2e_p99_us"],
    )
    # Compute relative speedup (NVDEC vs MF).
    out = pd.DataFrame(index=pivot.index)
    for col in ["e2e_p50_us", "e2e_p95_us", "e2e_p99_us"]:
        if (col, "nvdec") in pivot.columns and (col, "mf") in pivot.columns:
            out[f"{col}_mf"] = pivot[(col, "mf")].astype("Int64")
            out[f"{col}_nvdec"] = pivot[(col, "nvdec")].astype("Int64")
            out[f"{col}_ratio"] = (
                pivot[(col, "nvdec")] / pivot[(col, "mf")]
            ).round(2)
    print(out.to_string())
    n = out["e2e_p50_us_ratio"].notna().sum()
    median_ratio = out["e2e_p50_us_ratio"].median()
    wins = (out["e2e_p50_us_ratio"] < 1.0).sum()
    print(
        f"\nNVDEC wins on e2e_p50: {wins}/{n} configs, median ratio {median_ratio:.2f}"
    )


def outlier_configs(summary: pd.DataFrame) -> None:
    section("Outlier configs (p99/p50 > 5x -- long-tail latency)")
    real = summary[summary["loss_ppm"] != 1_000_000].copy()
    real["e2e_tail_ratio"] = real["e2e_p99_us"] / real["e2e_p50_us"]
    bad = real[real["e2e_tail_ratio"] > 5.0].sort_values(
        "e2e_tail_ratio", ascending=False
    )
    if bad.empty:
        worst = real.nlargest(5, "e2e_tail_ratio")[
            ["config_id", "e2e_p50_us", "e2e_p99_us", "e2e_tail_ratio"]
        ]
        print("None above 5x. Worst tail-ratio configs:")
        print(worst.to_string(index=False))
    else:
        print(
            bad[
                ["config_id", "e2e_p50_us", "e2e_p99_us", "e2e_tail_ratio"]
            ].to_string(index=False)
        )


def fps_ratio_table(summary: pd.DataFrame) -> None:
    """Compare 60fps vs 120fps at matching (resolution, bitrate, decoder)."""
    section("60fps vs 120fps e2e_p50 (does doubling fps add latency?)")
    real = summary[summary["loss_ppm"] != 1_000_000]
    pivot = real.pivot_table(
        index=["resolution", "bitrate_mbps", "decoder"],
        columns="fps",
        values="e2e_p50_us",
    )
    pivot.columns = [f"e2e_p50_us_fps{c}" for c in pivot.columns]
    if "e2e_p50_us_fps60" in pivot.columns and "e2e_p50_us_fps120" in pivot.columns:
        pivot["ratio_120_over_60"] = (
            pivot["e2e_p50_us_fps120"] / pivot["e2e_p50_us_fps60"]
        ).round(2)
    print(pivot.to_string())
    if "ratio_120_over_60" in pivot.columns:
        med = pivot["ratio_120_over_60"].median()
        print(f"\nMedian ratio (120/60): {med:.2f}  (close to 1.0 means fps not bottleneck)")


def main() -> None:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    out_dir = Path(sys.argv[1])
    summary, per_frame = load_run(out_dir)
    print(f"Loaded {len(summary)} configs, {len(per_frame):,} per-frame samples")

    loss_breakdown(summary)
    per_stage_lags(per_frame, summary)
    stability_table(per_frame, summary)
    paired_nvdec_vs_mf(summary)
    outlier_configs(summary)
    fps_ratio_table(summary)


if __name__ == "__main__":
    main()
