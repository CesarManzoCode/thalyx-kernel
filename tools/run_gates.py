#!/usr/bin/env python3
"""Run every gate, K1 to K5, in order, on one platform, and record the verdicts.

A gate's verdict is a statement about a kernel *on a platform*. Every verdict
recorded before K6 was gathered on one platform -- QEMU q35 with TCG and an
explicit processor model -- and saying "K1 to K5 pass" without that qualifier
would be saying something nobody measured. This script runs the whole chain
the same way on each platform it knows, into a separate tree, so a verdict on
one platform can never be read as a verdict on another.

Platforms:

  tcg        the platform every verdict so far was gathered on. Its outputs go
             exactly where every tool already reads them (`build/run`,
             `build/k1-gate.json` and so on), so running it is running the
             canonical gates.
  kvm        the same images, the same processor model and the same machine,
             on the host processor's hardware virtualization. Guest
             instructions execute on the physical processor; the devices, the
             firmware and the interrupt controllers are still emulated. It is a
             different platform, not physical hardware, and it is recorded as
             such. Outputs under `build/platforms/kvm/`.
  kvm-host   KVM with the host's own processor model (`-cpu host`): the
             features of a real processor, including the ones this kernel must
             leave off -- AVX and XSAVE among them. Outputs under
             `build/platforms/kvm-host/`.

The gates run serially. K3's criterion that a seal is published against a
live writer depends on a real race, and a loaded host changes who wins it;
running two gates at once would be measuring the host.

Usage: tools/run_gates.py --platform kvm [--gates k1 k2 ...] [--no-self-test]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"

PLATFORMS = {
    "tcg": {"accel": "tcg", "cpu": None},
    "kvm": {"accel": "kvm", "cpu": None},
    "kvm-host": {"accel": "kvm", "cpu": "host"},
}
GATES = ["k1", "k2", "k3", "k4", "k5"]
EXPECTED = {"k1": 13, "k2": 21, "k3": 28, "k4": 31, "k5": 47}


def layout(platform: str) -> dict[str, Path]:
    """Where a platform's runs and verdicts live."""
    base = BUILD if platform == "tcg" else BUILD / "platforms" / platform
    return {
        "base": base,
        "k1_run": base / "run",
        "k2_run": base / "run-k2",
        "k3_run": base / "run-k3",
        "k4_cases": base / "k4-cases",
        "k5_runs": base / "k5-runs",
        "k5_cases": base / "k5-cases",
        "k1": base / "k1-gate.json",
        "k2": base / "k2-gate.json",
        "k3": base / "k3-gate.json",
        "k4": base / "k4-gate.json",
        "k5": base / "k5-gate.json",
        "logs": base / "gate-logs",
    }


