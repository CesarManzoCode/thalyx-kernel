#!/usr/bin/env python3
"""Run K6's paired benchmarks: this kernel and Linux, one virtual machine.

A campaign is a number of rounds. A round is one boot of every arm:

  native          this kernel, the K6 package: tests/k6/bench.c on the native
                  target, K6's supervisor, K5's engine.
  linux           the host's own Linux kernel as a guest of the same machine,
                  running the same bench.c over Linux primitives, with the
                  mitigations it enables by default.
  linux-nomitig   the same guest booted with `mitigations=off`. This kernel
                  implements no speculative-execution mitigation, so the Linux
                  it is compared with is also run without them: a comparison
                  has to survive the rival's best argument, and "the other side
                  pays for mitigations yours does not" is that argument.

Every arm of a round runs the same plan: the suite's entries in an order drawn
at random for that round, from the campaign seed, minus the entries the schema
declares native-only on the Linux arms. The order the arms boot in is drawn at
random per round too. The pairing unit is the round, and the statistics in
tools/k6_analysis.py are built on that.

Every boot is the same QEMU machine -- q35, one processor model, four
processors, 1024 MiB, OVMF -- on the same accelerator, and the QEMU process is
confined to the same host processors, one per physical core, so the SMT
sibling of a guest processor is never another guest processor. What each boot
ran is kept: the plan, the image or initramfs digest, the command line, the raw
log.

Usage:
  tools/run_k6.py --label pilot --rounds 2 --scale 0.5
  tools/run_k6.py --label baseline --rounds 6 [--sizes build/k6/campaigns/pilot/sizes.json]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
CAMPAIGNS = BUILD / "k6" / "campaigns"
LINUX = BUILD / "k6" / "linux"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_k6_linux  # noqa: E402
import k6_analysis as analysis  # noqa: E402
import k6_plan  # noqa: E402
import toolchain as tc  # noqa: E402

CPU_MODEL = "qemu64,+smep,+smap,+pdpe1gb,+x2apic"
PROCESSORS = 4
MEMORY = "1024M"
# One host thread per physical core, core 0 left to the host: siblings are
# (0,6) (1,7) ... (5,11) on the development machine, and the inventory records
# the topology this was chosen against.
PIN = "1-5"

# (benchmark, parameter, samples, unrecorded warm-up). Thirty-two entries, the
# most a plan holds, and every benchmark and parameter the schema lists.
SUITE = [
    ("entry.version", 0, 2000, 200),
    ("entry.clock", 0, 2000, 200),
    ("ipc.call", 0, 1000, 100),
    ("ipc.call", 64, 1000, 100),
    ("ipc.call", 256, 1000, 100),
    ("ipc.caps", 1, 500, 50),
    ("ipc.caps", 4, 500, 50),
    ("mem.map", 1, 500, 20),
    ("mem.map", 64, 200, 10),
    ("mem.seal", 16, 200, 10),
    ("cap.derive", 0, 2000, 200),
    ("sched.wake", 0, 300, 0),
    ("quota.share", 25, 100, 0),
    ("scale.compute", 1, 50, 0),
    ("scale.compute", 2, 50, 0),
    ("scale.compute", 3, 50, 0),
    ("scale.compute", 4, 50, 0),
    ("scale.ipc", 1, 50, 0),
    ("scale.ipc", 2, 50, 0),
    ("scale.ipc", 3, 50, 0),
    ("scale.ipc", 4, 50, 0),
    ("closure.unit", 0, 40, 0),
    ("engine.load", 0, 1, 0),
    ("engine.infer", 0, 30, 0),
    ("engine.infer", 1, 30, 0),
    ("engine.infer", 4, 5, 0),
    ("engine.cancel", 0, 3, 0),
    ("ipc.lineage", 0, 1000, 100),
    ("ipc.lineage", 8, 1000, 100),
    ("ipc.lineage", 16, 1000, 100),
    ("ipc.lineage", 28, 1000, 100),
    ("audit.drain", 0, 4000, 0),
]

# Entries whose sample count is a property of the benchmark, not a precision
# target: a slice count, a single load, a handful of cancellations.
FIXED = {"scale.compute", "scale.ipc", "quota.share", "engine.load", "engine.cancel",
         "audit.drain"}

ARMS = {
    "native": {"kind": "native"},
    "linux": {"kind": "linux", "cmdline": "console=ttyS0 quiet loglevel=1 panic=-1"},
    "linux-nomitig": {"kind": "linux",
                      "cmdline": "console=ttyS0 quiet loglevel=1 panic=-1 mitigations=off"},
}

SUMMARY_RE = re.compile(r"^THLX1 kernel \d+ \S+ (scope\.accounting|sched\.cpu_summary|"
                        r"sched\.summary|diag\.summary|k1\.summary|k1\.terminal) (.*)$")
KV_RE = re.compile(r"(\w+)=(\S+)")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def native_only() -> set[str]:
    return {bench["name"] for bench in analysis.schema()["benchmarks"]
            if bench["equivalence"] == "native_only"}


def suite(scale: float, sizes: dict) -> list[tuple[str, int, int, int]]:
    out = []
    for name, param, samples, warmup in SUITE:
        key = f"{name}:{param}"
        if name not in FIXED:
            if key in sizes:
                samples = sizes[key]
            samples = max(10, int(samples * scale))
        out.append((name, param, min(samples, 16384), warmup))
    return out


def plan_entries(entries, round_: int, seed: int, arm: str) -> list:
    rng = random.Random(seed * 1_000_003 + round_)
    order = list(entries)
    rng.shuffle(order)
    # The auditor reports what draining cost over everything before it, so it
    # runs last wherever the shuffle put it.
    order.sort(key=lambda entry: entry[0] == "audit.drain")
    if ARMS[arm]["kind"] == "linux":
        skip = native_only()
        order = [entry for entry in order if entry[0] not in skip]
    return order


def qemu_base(tools: tc.Toolchain, vars_copy: Path) -> list[str]:
    argv = [
        "taskset", "-c", PIN,
        str(tools.qemu),
        "-machine", tc.machine(),
        "-cpu", tc.cpu_model(CPU_MODEL),
        "-smp", str(PROCESSORS),
        "-m", MEMORY,
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={tools.ovmf_code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy}",
        "-display", "none",
        "-no-reboot",
    ]
    if tools.prefix is not None and (tools.prefix / "usr/share/qemu").is_dir():
        argv += ["-L", str(tools.prefix / "usr/share/qemu")]
    return argv


def run_qemu(tools: tc.Toolchain, argv: list[str], timeout: float) -> tuple[int | None, str, str, float]:
    started = time.monotonic()
    try:
        result = tools.run(argv, capture_output=True, text=True, timeout=timeout)
        return result.returncode, result.stdout, result.stderr, time.monotonic() - started
    except subprocess.TimeoutExpired as expired:
        out = expired.stdout.decode("utf-8", "replace") if expired.stdout else ""
        err = expired.stderr.decode("utf-8", "replace") if expired.stderr else ""
        return None, out, err, time.monotonic() - started


def boot_native(tools, directory: Path, plan: bytes, timeout: float) -> dict:
    (directory / "plan.bin").write_bytes(plan)
    build = subprocess.run([sys.executable, str(ROOT / "tools/build_image.py"), "--phase", "k6",
                            "--plan", str(directory / "plan.bin")],
                           cwd=ROOT, capture_output=True, text=True)
    if build.returncode != 0:
        (directory / "build.log").write_text(build.stdout + build.stderr)
        raise SystemExit(f"native image build failed; see {directory / 'build.log'}")
    image = directory / "thalyx-k6.img"
    shutil.copyfile(BUILD / "thalyx-k6.img", image)
    manifest = json.loads((BUILD / "image-manifest-k6.json").read_text())
    vars_copy = directory / "OVMF_VARS.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)
    argv = qemu_base(tools, vars_copy) + [
        "-drive", f"format=raw,file={image}",
        "-serial", "stdio",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
    ]
    status, out, err, wall = run_qemu(tools, argv, timeout)
    (directory / "serial.log").write_text(out)
    (directory / "qemu-stderr.log").write_text(err)
    return {"argv": argv, "exit_status": status, "wall_seconds": round(wall, 3),
            "image_sha256": sha256(image), "kernel_sha256": manifest["artifacts"]["kernel"]["sha256"],
            "modules": manifest["modules"], "log": "serial.log"}


def boot_linux(tools, directory: Path, plan: bytes, arm: str, timeout: float) -> dict:
    plan_cpio = build_k6_linux.plan_archive(plan, directory / "plan.cpio")
    initrd = directory / "initrd.img"
    initrd.write_bytes((LINUX / "base.cpio").read_bytes() + plan_cpio.read_bytes())
    vars_copy = directory / "OVMF_VARS.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)
    debugcon = directory / "debugcon.log"
    argv = qemu_base(tools, vars_copy) + [
        "-kernel", str(LINUX / "vmlinuz"),
        "-initrd", str(initrd),
        "-append", ARMS[arm]["cmdline"],
        "-serial", "stdio",
        "-chardev", f"file,id=k6con,path={debugcon}",
        "-device", "isa-debugcon,chardev=k6con,iobase=0xe9",
    ]
    status, out, err, wall = run_qemu(tools, argv, timeout)
    (directory / "serial.log").write_text(out)
    (directory / "qemu-stderr.log").write_text(err)
    return {"argv": argv, "exit_status": status, "wall_seconds": round(wall, 3),
            "initrd_sha256": sha256(initrd), "kernel_sha256": sha256(LINUX / "vmlinuz"),
            "log": "debugcon.log"}


def kernel_summaries(text: str) -> dict:
    """The native kernel's own end-of-run accounting, for attributing costs."""
    out: dict = {"scope_accounting": [], "cpu_summary": []}
    for line in text.splitlines():
        match = SUMMARY_RE.match(line)
        if not match:
            continue
        fields = {key: analysis._number(value) for key, value in KV_RE.findall(match.group(2))}
        name = match.group(1)
        if name == "scope.accounting":
            out["scope_accounting"].append(fields)
        elif name == "sched.cpu_summary":
            out["cpu_summary"].append(fields)
        else:
            out[name.replace(".", "_")] = fields
    return out


