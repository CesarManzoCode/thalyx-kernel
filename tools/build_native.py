#!/usr/bin/env python3
"""Build native C programs for the x86_64-thalyx target.

This is **host build tooling**. It runs a cross-compilation on the development
machine and produces ELF images that only this kernel can load; nothing it does
is evidence that anything executed. What executes natively is the image it
writes, inside the guest, and the evidence for that comes from the kernel's own
records.

The target is defined here and nowhere else, because a target is a set of
promises rather than a triple:

  * ELF64, `ET_EXEC`, static, no interpreter, no relocations, entry `_start`,
    three page-aligned `PT_LOAD` segments split R-X / R-- / RW-.
  * Base 0x400000, small code model, no PIC. The kernel's bounded loader does
    not relocate.
  * SSE and SSE2 in hardware, `-mfpmath=sse`, and **no AVX**: the kernel saves
    and restores the legacy `FXSAVE` area eagerly and does not enable `XSAVE`,
    so a program using wide registers would have them silently dropped or
    leaked across a context switch. This is the one flag whose absence would be
    a correctness bug rather than a performance choice.
  * No x87 beyond what `long double` would need, which nothing here uses.
  * No stack protector, no unwind tables, no `.init_array`: nothing below this
    program unwinds and nothing runs constructors for it.
  * A libc that is `user/native`, and no other. `-nostdinc` is what makes that
    true rather than intended: the host's headers are not on the search path,
    so a program that reaches for a POSIX function fails to compile here
    instead of failing to link in the guest.

The red zone is left enabled on purpose: the CPU switches to the per-processor
stack from the TSS on every interrupt and to the kernel's `syscall` stack on
every entry, so nothing writes below a user program's stack pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build" / "native"
NATIVE = ROOT / "user" / "native"

TARGET = "x86_64-thalyx"

CFLAGS = [
    "-std=gnu11",
    "-O2",
    "-g",
    "-m64",
    "-ffreestanding",
    "-fno-pic",
    "-fno-pie",
    "-mcmodel=small",
    "-mno-red-zone" if os.environ.get("THALYX_NO_RED_ZONE") else "-mred-zone",
    "-msse",
    "-msse2",
    "-mfpmath=sse",
    "-mno-avx",
    "-mno-avx2",
    "-mno-sse4.1",
    "-mno-sse4.2",
    "-fno-stack-protector",
    "-fno-asynchronous-unwind-tables",
    "-fno-unwind-tables",
    "-fno-common",
    "-fno-strict-aliasing",
    "-fno-ident",
]

WARNINGS = ["-Wall", "-Wextra", "-Wno-unused-parameter"]

RUNTIME_SOURCES = [
    "start.S",
    "boot.c",
    "sys.c",
    "string.c",
    "heap.c",
    "mem.c",
    "printf.c",
    "math.c",
    "conv.c",
    "time.c",
    "ipc.c",
    "thread.c",
]


def gcc() -> str:
    return os.environ.get("THALYX_CC", "gcc")


def freestanding_include() -> Path:
    out = subprocess.run([gcc(), "-print-file-name=include"], capture_output=True, text=True)
    return Path(out.stdout.strip())


def includes() -> list[str]:
    return [
        "-nostdinc",
        "-isystem",
        str(freestanding_include()),
        "-I",
        str(NATIVE / "include"),
        "-I",
        str(ROOT / "abi" / "include"),
    ]


def run(argv: list[str], quiet: bool = False) -> None:
    if not quiet:
        print("+", " ".join(argv), file=sys.stderr)
    result = subprocess.run(argv, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(f"command failed with status {result.returncode}: {' '.join(argv)}")


def compile_one(source: Path, obj: Path, extra: list[str], warnings: list[str]) -> None:
    obj.parent.mkdir(parents=True, exist_ok=True)
    argv = [gcc(), "-c", str(source), "-o", str(obj)]
    argv += CFLAGS + warnings + includes() + extra
    argv += [f"-ffile-prefix-map={ROOT}=/thalyx-kernel"]
    run(argv, quiet=True)


def build_runtime() -> Path:
    """The native runtime, as one archive every program links."""
    objects: list[Path] = []
    for name in RUNTIME_SOURCES:
        source = NATIVE / "src" / name
        obj = BUILD / "runtime" / (name + ".o")
        compile_one(source, obj, [], WARNINGS)
        objects.append(obj)
    archive = BUILD / "libthalyx-native.a"
    if archive.exists():
        archive.unlink()
    run(["ar", "rcs", str(archive)] + [str(o) for o in objects], quiet=True)
    return archive


def link(name: str, objects: list[Path], archive: Path, extra_archives: list[Path]) -> Path:
    image = BUILD / f"{name}.elf"
    argv = [
        gcc(),
        "-nostdlib",
        "-static",
        "-no-pie",
        "-Wl,-T," + str(NATIVE / "link.ld"),
        "-Wl,--build-id=none",
        "-Wl,-z,noexecstack",
        "-Wl,--gc-sections",
        "-o",
        str(image),
    ]
    argv += [str(o) for o in objects]
    argv += [str(a) for a in extra_archives]
    argv += [str(archive), "-lgcc"]
    run(argv, quiet=True)
    subprocess.run(["strip", "--strip-debug", str(image)], cwd=ROOT, check=False)
    return image


def describe() -> dict:
    version = subprocess.run([gcc(), "--version"], capture_output=True, text=True).stdout
    return {
        "target": TARGET,
        "host_compiler": version.splitlines()[0] if version else "unknown",
        "cflags": CFLAGS,
        "note": "host build tooling; producing an image is not executing one",
    }


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


PROGRAMS: dict[str, dict] = {}


def register(name: str, sources: list[str], extra_cflags: list[str] | None = None,
             warnings: list[str] | None = None, extra_dirs: list[str] | None = None) -> None:
    PROGRAMS[name] = {
        "sources": sources,
        "cflags": extra_cflags or [],
        "warnings": WARNINGS if warnings is None else warnings,
        "dirs": extra_dirs or [],
    }


def build_program(name: str, archive: Path) -> Path:
    spec = PROGRAMS[name]
    objects: list[Path] = []
    extra = spec["cflags"] + [f"-I{ROOT / d}" for d in spec["dirs"]]
    for relative in spec["sources"]:
        source = ROOT / relative
        obj = BUILD / name / (Path(relative).name + ".o")
        compile_one(source, obj, extra, spec["warnings"])
        objects.append(obj)
    return link(name, objects, archive, [])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("programs", nargs="*", help="programs to build; all when empty")
    arguments = parser.parse_args()

    if shutil.which(gcc()) is None:
        print(f"{gcc()} not found; set THALYX_CC", file=sys.stderr)
        return 1

    BUILD.mkdir(parents=True, exist_ok=True)
    archive = build_runtime()

    wanted = arguments.programs or sorted(PROGRAMS)
    manifest = {"toolchain": describe(), "runtime": {"path": str(archive.relative_to(ROOT))},
                "programs": {}}
    for name in wanted:
        if name not in PROGRAMS:
            print(f"unknown native program: {name}", file=sys.stderr)
            return 1
        image = build_program(name, archive)
        manifest["programs"][name] = {
            "path": str(image.relative_to(ROOT)),
            "sha256": digest(image),
            "bytes": image.stat().st_size,
        }
    (BUILD / "native-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))
    return 0


register("nsmoke", ["user/nsmoke/main.c"])

if __name__ == "__main__":
    raise SystemExit(main())