def steps(platform: str, gates: list[str], self_test: bool) -> list[tuple[str, list[str]]]:
    p = layout(platform)
    py = sys.executable
    tool = lambda name: str(ROOT / "tools" / name)  # noqa: E731
    plan: list[tuple[str, list[str]]] = []
    if "k1" in gates:
        plan += [
            ("k1-build", [py, tool("build_image.py"), "--phase", "k1"]),
            ("k1-run", [py, tool("run_k1.py"), "--out", str(p["k1_run"])]),
            ("k1-gate", [py, tool("check_k1.py"), "--run", str(p["k1_run"]),
                         "--json", str(p["k1"])]),
        ]
    if "k2" in gates:
        common = ["--run", str(p["k2_run"]), "--k1", str(p["k1"])]
        plan += [
            ("k2-build", [py, tool("build_image.py"), "--phase", "k2"]),
            ("k2-run", [py, tool("run_k2.py"), "--out", str(p["k2_run"])]),
            ("k2-gate", [py, tool("check_k2.py"), *common, "--json", str(p["k2"])]),
        ]
        if self_test:
            plan.append(("k2-self-test", [py, tool("check_k2.py"), *common, "--self-test",
                                          "--quiet"]))
    if "k3" in gates:
        common = ["--run", str(p["k3_run"]), "--k1", str(p["k1"]), "--k2", str(p["k2"])]
        plan += [
            ("k3-build", [py, tool("build_image.py"), "--phase", "k3"]),
            ("k3-run", [py, tool("run_k3.py"), "--out", str(p["k3_run"])]),
            ("k3-gate", [py, tool("check_k3.py"), *common, "--json", str(p["k3"])]),
        ]
        if self_test:
            plan.append(("k3-self-test", [py, tool("check_k3.py"), *common, "--self-test",
                                          "--quiet"]))
    if "k4" in gates:
        common = ["--cases", str(p["k4_cases"]), "--k1", str(p["k1"]), "--k2", str(p["k2"]),
                  "--k3", str(p["k3"])]
        plan += [
            ("k4-build", [py, tool("build_image.py"), "--phase", "k4"]),
            ("k4-run", [py, tool("run_k4_cases.py"), "--out", str(p["k4_cases"])]),
            ("k4-gate", [py, tool("check_k4.py"), *common, "--json", str(p["k4"])]),
        ]
        if self_test:
            plan.append(("k4-self-test", [py, tool("check_k4.py"), *common, "--self-test",
                                          "--quiet"]))
    if "k5" in gates:
        common = ["--runs", str(p["k5_runs"]), "--cases", str(p["k5_cases"]),
                  "--k1", str(p["k1"]), "--k2", str(p["k2"]), "--k3", str(p["k3"]),
                  "--k4", str(p["k4"])]
        plan += [
            ("k5-stages", [py, tool("run_k5_stages.py"), "--out", str(p["k5_runs"])]),
            ("k5-cases", [py, tool("run_k5_cases.py"), "--out", str(p["k5_cases"])]),
            ("k5-gate", [py, tool("check_k5.py"), *common, "--json", str(p["k5"])]),
        ]
        if self_test:
            plan.append(("k5-self-test", [py, tool("check_k5.py"), *common, "--self-test",
                                          "--quiet"]))
    return plan


def summary_of(path: Path) -> dict | None:
    if not path.exists():
        return None
    verdict = json.loads(path.read_text())
    criteria = verdict.get("criteria", [])
    return {
        "passed": bool(verdict.get("passed")),
        "met": sum(1 for c in criteria if c.get("passed")),
        "total": len(criteria),
        "failed": [c["name"] for c in criteria if not c.get("passed")],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), default="tcg")
    parser.add_argument("--gates", nargs="*", choices=GATES, default=GATES)
    parser.add_argument("--no-self-test", action="store_true")
    arguments = parser.parse_args()

    platform = PLATFORMS[arguments.platform]
    environment = dict(os.environ)
    environment["THALYX_ACCEL"] = platform["accel"]
    environment.pop("THALYX_CPU", None)
    if platform["cpu"]:
        environment["THALYX_CPU"] = platform["cpu"]

    p = layout(arguments.platform)
    p["logs"].mkdir(parents=True, exist_ok=True)
    record: dict = {
        "platform": arguments.platform,
        "accelerator": platform["accel"],
        "cpu_override": platform["cpu"],
        "gates": arguments.gates,
        "steps": [],
    }
    failed_steps = []
    for name, argv in steps(arguments.platform, arguments.gates, not arguments.no_self_test):
        started = time.monotonic()
        log = p["logs"] / f"{name}.log"
        with log.open("w") as handle:
            result = subprocess.run(argv, cwd=ROOT, env=environment, stdout=handle,
                                    stderr=subprocess.STDOUT)
        seconds = round(time.monotonic() - started, 1)
        tail = log.read_text().strip().splitlines()[-1:] or [""]
        record["steps"].append({"step": name, "argv": argv, "status": result.returncode,
                                "seconds": seconds, "log": str(log.relative_to(ROOT)),
                                "last_line": tail[0]})
        print(f"{'ok ' if result.returncode == 0 else 'FAIL'} {name:14} {seconds:7.1f}s  "
              f"{tail[0][:100]}", flush=True)
        if result.returncode != 0:
            failed_steps.append(name)

    record["verdicts"] = {gate: summary_of(p[gate]) for gate in arguments.gates}
    record["expected"] = {gate: EXPECTED[gate] for gate in arguments.gates}
    record["passed"] = not failed_steps and all(
        (v := record["verdicts"][g]) is not None and v["passed"] and v["met"] == EXPECTED[g]
        for g in arguments.gates)
    (p["base"] / "platform-gates.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({"platform": arguments.platform, "passed": record["passed"],
                      "verdicts": record["verdicts"], "failed_steps": failed_steps}, indent=2))
    return 0 if record["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
