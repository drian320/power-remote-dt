#!/usr/bin/env python3
"""Reject any modification inside protected 8-bit-helper line ranges.

Per P3 RALPLAN-DR iter-2 change #1 + #2: the bodies of the 8-bit FFmpeg HEVC
helpers MUST stay byte-for-byte identical to master. This script parses
`git diff origin/master` hunks for the listed files and fails CI if any
+/- line falls strictly inside the protected line range (in OLD-file
coordinates).

Per iter-3 Critic fix: the anchor for `+` additions was previously
computed as `max(old_line - 1, 1)`, which mis-flags pure additions that
land at `hi + 1` (e.g. a `_main10` sibling appended immediately after
the protected range's last line). The corrected logic only flags `+`
lines whose insertion point in OLD-file coordinates falls STRICTLY
inside `[lo, hi]`, not at `hi + 1`. The hunk-header `@@ -old_lo,old_count
+new_lo,new_count @@` is parsed and `old_line` is advanced only by
context (' ') and deletion ('-') lines, so when a `+` line is read,
`old_line` already points to the OLD-file row where the new content
would land. We check `lo <= old_line <= hi`, NOT `lo <= old_line-1 <= hi`.

PROTECTED entries support two forms:
  - (path, lo, hi)                       — explicit line-number range (OLD-file
                                           coordinates, as before)
  - (path, start_marker, end_marker)     — named-region markers; the script
                                           resolves the range at startup by
                                           grepping the origin/master version
                                           of the file for the marker strings.
                                           lo is the line AFTER start_marker;
                                           hi is the line BEFORE end_marker.
                                           The marker lines themselves are NOT
                                           part of the protected range.

Inline test cases:

  CASE A — modification at line 50 (inside [38, 87]):
    diff hunk `@@ -48,5 +48,5 @@\\n ctx;\\n-old_line;\\n+new_line;\\n ctx;`
    old_line trace: starts at 48, advances 48->49 (context), then `-`
    line at 49 triggers `lo(38) <= 49 <= hi(87)` -> BAD. EXPECTED: FAIL.

  CASE B — pure addition appended at line 88 (right after hi=87):
    diff hunk `@@ -85,3 +85,5 @@\\n ctx_85;\\n ctx_86;\\n ctx_87;\\n+new_88;\\n+new_89;`
    old_line trace: starts at 85, advances 85->86->87->88 (3 context
    lines), then `+new_88` is read with old_line=88. Check
    `lo(38) <= 88 <= hi(87)` -> 88 > 87 -> NOT flagged. EXPECTED: PASS.

  CASE C — marker-anchored protected region: modification inside must FAIL,
    pure append outside (after endregion marker) must PASS.
"""

import re
import subprocess
import sys
from typing import Optional, Tuple, Union

PROTECTED: list[Union[Tuple[str, int, int], Tuple[str, str, str]]] = [
    ("crates/media-ffmpeg/src/options.rs", 38, 87),
    ("crates/media-ffmpeg/src/nvenc_common.rs", 41, 96),
    # P3 PR2: copy_nv12_planes is the 8-bit decoder plane-copy helper; its body
    # must stay byte-stable when copy_p010_planes (_main10 sibling) is appended
    # after it. Range is in OLD-file (master) coordinates: lines 56-83.
    ("crates/media-ffmpeg/src/decoder_common.rs", 56, 83),
    # F8: named-region marker-anchored ranges — resolved at startup from
    # origin/master content. The marker lines themselves are NOT protected.
    (
        "crates/media-win/src/mf/decoder.rs",
        "// region: 8-bit-mf-decoder",
        "// endregion: 8-bit-mf-decoder",
    ),
    (
        "crates/media-win/src/pipeline/consumer.rs",
        "// region: 8-bit-mf-consumer",
        "// endregion: 8-bit-mf-consumer",
    ),
]
HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def resolve_marker_range(path: str, start_marker: str, end_marker: str) -> Optional[Tuple[int, int]]:
    """Resolve a named-region marker pair to a (lo, hi) line-number range.

    Greps the origin/master version of `path` for `start_marker` and
    `end_marker`. Returns (lo, hi) where lo is the line number immediately
    AFTER start_marker and hi is the line immediately BEFORE end_marker
    (1-based, inclusive). The marker lines themselves are not in [lo, hi].

    Returns None if either marker is not found (script continues without
    protecting this range, but emits a warning).
    """
    try:
        content = subprocess.check_output(
            ["git", "show", f"origin/master:{path}"], text=True
        )
    except subprocess.CalledProcessError:
        print(
            f"::warning::marker-resolve: could not read origin/master:{path} "
            f"(file may be new on this branch — range protection skipped)",
            file=sys.stderr,
        )
        return None

    start_lineno: Optional[int] = None
    end_lineno: Optional[int] = None
    for i, line in enumerate(content.splitlines(), start=1):
        if start_marker in line and start_lineno is None:
            start_lineno = i
        elif end_marker in line and end_lineno is None:
            end_lineno = i

    if start_lineno is None or end_lineno is None:
        print(
            f"::warning::marker-resolve: markers not found in origin/master:{path} "
            f"(start={start_marker!r} found={start_lineno is not None}, "
            f"end={end_marker!r} found={end_lineno is not None}) — range protection skipped",
            file=sys.stderr,
        )
        return None

    lo = start_lineno + 1
    hi = end_lineno - 1
    if lo > hi:
        print(
            f"::warning::marker-resolve: empty range [{lo}, {hi}] for {path} "
            f"(markers are adjacent) — range protection skipped",
            file=sys.stderr,
        )
        return None

    return (lo, hi)


