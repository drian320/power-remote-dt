#!/usr/bin/env python3
"""Verify every `unsafe {` block and `unsafe fn` has a `// SAFETY:` comment
within 3 lines above it. Exits 0 on success, 1 with offending file:line list."""

import re
import sys
from pathlib import Path

UNSAFE_PAT = re.compile(r"^\s*unsafe\s*(\{|fn\s)")
SAFETY_PAT = re.compile(r"^\s*//\s*SAFETY:")
SKIP_DIRS = {"target", "vendor"}


def check_file(path: Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    offenders = []
    for i, line in enumerate(lines):
        if UNSAFE_PAT.search(line):
            # Check the 3 lines above (indices i-3 .. i-1).
            window = lines[max(0, i - 3) : i]
            if not any(SAFETY_PAT.search(w) for w in window):
                offenders.append(f"{path}:{i + 1}")
    return offenders


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: check-safety-comments.py <directory>", file=sys.stderr)
        return 2

    root = Path(sys.argv[1])
    all_offenders: list[str] = []

    for rs_file in sorted(root.rglob("*.rs")):
        # Skip generated / vendored directories.
        if any(part in SKIP_DIRS for part in rs_file.parts):
            continue
        all_offenders.extend(check_file(rs_file))

    if all_offenders:
        print("Missing SAFETY comments (within 3 lines above each unsafe block/fn):")
        for loc in all_offenders:
            print(f"  {loc}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
