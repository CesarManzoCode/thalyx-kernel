#!/usr/bin/env python3
"""Check that both sides of the K5 protocols still match the schema.

Regenerates from `abi/schema/k5-proto-v1.json` and compares with what is
committed. A file that drifted from the schema fails here rather than at three
in the morning inside a guest, and a schema change that was not regenerated
fails the same way.

Usage: tools/check_k5_proto.py
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_k5_proto as gen  # noqa: E402


def main() -> int:
    failures = []
    for path, expected in gen.outputs().items():
        if not path.exists():
            failures.append(f"{path.relative_to(ROOT)} is missing")
            continue
        if path.read_text() != expected:
            failures.append(f"{path.relative_to(ROOT)} differs from the schema")
    for line in failures:
        print(f"FAIL  {line}")
    if failures:
        print(f"\nK5 PROTOCOL CHECK FAILED: {len(failures)} generated file(s) out of date")
        return 1
    print("PASS  generated Rust and C match abi/schema/k5-proto-v1.json")
    print("\nK5 PROTOCOL CHECK PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
