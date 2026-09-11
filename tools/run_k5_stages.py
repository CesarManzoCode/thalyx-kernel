#!/usr/bin/env python3
"""Build and run every stage of the K5 port, one directory per run.

A stage is a package: the same kernel, the same supervisor image, a different
set of programs and a different plan module. Running them from one script keeps
"the same kernel ran all of it" a property of the evidence rather than of
somebody's memory.

Usage: tools/run_k5_stages.py [--stages smoke ...] [--out build/k5-runs]
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"

STAGES = ["smoke", "surface", "work", "engine"]
SEEDS = {"smoke": 0x5EED0001, "surface": 0x5EED0002, "work": 0x5EED0003, "engine": 0x5EED0004}
NEEDS_MEDIUM = {"smoke": False, "surface": True, "work": True, "engine": True}

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_reference  # noqa: E402


def run(argv: list[str]) -> None:
    print("+", " ".join(argv), file=sys.stderr)
    result = subprocess.run(argv, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(f"failed: {' '.join(argv)}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stages", nargs="*", default=STAGES)
    parser.add_argument("--out", type=Path, default=BUILD / "k5-runs")
    parser.add_argument("--timeout", type=float, default=900.0)
    arguments = parser.parse_args()

    arguments.out.mkdir(parents=True, exist_ok=True)
    summary = []
    for stage in arguments.stages:
        if stage not in STAGES:
            print(f"unknown stage: {stage}", file=sys.stderr)
            return 1
        run([
            sys.executable, str(ROOT / "tools/build_image.py"),
            "--phase", "k5", "--stage", stage, "--seed", hex(SEEDS[stage]),
        ])
        out = arguments.out / stage
        if out.exists():
            shutil.rmtree(out)
        argv = [
            sys.executable, str(ROOT / "tools/run_k5.py"),
            "--out", str(out), "--timeout", str(arguments.timeout),
        ]
        if NEEDS_MEDIUM[stage]:
            argv += ["--medium", str(out / "medium.img"), "--fresh-medium"]
        run(argv)
        if stage == "engine":
            # The other side of the comparison, on the host and labelled so:
            # Thalyx's own engine, unchanged, on the same model and prompts.
            cases = []
            for case in run_reference.fixture_cases():
                for prompt in case["prompts"]:
                    cases += ["--case", prompt, str(case["predict"])]
            run([sys.executable, str(ROOT / "tools/run_reference.py"), *cases,
                 "--out", str(out / "reference.json")])
        record = json.loads((out / "run.json").read_text())
        summary.append({"stage": stage, "exit_status": record["exit_status"],
                        "wall_seconds": record["wall_seconds"]})
    (arguments.out / "stages.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