def parse_boot(arm: str, directory: Path) -> tuple[dict, dict]:
    if ARMS[arm]["kind"] == "native":
        text = (directory / "serial.log").read_text()
        stream, diag = analysis.native_stream(text)
        extra = {"diag": diag, "kernel": kernel_summaries(text)}
    else:
        log = directory / "debugcon.log"
        text = log.read_text() if log.exists() else ""
        stream, info = analysis.linux_stream(text)
        extra = {"guest": info}
    return analysis.entries_of(stream), extra


def git_revision() -> dict:
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True,
                          text=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True,
                           text=True).stdout.splitlines()
    return {"head": head, "uncommitted_paths": len(dirty)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--label", required=True)
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument("--seed", type=lambda text: int(text, 0), default=0x6B6)
    parser.add_argument("--arms", nargs="*", default=list(ARMS), choices=list(ARMS))
    parser.add_argument("--scale", type=float, default=1.0)
    parser.add_argument("--sizes", type=Path, default=None,
                        help="samples per entry, as a pilot's sizes.json recommends")
    parser.add_argument("--timeout", type=float, default=1500.0)
    parser.add_argument("--only", nargs="*", default=None, help="run only these benchmark names")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1
    if tc.accelerator() != "kvm":
        print("warning: a timing campaign under TCG measures the emulator, not a processor",
              file=sys.stderr)

    out = CAMPAIGNS / arguments.label
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    if any(ARMS[arm]["kind"] == "linux" for arm in arguments.arms):
        if not (LINUX / "base.cpio").exists():
            subprocess.run([sys.executable, str(ROOT / "tools/build_k6_linux.py")], cwd=ROOT,
                           check=True)
    subprocess.run([sys.executable, str(ROOT / "tools/inventory_host.py"), "--out",
                    str(out / "host-inventory.json")], cwd=ROOT, check=True,
                   capture_output=True)

    sizes = json.loads(arguments.sizes.read_text()) if arguments.sizes else {}
    entries = suite(arguments.scale, sizes)
    if arguments.only:
        entries = [entry for entry in entries if entry[0] in arguments.only]
    model_bytes = (BUILD / "reference/tiny.gguf").stat().st_size
    campaign = {
        "label": arguments.label,
        "rounds": arguments.rounds,
        "seed": arguments.seed,
        "arms": arguments.arms,
        "accelerator": tc.accelerator(),
        "cpu": tc.cpu_model(CPU_MODEL),
        "processors": PROCESSORS,
        "memory": MEMORY,
        "pin": PIN,
        "suite": entries,
        "revision": git_revision(),
        "toolchain": tools.describe(),
        "linux_manifest": json.loads((LINUX / "linux-manifest.json").read_text())
        if (LINUX / "linux-manifest.json").exists() else None,
        "boots": [],
    }
    runs = []
    order_rng = random.Random(arguments.seed ^ 0xA5A5)
    for round_ in range(1, arguments.rounds + 1):
        arms = list(arguments.arms)
        order_rng.shuffle(arms)
        for arm in arms:
            directory = out / f"r{round_:02d}-{arm}"
            directory.mkdir()
            ordered = plan_entries(entries, round_, arguments.seed, arm)
            plan = k6_plan.pack(ordered, arguments.seed * 1000 + round_, round_, model_bytes)
            load = Path("/proc/loadavg").read_text().split()[:3]
            if ARMS[arm]["kind"] == "native":
                record = boot_native(tools, directory, plan, arguments.timeout)
            else:
                record = boot_linux(tools, directory, plan, arm, arguments.timeout)
            parsed, extra = parse_boot(arm, directory)
            record.update({"arm": arm, "round": round_, "directory": str(directory.relative_to(ROOT)),
                           "plan_sha256": hashlib.sha256(plan).hexdigest(),
                           "entries_planned": len(ordered), "entries_reported": len(parsed["entries"]),
                           "done": parsed["done"], "load_before": load})
            campaign["boots"].append(record)
            runs.append({"backend": arm, "round": round_, "parsed": parsed, "extra": extra})
            print(f"round {round_} {arm:14} exit={record['exit_status']} "
                  f"{record['wall_seconds']:7.1f}s entries={len(parsed['entries'])}/{len(ordered)}",
                  flush=True)
            (out / "campaign.json").write_text(json.dumps(campaign, indent=2) + "\n")

    samples = [{"arm": run["backend"], "round": run["round"], "tsc_hz": run["parsed"]["tsc_hz"],
                "seed": run["parsed"]["seed"], "done": run["parsed"]["done"],
                "entries": [{k: e[k] for k in ("bench", "param", "requested", "samples", "lost",
                                               "errors", "error_status", "aux", "digest", "check",
                                               "mismatch", "sources")}
                            for e in run["parsed"]["entries"]],
                "extra": run["extra"]} for run in runs]
    (out / "samples.json").write_text(json.dumps(samples) + "\n")
    results = analysis.analyze(runs)
    results["campaign"] = {k: campaign[k] for k in ("label", "rounds", "seed", "arms", "accelerator",
                                                    "cpu", "processors", "memory", "pin", "revision")}
    results["sizes"] = analysis.plan_sizes(runs)
    (out / "results.json").write_text(json.dumps(results, indent=2, default=str) + "\n")
    (out / "sizes.json").write_text(json.dumps(
        {key: value["samples_needed"] for key, value in results["sizes"].items()}, indent=2) + "\n")
    for item in results["results"]:
        line = [f"{item['bench']}:{item['param']}"]
        for arm, stats in item["backends"].items():
            median = stats["pooled"].get("median")
            line.append(f"{arm}={median:.4g}{stats['unit']}" if median is not None else f"{arm}=-")
        for comparison in item["comparisons"]:
            line.append(f"{comparison['against']}:{comparison['ratio_native_over_other']:.3g} "
                        f"{comparison['verdict']}")
        print("  ".join(line))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
