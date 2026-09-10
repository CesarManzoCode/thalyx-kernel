#!/usr/bin/env python3
"""Run the K4 image under QEMU, one case at a time, over a medium that persists.

A case is a scenario, a fault directive and a number of legs. What makes it a
K4 run rather than a K3 run with a disk is that the medium is created once and
carried from one leg to the next: the first leg is cut at a named point, and the
second boots on whatever the first actually left behind.

The directive is written into a block outside the store, and for one named leg
only -- the first, unless the case says otherwise. Every other leg gets a
directive that names no point, so the run it performs is the recovery rather
than another cut. Cutting a later leg is how a case gets a look at what
recovery itself wrote before a compaction rewrites the arena over it; what it
never does is let recovery consult the directive, and no rule of the format
names it. It exists so that "the write after this one never happened" is a
decision rather than a hope.

The suppression is done by the guest's own driver, not by the emulator. QEMU
sees the writes that were issued and no others, so what the medium holds after a
leg is exactly what the driver put there. That is a narrower claim than a power
cut against a real controller, and the evidence says so rather than implying the
harness proves something about hardware it never touched.

Nothing here interprets the serial log; the gate reads the kernel's own records
and the medium itself.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import k4_format as fmt  # noqa: E402
import run_k1  # noqa: E402
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
IMAGE = BUILD / "thalyx-k4.img"

# The K3 model. K4 changes what runs, not what it runs on.
CPU_MODEL = run_k1.CPU_MODEL + ",+x2apic"
MEMORY = run_k1.MEMORY
PROCESSORS = 4

# Enough for the store, its two arenas and the block the directive lives in,
# with room left over that no structure of the format names.
DISK_MIB = 8


def blank_medium(path: Path) -> None:
    """Creates a zeroed medium, so a case never inherits another case's store."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as handle:
        handle.truncate(DISK_MIB * 1024 * 1024)


def write_directive(
    path: Path,
    point: int,
    mode: int,
    arg: int,
    leg: int,
    scenario: int,
    seed: int,
    stop_at_next_flush: int,
) -> dict:
    """Writes the fault directive into the block outside the store."""
    body = fmt.encode(
        "HarnessDirective",
        {
            "magic": fmt.MAGIC["harness"],
            "version": 1,
            "fault_point": point,
            "fault_arg": arg,
            "fault_mode": mode,
            "leg": leg,
            "scenario": scenario,
            "seed": seed,
            "stop_at_next_flush": stop_at_next_flush,
        },
    )
    body = body[: fmt.STRUCTS["HarnessDirective"][0] - 32]
    digest = hashlib.sha256(body).digest()
    block = bytearray(fmt.BLOCK_SIZE)
    block[: len(body)] = body
    block[len(body) : len(body) + 32] = digest
    with path.open("r+b") as handle:
        handle.seek(fmt.HARNESS_DIRECTIVE_BLOCK * fmt.BLOCK_SIZE)
        handle.write(bytes(block))
    return {
        "fault_point": fmt.FAULTPOINT_NAME.get(point, point),
        "fault_mode": fmt.FAULTMODE_NAME.get(mode, mode),
        "fault_arg": arg,
        "leg": leg,
        "scenario": scenario,
        "seed": seed,
        "stop_at_next_flush": stop_at_next_flush,
    }


