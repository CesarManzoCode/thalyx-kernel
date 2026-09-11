#!/usr/bin/env python3
"""Run the K5 image under QEMU and capture the diagnostic plane.

The machine, CPU model, memory and firmware are the ones `run_k1.py` defines,
with the K3 processor count and the K4 medium, because K5 stands on both: the
port's managed state is K4's service on K4's driver, and the port's domains are
scheduled by K3's scheduler on four processors.

Nothing here interprets the serial log. The gate reads the kernel's own records
and the bytes of the medium; this script only produces them.
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
IMAGE = BUILD / "thalyx-k5.img"

CPU_MODEL = run_k1.CPU_MODEL + ",+x2apic"
MEMORY = "1024M"
PROCESSORS = 4
MEDIUM_MIB = 8


def qemu_command(
    tools: tc.Toolchain,
    vars_copy: Path,
    image: Path,
    medium: Path | None,
    processors: int,
    debug_exit: bool,
) -> list[str]:
    argv = [
        str(tools.qemu),
        "-machine", tc.machine(),
        "-cpu", tc.cpu_model(CPU_MODEL),
        "-smp", str(processors),
        "-m", MEMORY,
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={tools.ovmf_code}",
        "-drive", f"if=pflash,format=raw,unit=1,file={vars_copy}",
        "-drive", f"format=raw,file={image}",
    ]
    if medium is not None:
        argv += [
            "-drive", f"if=none,id=k5medium,format=raw,cache=writeback,file={medium}",
            "-device", "virtio-blk-pci,drive=k5medium,disable-legacy=on,disable-modern=off",
        ]
    argv += [
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
    parser.add_argument("--out", type=Path, default=BUILD / "run-k5")
    parser.add_argument("--medium", type=Path, default=None,
                        help="block medium to attach; none when the stage needs none")
    parser.add_argument("--fresh-medium", action="store_true",
                        help="zero the medium before the run")
    parser.add_argument("--processors", type=int, default=PROCESSORS)
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument("--no-debug-exit", action="store_true")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    if not arguments.image.exists():
        print(f"image not found: {arguments.image}; run tools/build_image.py --phase k5",
              file=sys.stderr)
        return 1

    arguments.out.mkdir(parents=True, exist_ok=True)
    vars_copy = arguments.out / "OVMF_VARS.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)

    medium = arguments.medium
    if medium is not None and (arguments.fresh_medium or not medium.exists()):
        medium.parent.mkdir(parents=True, exist_ok=True)
        with medium.open("wb") as handle:
            handle.truncate(MEDIUM_MIB * 1024 * 1024)

    argv = qemu_command(tools, vars_copy, arguments.image, medium,
                        arguments.processors, not arguments.no_debug_exit)
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

    manifest = BUILD / "image-manifest-k5.json"
    record = {
        "command": argv,
        "cpu": tc.cpu_model(CPU_MODEL),
        "processors": arguments.processors,
        "memory": MEMORY,
        "machine": "q35",
        "accelerator": tc.accelerator(),
        "image": str(arguments.image),
        "medium": str(medium) if medium else None,
        "exit_status": status,
        "timed_out": timed_out,
        "wall_seconds": round(elapsed, 3),
        "serial_log": str(log.relative_to(ROOT)) if log.is_relative_to(ROOT) else str(log),
        "expected_exit_complete": run_k1.EXIT_COMPLETE,
        "expected_exit_panic": run_k1.EXIT_PANIC,
        "image_manifest": json.loads(manifest.read_text()) if manifest.exists() else None,
        "toolchain": tools.describe(),
    }
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: v for k, v in record.items()
                      if k not in ("toolchain", "image_manifest", "command")}, indent=2))
    return 0 if status == run_k1.EXIT_COMPLETE else 1


if __name__ == "__main__":
    raise SystemExit(main())
