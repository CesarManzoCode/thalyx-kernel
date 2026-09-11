#!/usr/bin/env python3
"""Run the K5 case matrix: what EXP-10 asks of the port under adversity.

The `engine` stage is the vertical with everything in it and nothing done to
it. A case is that same image -- the same kernel, the same supervisor, the same
runtime, tool and resident engine -- with a scenario and, when the case says
so, a cut. The scenario travels in the medium's directive block exactly as
K4's did, so the supervisor reads which works to build from the same place the
state service reads where to fail, and the host writes both.

The cases, and why each is here:

  rivals             two works over one version, both asking the same engine,
                     both publishing against the same generation. One is
                     refused with GENERATION_STALE and starts the vertical
                     again over the version that won. Two principals, two
                     prompt buffers, two workspaces, one engine.
  cancel             a work asks the engine for more tokens than anyone will
                     wait for, and its scope is closed while the engine
                     computes for it. The engine notices between tokens, stops,
                     answers CANCELLED, and stays resident; an unrelated work
                     then uses it and publishes.
  cut-after-prepare  the run is cut with the work's publication prepared and
                     not committed. Recovery aborts it; the work, on the next
                     boot, finds its spent request identities, sees its change
                     is not published, and does the whole vertical again --
                     runtime, engine, tool -- before publishing.
  cut-after-commit   the run is cut with the commit durable and nothing after
                     it. Recovery adopts the version; the work finds its change
                     already published and does not publish it twice.
  io-error-commit    the medium refuses the commit's write. The service
                     refuses the publication rather than assuming it, the work
                     records the refusal and abandons, and the next boot
                     publishes.

The fault argument on a cut is `1`: the second publication of the run, because
the first is the seed version the work publishes before it does any work, and a
cut in the seed would be a cut in the fixture rather than in the vertical.

Nothing here judges a case. Each gets its own medium and its own directory,
the Linux reference is asked the same prompts the fixture names, and the gate
reads what the kernel and the medium recorded.

Usage: tools/run_k5_cases.py [--only CASE ...] [--out build/k5-cases]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import k4_format as fmt  # noqa: E402
import run_k1  # noqa: E402
import run_k4  # noqa: E402
import run_k5  # noqa: E402
import run_reference  # noqa: E402
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
IMAGE = BUILD / "thalyx-k5.img"

# (name, scenario, seed, legs, [cut specs as run_k4.py takes them])
CASES = [
    ("rivals", 2, 0x5EED0102, 1, []),
    ("cancel", 3, 0x5EED0103, 1, []),
    ("cut-after-prepare", 1, 0x5EED0104, 2, ["1:AFTER_PREPARE:STOP:1:0"]),
    ("cut-after-commit", 1, 0x5EED0105, 2, ["1:AFTER_COMMIT:STOP:1:0"]),
    ("io-error-commit", 1, 0x5EED0106, 2, ["1:BEFORE_COMMIT:IO_ERROR:1:0"]),
]


def run(argv: list[str]) -> None:
    print("+", " ".join(argv), file=sys.stderr)
    result = subprocess.run(argv, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(f"failed: {' '.join(argv)}")


def parse_cut(spec: str) -> tuple[int, tuple[int, int, int, int]]:
    parts = spec.split(":")
    if len(parts) < 3:
        raise SystemExit(f"a cut wants LEG:POINT:MODE[:ARG[:FLUSH]], got {spec!r}")
    return int(parts[0]), (
        run_k4.named(fmt.FAULTPOINT, parts[1]),
        run_k4.named(fmt.FAULTMODE, parts[2]),
        int(parts[3]) if len(parts) > 3 else 0,
        int(parts[4]) if len(parts) > 4 else 0,
    )


def one_case(tools: tc.Toolchain, out: Path, name: str, scenario: int, seed: int,
             legs: int, cuts: list[str], timeout: float) -> dict:
    # The image for this case: the engine stage with this case's seed in its
    # plan. A case cannot precompute a run it does not know the seed of.
    run([sys.executable, str(ROOT / "tools/build_image.py"),
         "--phase", "k5", "--stage", "engine", "--seed", hex(seed)])
    manifest = json.loads((BUILD / "image-manifest-k5.json").read_text())
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    image = out / "thalyx-k5.img"
    shutil.copyfile(IMAGE, image)

    medium = out / "medium.img"
    run_k4.blank_medium(medium)
    cut_by_leg = dict(parse_cut(spec) for spec in cuts)
    directives = []
    leg_records = []
    for leg in range(1, legs + 1):
        cut = cut_by_leg.get(leg, (fmt.FAULTPOINT["NONE"], fmt.FAULTMODE["NONE"], 0, 0))
        directives.append(run_k4.write_directive(
            medium, cut[0], cut[1], cut[2], leg, scenario, seed, cut[3]))
        leg_records.append(run_k4.one_leg(
            tools, out, image, medium, leg, run_k5.PROCESSORS, timeout, True, run_k5.MEMORY))
    record = {
        "case": name,
        "scenario": scenario,
        "seed": seed,
        "cuts": cuts,
        "cpu": tc.cpu_model(run_k5.CPU_MODEL),
        "processors": run_k5.PROCESSORS,
        "memory": run_k5.MEMORY,
        "machine": "q35",
        "accelerator": tc.accelerator(),
        "image": str(image),
        "image_sha256": hashlib.sha256(image.read_bytes()).hexdigest(),
        "image_manifest": manifest,
        "medium": str(medium),
        "directives": directives,
        "legs": leg_records,
        "expected_exit_complete": run_k1.EXIT_COMPLETE,
        "expected_exit_panic": run_k1.EXIT_PANIC,
        "toolchain": tools.describe(),
    }
    (out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=BUILD / "k5-cases")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument("--only", nargs="*", help="run only these cases by name")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    arguments.out.mkdir(parents=True, exist_ok=True)
    index = []
    for name, scenario, seed, legs, cuts in CASES:
        if arguments.only and name not in arguments.only:
            continue
        print(f"=== {name} ===", file=sys.stderr)
        record = one_case(tools, arguments.out / name, name, scenario, seed, legs, cuts,
                          arguments.timeout)
        for leg in record["legs"]:
            print(f"    leg {leg['leg']}: exit={leg['exit_status']} "
                  f"timed_out={leg['timed_out']} {leg['wall_seconds']}s", file=sys.stderr)
        index.append({k: record[k] for k in ("case", "scenario", "seed", "cuts")}
                     | {"directory": str((arguments.out / name).relative_to(ROOT)),
                        "legs": [{k: leg[k] for k in ("leg", "exit_status", "timed_out",
                                                     "wall_seconds")}
                                 for leg in record["legs"]]})

    # The other side of the comparison, once for the matrix: Thalyx's own
    # engine, unchanged, on the host, asked every prompt the fixture names.
    cases = []
    for case in run_reference.fixture_cases():
        for prompt in case["prompts"]:
            cases += ["--case", prompt, str(case["predict"])]
    run([sys.executable, str(ROOT / "tools/run_reference.py"), *cases,
         "--out", str(arguments.out / "reference.json")])

    (arguments.out / "cases.json").write_text(json.dumps(index, indent=2) + "\n")
    print(json.dumps(index, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
