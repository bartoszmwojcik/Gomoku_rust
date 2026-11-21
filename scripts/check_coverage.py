#!/usr/bin/env python3
"""Per-crate coverage gates over a `cargo llvm-cov --json --summary-only` report."""
import json
import re
import sys

# crate -> (minimum line %, minimum function % or None)
GATES = {
    "gomoku-core": (100.0, 100.0),
    "gomoku-db": (85.0, None),
    "gomoku-web": (85.0, None),
}

report = json.load(open(sys.argv[1]))
files = report["data"][0]["files"]
totals = {}
for entry in files:
    match = re.search(r"/crates/([^/]+)/(src|tests)/", entry["filename"])
    if not match or match.group(2) != "src":
        continue
    crate = match.group(1)
    t = totals.setdefault(crate, {"lines": [0, 0], "functions": [0, 0]})
    for kind in ("lines", "functions"):
        t[kind][0] += entry["summary"][kind]["covered"]
        t[kind][1] += entry["summary"][kind]["count"]

failed = False
for crate, (min_lines, min_functions) in GATES.items():
    t = totals.get(crate)
    if t is None:
        print(f"{crate}: no coverage data", file=sys.stderr)
        failed = True
        continue
    lines = 100.0 * t["lines"][0] / max(1, t["lines"][1])
    functions = 100.0 * t["functions"][0] / max(1, t["functions"][1])
    problems = []
    if lines < min_lines:
        problems.append(f"lines {lines:.2f}% < {min_lines}%")
    if min_functions is not None and functions < min_functions:
        problems.append(f"functions {functions:.2f}% < {min_functions}%")
    status = "FAIL " + "; ".join(problems) if problems else "ok"
    print(f"{crate}: lines {lines:.2f}% ({t['lines'][0]}/{t['lines'][1]}), functions {functions:.2f}% -> {status}")
    failed |= bool(problems)
sys.exit(1 if failed else 0)
