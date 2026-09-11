#!/usr/bin/env python3
"""Run the K1 image under QEMU with OVMF and capture the diagnostic plane.

The variable store is copied per run so the firmware cannot carry state between
runs. Serial output is captured verbatim; nothing here interprets it, so that
the gate evaluates the kernel's own records rather than this script's summary of
them.
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
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
IMAGE = BUILD / "thalyx-k1.img"

# `isa-debug-exit` reports the value the kernel writes as `(value << 1) | 1`.
KERNEL_STATUS_COMPLETE = 0x10
KERNEL_STATUS_PANIC = 0x11
EXIT_COMPLETE = (KERNEL_STATUS_COMPLETE << 1) | 1
EXIT_PANIC = (KERNEL_STATUS_PANIC << 1) | 1

# An explicit model rather than `max`, so a run on another host exercises the
# same feature set. SMEP and SMAP are requested because the kernel's platform
# profile reports whether it got them.
CPU_MODEL = "qemu64,+smep,+smap,+pdpe1gb"
MEMORY = "512M"


def qemu_command(tools: tc.Toolchain, vars_copy: Path, image: Path, debug_exit: bool) -> list[str]:
    argv = [
        str(tools.qemu),
        "-machine", tc.machine(),
        "-cpu", tc.cpu_model(CPU_MODEL),
        "-smp", "1",
        "-m", MEMORY,
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={tools.ovmf_code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy}",
        "-drive", f"format=raw,file={image}",
        "-serial", "stdio",
        "-display", "none",
        "-no-reboot",
    ]
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
    parser.add_argument("--out", type=Path, default=BUILD / "run")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--no-debug-exit", action="store_true")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    if not arguments.image.exists():
        print(f"image not found: {arguments.image}; run tools/build_image.py", file=sys.stderr)
        return 1

    arguments.out.mkdir(parents=True, exist_ok=True)
    vars_copy = arguments.out / "OVMF_VARS.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)

    argv = qemu_command(tools, vars_copy, arguments.image, not arguments.no_debug_exit)
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
        "cpu": tc.cpu_model(CPU_MODEL),
        "processors": 1,
        "memory": MEMORY,
        "machine": "q35",
        "accelerator": tc.accelerator(),
        "image": str(arguments.image),
        "exit_status": status,
        "timed_out": timed_out,
        "wall_seconds": round(elapsed, 3),
        "serial_log": str(log.relative_to(ROOT)) if log.is_relative_to(ROOT) else str(log),
        "expected_exit_complete": EXIT_COMPLETE,
        "expected_exit_panic": EXIT_PANIC,
        "toolchain": tools.describe(),
    }
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: v for k, v in record.items() if k != "toolchain"}, indent=2))
    return 0 if status == EXIT_COMPLETE else 1


if __name__ == "__main__":
    raise SystemExit(main())
