#!/usr/bin/env python3
"""Run the K3 image under QEMU with several processors and a block device.

Two things differ from the earlier runners, and both are the point of the
phase. The machine has four processors instead of one, so the scheduling,
invalidation and locking paths are exercised by processors that are actually
running at the same time rather than by one processor pretending. And it has a
modern virtio block device, so the device path has a device.

The processor model adds x2APIC to the one `run_k1.py` fixes. That is not a
cosmetic difference: x2APIC is a different register interface with a different
interrupt command register and different ordering rules, and a kernel that only
ever ran on the memory-mapped interface would not have exercised the fence its
model-specific-register path needs. The earlier phases keep their model, so
their evidence stays a statement about the same environment it was gathered in.

`--profile dmar` adds an emulated remapping unit that the kernel does *not*
program. That configuration exists to be refused: it is the difference between
"the firmware describes a remapping unit" and "a device is isolated", and a run
that could not tell them apart would be the wrong kind of evidence.

Nothing here interprets the serial log; the gate reads the kernel's own records.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_k1  # noqa: E402
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
IMAGE = BUILD / "thalyx-k3.img"

# The K1 model plus x2APIC, so both interrupt-controller interfaces are covered
# by the evidence base as a whole rather than assumed equivalent.
CPU_MODEL = run_k1.CPU_MODEL + ",+x2apic"
MEMORY = run_k1.MEMORY
PROCESSORS = 4
# A small scratch medium. The driver writes one block of it; nothing in this
# phase claims anything durable about what happens to those bytes.
DISK_MIB = 4


def scratch_disk(path: Path) -> None:
    """Creates a fresh zeroed medium, so a run never inherits the last one's."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as handle:
        handle.truncate(DISK_MIB * 1024 * 1024)


def qemu_command(
    tools: tc.Toolchain,
    vars_copy: Path,
    image: Path,
    disk: Path,
    processors: int,
    profile: str,
    debug_exit: bool,
) -> list[str]:
    argv = [
        str(tools.qemu),
        "-machine", "q35,accel=tcg",
        "-cpu", CPU_MODEL,
        "-smp", str(processors),
        "-m", MEMORY,
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={tools.ovmf_code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy}",
        "-drive", f"format=raw,file={image}",
        "-drive", f"if=none,id=k3disk,format=raw,cache=writeback,file={disk}",
        "-device", "virtio-blk-pci,drive=k3disk,disable-legacy=on,disable-modern=off",
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
    ]
    if profile == "dmar":
        # Described, not programmed. The guest kernel refuses the strong profile
        # on this machine, and the run is what shows the refusal is decided from
        # the conditions rather than from the table's absence.
        argv += ["-device", "intel-iommu,intremap=off,caching-mode=on"]
    if debug_exit:
        argv += ["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]
    if tools.prefix is not None:
        share = tools.prefix / "usr/share/qemu"
        if share.is_dir():
            argv += ["-L", str(share)]
    return argv


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, default=IMAGE)
    parser.add_argument("--out", type=Path, default=BUILD / "run-k3")
    parser.add_argument("--timeout", type=float, default=1800.0)
    parser.add_argument("--smp", type=int, default=PROCESSORS)
    parser.add_argument("--profile", default="weak", choices=["weak", "dmar"])
    parser.add_argument("--no-debug-exit", action="store_true")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    if not arguments.image.exists():
        print(
            f"image not found: {arguments.image}; run tools/build_image.py --phase k3",
            file=sys.stderr,
        )
        return 1

    arguments.out.mkdir(parents=True, exist_ok=True)
    vars_copy = arguments.out / "OVMF_VARS.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)
    disk = arguments.out / "k3-disk.img"
    scratch_disk(disk)

    argv = qemu_command(
        tools,
        vars_copy,
        arguments.image,
        disk,
        arguments.smp,
        arguments.profile,
        not arguments.no_debug_exit,
    )
    print("+", " ".join(argv), file=sys.stderr)

    started = time.monotonic()
    timed_out = False
    try:
        result = tools.run(argv, capture_output=True, text=True, timeout=arguments.timeout)
        status = result.returncode
        serial = result.stdout
        stderr = result.stderr
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        status = None
        serial = expired.stdout.decode("utf-8", "replace") if expired.stdout else ""
        stderr = expired.stderr.decode("utf-8", "replace") if expired.stderr else ""
    elapsed = time.monotonic() - started

    log = arguments.out / "serial.log"
    log.write_text(serial)
    (arguments.out / "qemu-stderr.log").write_text(stderr)

    record = {
        "command": argv,
        "cpu": CPU_MODEL,
        "processors": arguments.smp,
        "memory": MEMORY,
        "profile": arguments.profile,
        "machine": "q35",
        "accelerator": "tcg",
        "irqchip": "in-kernel-not-applicable-under-tcg",
        "virtio": "virtio-blk-pci modern only (disable-legacy=on)",
        "disk_backend": {"format": "raw", "cache": "writeback", "path": str(disk)},
        "iommu": "intel-iommu present, not programmed" if arguments.profile == "dmar" else "absent",
        "image": str(arguments.image),
        "exit_status": status,
        "timed_out": timed_out,
        "wall_seconds": round(elapsed, 3),
        "serial_log": str(log.relative_to(ROOT)) if log.is_relative_to(ROOT) else str(log),
        "expected_exit_complete": run_k1.EXIT_COMPLETE,
        "expected_exit_panic": run_k1.EXIT_PANIC,
        "toolchain": tools.describe(),
    }
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: v for k, v in record.items() if k != "toolchain"}, indent=2))
    return 0 if status == run_k1.EXIT_COMPLETE else 1


if __name__ == "__main__":
    raise SystemExit(main())
