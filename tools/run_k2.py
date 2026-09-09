#!/usr/bin/env python3
"""Run the K2 image under QEMU and capture the diagnostic plane.

The machine, the CPU model, the memory size and the firmware are the ones
`run_k1.py` defines, and this reuses them rather than restating them. If the two
phases ran on machines that differed in any of those, "K1 still passes on the
kernel K2 grew into" would be a comparison between two environments as well as
two packages, and the interesting half would be unattributable.

Like the K1 runner, nothing here interprets the serial log: the gate reads the
kernel's own records, not this script's summary of them.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_k1  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
IMAGE = BUILD / "thalyx-k2.img"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, default=IMAGE)
    parser.add_argument("--out", type=Path, default=BUILD / "run-k2")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--no-debug-exit", action="store_true")
    arguments = parser.parse_args()

    argv = [
        "--image",
        str(arguments.image),
        "--out",
        str(arguments.out),
        "--timeout",
        str(arguments.timeout),
    ]
    if arguments.no_debug_exit:
        argv.append("--no-debug-exit")
    sys.argv = [sys.argv[0], *argv]
    return run_k1.main()


if __name__ == "__main__":
    raise SystemExit(main())
