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
"""

import re
import subprocess
import sys
from typing import Optional

PROTECTED = [
    ("crates/media-ffmpeg/src/options.rs", 38, 87),
    ("crates/media-ffmpeg/src/nvenc_common.rs", 41, 96),
]
HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


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
    """Verify CASE A and CASE B logic against synthetic diffs."""

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

    print("self-tests PASS (CASE A flagged correctly; CASE B passed correctly)")


if __name__ == "__main__":
    _run_self_tests()

    ok = True
    for path, lo, hi in PROTECTED:
        if not check(path, lo, hi):
            ok = False

    if ok:
        print(
            "byte-identity guard PASS: "
            "options.rs:38-87 unchanged; nvenc_common.rs:41-96 unchanged"
        )
    sys.exit(0 if ok else 1)