def check(path: str, lo: int, hi: int) -> bool:
    """Return True if the protected range [lo, hi] is unmodified, False otherwise."""
    diff = subprocess.check_output(
        ["git", "diff", "origin/master", "--", path], text=True
    )
    if not diff.strip():
        return True

    old_line: Optional[int] = None
    new_line: Optional[int] = None
    bad = []

    for line in diff.splitlines():
        m = HUNK.match(line)
        if m:
            # Hunk header gives the starting OLD-file line for this hunk.
            old_line = int(m.group(1))
            new_line = int(m.group(3))
            continue

        if old_line is None:
            continue

        if line.startswith("-") and not line.startswith("---"):
            # Deletion: the removed line was at OLD-file row `old_line`.
            # Flag it if it falls inside the protected range.
            if lo <= old_line <= hi:
                bad.append((old_line, line))
            old_line += 1

        elif line.startswith("+") and not line.startswith("+++"):
            # Addition: the new content lands at OLD-file row `old_line`
            # (i.e. shifts existing row `old_line` down by one).
            # Only flag if the landing row is STRICTLY inside [lo, hi].
            # An insertion at `hi + 1` (sibling appended after the
            # protected range) is intentionally allowed — CASE B above.
            if lo <= old_line <= hi:
                bad.append((old_line, line))
            # `+` does NOT advance old_line — it only consumes a new-file row.
            new_line += 1  # type: ignore[operator]

        elif line.startswith(" "):
            old_line += 1
            new_line += 1  # type: ignore[operator]

    if bad:
        print(
            f"::error::{path}: {len(bad)} modification(s) in protected range [{lo}, {hi}]:",
            file=sys.stderr,
        )
        for ln, txt in bad[:20]:
            print(f"  line {ln}: {txt}", file=sys.stderr)
        return False

    return True


def _run_self_tests() -> None:
    """Verify CASE A, CASE B, and CASE C logic against synthetic diffs."""

    def _parse(diff_text: str, lo: int, hi: int) -> list:
        bad = []
        old_line = None
        new_line = None
        for line in diff_text.splitlines():
            m = HUNK.match(line)
            if m:
                old_line = int(m.group(1))
                new_line = int(m.group(3))
                continue
            if old_line is None:
                continue
            if line.startswith("-") and not line.startswith("---"):
                if lo <= old_line <= hi:
                    bad.append((old_line, line))
                old_line += 1
            elif line.startswith("+") and not line.startswith("+++"):
                if lo <= old_line <= hi:
                    bad.append((old_line, line))
                new_line += 1
            elif line.startswith(" "):
                old_line += 1
                new_line += 1
        return bad

    # CASE A: substitution at line 49 — inside [38, 87] — must FAIL
    case_a = "@@ -48,5 +48,5 @@\n ctx;\n-old_line;\n+new_line;\n ctx;"
    result_a = _parse(case_a, 38, 87)
    assert result_a, f"CASE A should have flagged a violation but did not: {result_a}"

    # CASE B: pure addition after line 87 — must PASS (no violations)
    case_b = "@@ -85,3 +85,5 @@\n ctx_85;\n ctx_86;\n ctx_87;\n+new_88;\n+new_89;"
    result_b = _parse(case_b, 38, 87)
    assert not result_b, f"CASE B should have no violations but got: {result_b}"

    # CASE C: marker-anchored protected region [lo=3, hi=5] (markers at lines
    # 2 and 6, content lines 3-5).
    # C.1: modification inside (line 4) — must FAIL
    case_c1 = "@@ -3,3 +3,3 @@\n ctx_3;\n-old_4;\n+new_4;\n ctx_5;"
    result_c1 = _parse(case_c1, 3, 5)
    assert result_c1, f"CASE C.1 should have flagged a violation but did not: {result_c1}"

    # C.2: pure append after endregion marker (line 7, outside [3,5]) — must PASS
    case_c2 = "@@ -5,2 +5,4 @@\n ctx_5;\n ctx_6;\n+new_7;\n+new_8;"
    result_c2 = _parse(case_c2, 3, 5)
    assert not result_c2, f"CASE C.2 should have no violations but got: {result_c2}"

    print("self-tests PASS (CASE A flagged correctly; CASE B passed correctly; CASE C marker-anchor correct)")


def _resolve_all_protected() -> list[Tuple[str, int, int]]:
    """Resolve all PROTECTED entries to (path, lo, hi) tuples.

    Entries that are already (path, int, int) are passed through. Entries that
    are (path, str, str) marker tuples are resolved via resolve_marker_range;
    entries where the markers are not found on origin/master are skipped with a
    warning (the file may be new on this branch).
    """
    resolved = []
    for entry in PROTECTED:
        path, second, third = entry
        if isinstance(second, int) and isinstance(third, int):
            resolved.append((path, second, third))
        else:
            result = resolve_marker_range(path, second, third)  # type: ignore[arg-type]
            if result is not None:
                resolved.append((path, result[0], result[1]))
    return resolved


if __name__ == "__main__":
    _run_self_tests()

    resolved = _resolve_all_protected()

    ok = True
    for path, lo, hi in resolved:
        if not check(path, lo, hi):
            ok = False

    if ok:
        checked = "; ".join(f"{p}:[{lo},{hi}]" for p, lo, hi in resolved)
        print(f"byte-identity guard PASS: {checked}")
    sys.exit(0 if ok else 1)