def qemu_command(
    tools: tc.Toolchain,
    vars_copy: Path,
    image: Path,
    medium: Path,
    processors: int,
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
        "-drive", f"if=none,id=k4disk,format=raw,cache=writeback,file={medium}",
        "-device", "virtio-blk-pci,drive=k4disk,disable-legacy=on,disable-modern=off",
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


def one_leg(
    tools: tc.Toolchain,
    out: Path,
    image: Path,
    medium: Path,
    leg: int,
    processors: int,
    timeout: float,
    debug_exit: bool,
) -> dict:
    """Runs one leg and records what it did, without judging it."""
    vars_copy = out / f"OVMF_VARS-{leg}.fd"
    shutil.copyfile(tools.ovmf_vars, vars_copy)
    argv = qemu_command(tools, vars_copy, image, medium, processors, debug_exit)
    print("+", " ".join(argv), file=sys.stderr)

    started = time.monotonic()
    timed_out = False
    try:
        result = tools.run(argv, capture_output=True, text=True, timeout=timeout)
        status = result.returncode
        serial = result.stdout
        stderr = result.stderr
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        status = None
        serial = expired.stdout.decode("utf-8", "replace") if expired.stdout else ""
        stderr = expired.stderr.decode("utf-8", "replace") if expired.stderr else ""
    elapsed = time.monotonic() - started

    log = out / f"serial-{leg}.log"
    log.write_text(serial)
    (out / f"qemu-stderr-{leg}.log").write_text(stderr)
    # The medium as this leg left it. The next leg boots on the file itself; the
    # copy is what a gate reads when it wants to see an intermediate state.
    after = out / f"medium-after-{leg}.img"
    shutil.copyfile(medium, after)
    return {
        "leg": leg,
        "exit_status": status,
        "timed_out": timed_out,
        "wall_seconds": round(elapsed, 3),
        "serial_log": str(log.relative_to(ROOT)) if log.is_relative_to(ROOT) else str(log),
        "medium_after": str(after.relative_to(ROOT)) if after.is_relative_to(ROOT) else str(after),
        "medium_sha256": hashlib.sha256(after.read_bytes()).hexdigest(),
    }


def named(table: dict[str, int], value: str) -> int:
    if value.isdigit():
        return int(value)
    key = value.upper()
    if key not in table:
        raise SystemExit(f"unknown value {value!r}; one of {sorted(table)}")
    return table[key]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, default=IMAGE)
    parser.add_argument("--out", type=Path, default=BUILD / "run-k4")
    parser.add_argument("--timeout", type=float, default=1800.0)
    parser.add_argument("--smp", type=int, default=PROCESSORS)
    parser.add_argument("--legs", type=int, default=2)
    parser.add_argument("--scenario", type=int, default=1)
    parser.add_argument("--point", default="NONE")
    parser.add_argument("--mode", default="NONE")
    parser.add_argument("--arg", type=int, default=0)
    parser.add_argument("--seed", type=int, default=0x4B34)
    parser.add_argument("--stop-at-next-flush", type=int, default=0, choices=[0, 1])
    parser.add_argument(
        "--cut",
        action="append",
        default=[],
        metavar="LEG:POINT:MODE[:ARG[:FLUSH]]",
        help="cut this leg at this point; repeat for more than one. Without it the "
        "shorthand options above cut the first leg and no other.",
    )
    parser.add_argument(
        "--keep-medium",
        action="store_true",
        help="continue on the medium already in the output directory",
    )
    parser.add_argument("--no-debug-exit", action="store_true")
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    if not arguments.image.exists():
        print(
            f"image not found: {arguments.image}; run tools/build_image.py --phase k4",
            file=sys.stderr,
        )
        return 1

    # One directive per leg, keyed by leg number. The shorthand names the first
    # leg; `--cut` names any of them, and a leg nobody names gets a directive
    # that names no point, which is what makes it a recovery.
    cuts: dict[int, tuple[int, int, int, int]] = {}
    point = named(fmt.FAULTPOINT, arguments.point)
    mode = named(fmt.FAULTMODE, arguments.mode)
    if point != fmt.FAULTPOINT["NONE"] or mode != fmt.FAULTMODE["NONE"]:
        cuts[1] = (point, mode, arguments.arg, arguments.stop_at_next_flush)
    for spec in arguments.cut:
        parts = spec.split(":")
        if len(parts) < 3:
            raise SystemExit(f"--cut wants LEG:POINT:MODE[:ARG[:FLUSH]], got {spec!r}")
        leg = int(parts[0])
        cuts[leg] = (
            named(fmt.FAULTPOINT, parts[1]),
            named(fmt.FAULTMODE, parts[2]),
            int(parts[3]) if len(parts) > 3 else 0,
            int(parts[4]) if len(parts) > 4 else 0,
        )

    arguments.out.mkdir(parents=True, exist_ok=True)
    medium = arguments.out / "k4-medium.img"
    if not (arguments.keep_medium and medium.exists()):
        blank_medium(medium)

    legs: list[dict] = []
    directives: list[dict] = []
    for leg in range(1, arguments.legs + 1):
        cut = cuts.get(leg, (fmt.FAULTPOINT["NONE"], fmt.FAULTMODE["NONE"], 0, 0))
        directives.append(
            write_directive(
                medium,
                cut[0],
                cut[1],
                cut[2],
                leg,
                arguments.scenario,
                arguments.seed,
                cut[3],
            )
        )
        legs.append(
            one_leg(
                tools,
                arguments.out,
                arguments.image,
                medium,
                leg,
                arguments.smp,
                arguments.timeout,
                not arguments.no_debug_exit,
            )
        )

    record = {
        "case": {
            "scenario": arguments.scenario,
            "point": fmt.FAULTPOINT_NAME.get(point, point),
            "mode": fmt.FAULTMODE_NAME.get(mode, mode),
            "arg": arguments.arg,
            "seed": arguments.seed,
            "stop_at_next_flush": arguments.stop_at_next_flush,
            "legs": arguments.legs,
            "cuts": {
                str(leg): {
                    "point": fmt.FAULTPOINT_NAME.get(spec[0], spec[0]),
                    "mode": fmt.FAULTMODE_NAME.get(spec[1], spec[1]),
                    "arg": spec[2],
                    "stop_at_next_flush": spec[3],
                }
                for leg, spec in sorted(cuts.items())
            },
        },
        "cpu": CPU_MODEL,
        "processors": arguments.smp,
        "memory": MEMORY,
        "machine": "q35",
        "accelerator": "tcg",
        "virtio": "virtio-blk-pci modern only (disable-legacy=on)",
        "medium": {
            "format": "raw",
            "cache": "writeback",
            "path": str(medium),
            "mib": DISK_MIB,
            "suppression": "guest driver; the emulator sees only the writes that were issued",
        },
        "image": str(arguments.image),
        "directives": directives,
        "legs": legs,
        "expected_exit_complete": run_k1.EXIT_COMPLETE,
        "expected_exit_panic": run_k1.EXIT_PANIC,
        "toolchain": tools.describe(),
    }
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: v for k, v in record.items() if k != "toolchain"}, indent=2))
    return 0 if all(leg["exit_status"] == run_k1.EXIT_COMPLETE for leg in legs) else 1


if __name__ == "__main__":
    raise SystemExit(main())
