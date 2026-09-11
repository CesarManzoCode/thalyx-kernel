#!/usr/bin/env python3
"""Check that the K6 benchmark schema is usable and both sides still match it.

Two things are checked. The schema itself: every benchmark has a family, a
declared equivalence, something to run, and a description of each side it
claims to compare -- a comparison that did not say what the Linux side does, or
a `comparable` pair that did not list what makes it only comparable, would be a
judge without a rule. And the generated files: regenerated from the schema and
compared with what is committed, so a drifted header fails here rather than
inside a guest.

Usage: tools/check_k6_bench.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_k6_bench as gen  # noqa: E402


def main() -> int:
    failures = [f"schema: {line}" for line in gen.problems(json.loads(gen.SCHEMA.read_text()))]
    for path, expected in gen.outputs().items():
        if not path.exists():
            failures.append(f"{path.relative_to(ROOT)} is missing")
        elif path.read_text() != expected:
            failures.append(f"{path.relative_to(ROOT)} differs from the schema")
    for line in failures:
        print(f"FAIL  {line}")
    if failures:
        print(f"\nK6 BENCHMARK SCHEMA CHECK FAILED: {len(failures)} problem(s)")
        return 1
    print("PASS  schema well formed; generated C and Rust match abi/schema/k6-bench-v1.json")
    print("\nK6 BENCHMARK SCHEMA CHECK PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
