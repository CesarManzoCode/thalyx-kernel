#!/usr/bin/env python3
"""Run the K4 case matrix: one baseline and every cut the gate reasons about.

A case is a scenario, a fault directive and a number of legs, and each one gets
its own medium and its own output directory. Nothing here judges a case; it
runs them and writes down what each one did, so the gate reads runs rather than
this script's opinion of them.

The cases are chosen so that every point a publication can be cut at is cut at
least once, and so that the two cuts that matter most -- one where the commit
is durable and the checkpoint is not, and one where the prepare is durable and
the commit is not -- are both present. A matrix that only cut where recovery is
easy would be measuring the harness.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"

# (name, scenario, legs, [cut specs as run_k4.py takes them])
CASES = [
    # No directive at all: the vertical with nothing taken away from it.
    ("baseline", 1, 1, []),
    # A prepare is durable and its commit never happened. Recovery must resolve
    # it, and must resolve it as an abort: a prepare is not a promise.
    ("cut-after-prepare", 1, 2, ["1:AFTER_PREPARE:STOP:0:0"]),
    # The commit record is never issued and the service is told it was. The
    # run ends where the next flush would have been, because a suppressed write
    # the service believes in is only visible until a later compaction rewrites
    # the arena from memory and papers over it.
    ("drop-commit", 1, 2, ["1:BEFORE_COMMIT:DROP_WRITE:0:1"]),
    # The commit is durable and nothing after it is. Recovery must adopt the
    # version: this is the case where a version exists and the checkpoint that
    # would have named it does not.
    ("cut-after-commit", 1, 2, ["1:AFTER_COMMIT:STOP:0:0"]),
    # The checkpoint is written and the superblock still points at the old one.
    ("cut-after-checkpoint", 1, 2, ["1:AFTER_CHECKPOINT:STOP:0:0"]),
    # Everything is durable and the caller was never told. The retry has to be
    # answered from the durable result rather than done again.
    ("cut-after-superblock", 1, 2, ["1:AFTER_SUPERBLOCK:LOSE_RESPONSE:0:0"]),
    # An object record is torn across the sector boundary: the first sector
    # reaches the medium and the rest does not. The ordinal names the version's
    # content rather than its manifest or its tree, because those fit inside one
    # sector and a record inside one sector cannot be observed being torn.
    ("tear-object", 1, 2, ["1:BEFORE_OBJECT:TEAR_WRITE:5:0"]),
    # The medium refuses a write outright, before the commit.
    ("io-error-commit", 1, 2, ["1:BEFORE_COMMIT:IO_ERROR:0:0"]),
    # A write is held back and issued after the one that followed it, and the
    # run ends where the flush would have been so the reordering is visible.
    ("reorder-commit", 1, 2, ["1:BEFORE_COMMIT:REORDER:0:1"]),
    # The broker answered and the record of what it said never became durable.
    ("cut-after-outbox", 1, 2, ["1:AFTER_OUTBOX_SEND:STOP:0:0"]),
    # The service is replaced inside one run rather than between legs.
    ("kill-service", 1, 1, ["1:AFTER_PREPARE:KILL_SERVICE:0:0"]),
    # Two publishers asking for the same transition. One of them loses.
    ("rival", 2, 1, []),
    # The broker cannot say what became of an intent. `UNKNOWN` is a result.
    ("broker-unknown", 3, 1, []),
    # The same cut as `cut-after-prepare`, and then the recovery leg is cut too,
    # at the first thing it tries to write. What that leaves on the medium is
    # what recovery itself wrote and nothing after it, which is the only way to
    # look at an abort record: a leg that runs to the end compacts the arena
    # and the resolution is carried forward in a checkpoint instead.
    ("abort-visible", 1, 2, ["1:AFTER_PREPARE:STOP", "2:BEFORE_PREPARE:STOP"]),
    # The control plane is lost: the auditor stops reading and the log fills,
    # with a preparation durable. A full log refuses the admissions its
    # receipts would have covered, so what this case has to show is a
    # publication refused for a reason it can name, and a caller told that
    # nobody can say -- not a version that happened with nobody able to
    # account for it. The fill is a directive at a point, like every other
    # cut: filled from the supervisor against clients that were consuming the
    # log at the same time, which admission met the full log was a race.
    ("control-lost", 5, 1, ["1:AFTER_PREPARE:LOSE_CONTROL:0:0"]),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=BUILD / "k4-cases")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--only", nargs="*", help="run only these cases by name")
    arguments = parser.parse_args()

    arguments.out.mkdir(parents=True, exist_ok=True)
    index = []
    failed = []
    for name, scenario, legs, cuts in CASES:
        if arguments.only and name not in arguments.only:
            continue
        directory = arguments.out / name
        argv = [
            sys.executable,
            str(ROOT / "tools/run_k4.py"),
            "--out", str(directory),
            "--legs", str(legs),
            "--scenario", str(scenario),
            "--timeout", str(arguments.timeout),
        ]
        for cut in cuts:
            argv += ["--cut", cut]
        print(f"=== {name} ===", file=sys.stderr)
        result = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)
        record = directory / "run.json"
        entry = {
            "case": name,
            "scenario": scenario,
            "legs": legs,
            "cuts": cuts,
            "directory": str(directory.relative_to(ROOT)),
            "harness_status": result.returncode,
        }
        if record.exists():
            entry["run"] = json.loads(record.read_text())
        else:
            entry["stderr"] = result.stderr[-2000:]
            failed.append(name)
        index.append(entry)
        legs_run = entry.get("run", {}).get("legs", [])
        for leg in legs_run:
            print(
                f"    leg {leg['leg']}: exit={leg['exit_status']} "
                f"timed_out={leg['timed_out']} {leg['wall_seconds']}s",
                file=sys.stderr,
            )

    (arguments.out / "cases.json").write_text(json.dumps(index, indent=2) + "\n")
    print(f"wrote {arguments.out / 'cases.json'}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
