#!/usr/bin/env python3
"""Boot the K5 `thalyx` image with a virtio-console the host drives.

This is the machine side of EXP-13's third arm. It boots the same kernel, the
same K4 durable state service and block driver as every K5 stage, plus the
link domain, and exposes a multiport virtio-console function whose ports are
UNIX sockets the host connects to. The real Thalyx, on the host, speaks its
managed protocol over those sockets exactly as it speaks it over a loopback to
`thalyx-managed` for `linux-managed`.

Unlike `run_k5.py`, this does not wait for the guest to finish and read a
verdict off the serial log: the guest waits for the host, and the host drives
it. The host connects to the port sockets, runs its transactions, sends a
`shutdown` on the control port when it is done, and this process exits with the
guest. A run whose host never connects ends at the guest's own deadline.

The port sockets are created by QEMU (server side) at fixed names under
`--sockets`:

  port1 .. portLINES   the worker lines a consumer connects to
  port<CONTROL>        the control line: fence a work, read stats, shutdown

so the host knows every path without parsing anything this prints.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_k1  # noqa: E402
import run_k5  # noqa: E402
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"

# Mirrors `thalyx_user_k5pkg::link`: worker ports one to LINES, the control
# port after them, and one spare so QEMU's port zero is never a worker.
LINES = 4
CONTROL_PORT = 5
MAX_PORTS = 5


def console_args(sockets: Path) -> list[str]:
    """A modern multiport virtio-console with one UNIX socket per port."""
    argv = [
        "-device",
        # max_ports counts port zero, which this package does not use.
        f"virtio-serial-pci,id=vcon,disable-legacy=on,disable-modern=off,max_ports={MAX_PORTS + 1}",
    ]
    for port in range(1, MAX_PORTS + 1):
        path = sockets / f"port{port}"
        argv += [
            "-chardev",
            f"socket,id=vport{port},path={path},server=on,wait=off",
            "-device",
            f"virtserialport,chardev=vport{port},name=thalyx.port{port},nr={port}",
        ]
    return argv


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, default=BUILD / "thalyx-k5.img")
    parser.add_argument("--medium", type=Path, required=True,
                        help="the K4 block medium; created and zeroed with --fresh-medium")
    parser.add_argument("--sockets", type=Path, required=True,
                        help="directory the port sockets are created in")
    parser.add_argument("--out", type=Path, default=BUILD / "run-k1-link")
    parser.add_argument("--fresh-medium", action="store_true")
    parser.add_argument("--processors", type=int, default=run_k5.PROCESSORS)
    parser.add_argument("--timeout", type=float, default=1200.0)
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    if not arguments.image.exists():
        print(f"image not found: {arguments.image}; "
              f"run tools/build_image.py --phase k5 --stage thalyx", file=sys.stderr)
        return 1

    arguments.out.mkdir(parents=True, exist_ok=True)
    arguments.sockets.mkdir(parents=True, exist_ok=True)
    # A socket path this run did not make is not one it may reuse.
    for port in range(1, MAX_PORTS + 1):
        stale = arguments.sockets / f"port{port}"
        if stale.exists():
            stale.unlink()

    vars_copy = arguments.out / "OVMF_VARS.fd"
    import shutil
    shutil.copyfile(tools.ovmf_vars, vars_copy)

    medium = arguments.medium
    if arguments.fresh_medium or not medium.exists():
        medium.parent.mkdir(parents=True, exist_ok=True)
        with medium.open("wb") as handle:
            handle.truncate(run_k5.MEDIUM_MIB * 1024 * 1024)

    argv = run_k5.qemu_command(
        tools, vars_copy, arguments.image, medium, arguments.processors, debug_exit=True
    )
    # Insert the console device before the trailing -serial/-display flags; any
    # position is fine, so append.
    argv += console_args(arguments.sockets)

    record = {
        "command": argv,
        "image": str(arguments.image),
        "medium": str(medium),
        "sockets": str(arguments.sockets),
        "lines": LINES,
        "control_port": CONTROL_PORT,
        "accelerator": tc.accelerator(),
        "processors": arguments.processors,
    }
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    print("+", " ".join(argv), file=sys.stderr)

    started = time.monotonic()
    log = (arguments.out / "serial.log").open("w")
    try:
        process = tools.run(
            argv, stdout=log, stderr=subprocess.STDOUT, timeout=arguments.timeout, check=False
        )
        status = process.returncode
    except subprocess.TimeoutExpired:
        status = None
    finally:
        log.close()
    elapsed = time.monotonic() - started
    record["exit_status"] = status
    record["wall_seconds"] = round(elapsed, 3)
    (arguments.out / "run.json").write_text(json.dumps(record, indent=2) + "\n")
    return 0 if status in (run_k1.EXIT_COMPLETE, 0) else 1


if __name__ == "__main__":
    raise SystemExit(main())
