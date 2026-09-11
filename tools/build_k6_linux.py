#!/usr/bin/env python3
"""Build the Linux side of K6's paired benchmarks: a guest that is only Linux.

The comparison K6 makes is between this kernel and Linux *on the same virtual
machine*. The Linux side is therefore not the host: it is a guest booted in the
same QEMU, with the same machine type, processor model, processor count,
memory and firmware as the native runs, and its only program is the benchmark.

What this builds, under build/k6/linux/:

  lbench          the guest's /init: tests/k6/bench.c and tests/k6/plat_linux.c,
                  linked statically against glibc. bench.c is compiled with the
                  native target's own machine flags (build_native.MACHINE), so
                  the code that times a primitive is the same code on both sides.
  thalyx-engine   Thalyx's engine/thalyx-engine.cpp, unchanged, linked
                  statically from the very objects tools/build_reference.py
                  compiles for the K5 reference -- the same llama.cpp tree, the
                  same flags -- so the guest needs no loader and no libraries.
  base.cpio       an initramfs holding /init, /thalyx-engine, /tiny.gguf and a
                  /dev/console node. A boot appends a second archive holding
                  its /plan.bin; the kernel reads concatenated archives.
  vmlinuz         the host's installed kernel image, copied and hashed. It is
                  not built here and nothing claims it was: the manifest says
                  which package and release it is.
  linux-manifest.json   digests of all of it, and of the compilers.

**Nothing here is native evidence.** It is the other half of a comparison.

Usage: tools/build_k6_linux.py
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "build" / "k6" / "linux"
REFERENCE = ROOT / "build" / "reference"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_native  # noqa: E402

BENCH_SOURCES = [ROOT / "tests/k6/bench.c", ROOT / "tests/k6/plat_linux.c"]
# The native target's promises about generated code, without the parts that
# only mean something freestanding. `-iquote` and not `-I` for the native
# include tree: it holds a libc of its own, and glibc's headers must win.
LINUX_CFLAGS = [
    "-std=gnu11", "-O2", "-g", "-Wall", "-Wextra", "-Wno-unused-parameter",
    *[flag for flag in build_native.MACHINE if flag not in ("-fno-pic", "-fno-pie")],
    "-fno-pie", "-no-pie",
    f"-iquote{ROOT / 'tests/k6'}", f"-iquote{ROOT / 'user/native/include'}",
]


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("+", " ".join(str(a) for a in argv), file=sys.stderr)
    result = subprocess.run(argv, **kwargs)
    if result.returncode != 0:
        raise SystemExit(f"failed: {' '.join(str(a) for a in argv)}")
    return result


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_bench() -> Path:
    OUT.mkdir(parents=True, exist_ok=True)
    binary = OUT / "lbench"
    run(["gcc", *LINUX_CFLAGS, "-static", "-pthread", "-o", str(binary),
         *[str(s) for s in BENCH_SOURCES]])
    return binary


def build_engine() -> Path:
    """Thalyx's engine, static, from the objects the K5 reference links."""
    objdir = REFERENCE / "obj"
    if not (REFERENCE / "thalyx-engine").exists() or not objdir.is_dir():
        run([sys.executable, str(ROOT / "tools/build_reference.py")], cwd=ROOT)
    objects = sorted(objdir.glob("*.o"))
    if not objects:
        raise SystemExit("the reference build left no objects to link")
    engine = OUT / "thalyx-engine"
    run(["g++", "-static", "-o", str(engine), *[str(o) for o in objects],
         "-Wl,--gc-sections", "-pthread", "-lm"])
    return engine


def newc(entries: list[tuple[str, int, bytes, tuple[int, int]]]) -> bytes:
    """A `newc` cpio archive: name, mode, contents, (rdev major, minor)."""
    out = bytearray()
    for inode, (name, mode, data, rdev) in enumerate(entries, start=1):
        encoded = name.encode() + b"\0"
        header = "070701" + "".join(
            f"{value:08x}" for value in (inode, mode, 0, 0, 1, 0, len(data), 0, 0,
                                          rdev[0], rdev[1], len(encoded), 0))
        out += header.encode() + encoded
        out += b"\0" * (-len(out) % 4)
        out += data
        out += b"\0" * (-len(out) % 4)
    trailer = b"TRAILER!!!\0"
    out += ("070701" + "".join(f"{v:08x}" for v in (0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
                                                     len(trailer), 0))).encode() + trailer
    out += b"\0" * (-len(out) % 4)
    return bytes(out)


def base_archive(bench: Path, engine: Path, model: Path) -> Path:
    archive = OUT / "base.cpio"
    archive.write_bytes(newc([
        ("dev", 0o040755, b"", (0, 0)),
        ("dev/console", 0o020600, b"", (5, 1)),
        ("init", 0o100755, bench.read_bytes(), (0, 0)),
        ("thalyx-engine", 0o100755, engine.read_bytes(), (0, 0)),
        ("tiny.gguf", 0o100644, model.read_bytes(), (0, 0)),
    ]))
    return archive


def plan_archive(plan: bytes, destination: Path) -> Path:
    """The per-boot archive the kernel reads after the base one."""
    destination.write_bytes(newc([("plan.bin", 0o100644, plan, (0, 0))]))
    return destination


def host_kernel() -> tuple[Path, dict]:
    release = platform.release()
    source = Path("/usr/lib/modules") / release / "vmlinuz"
    if not source.exists():
        raise SystemExit(f"no kernel image at {source}; the Linux guest needs one")
    image = OUT / "vmlinuz"
    shutil.copyfile(source, image)
    base = Path("/usr/lib/modules") / release / "pkgbase"
    return image, {
        "release": release,
        "source": str(source),
        "package": base.read_text().strip() if base.exists() else None,
        "sha256": sha256(image),
    }


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    bench = build_bench()
    engine = build_engine()
    model = REFERENCE / "tiny.gguf"
    if not model.exists():
        run([sys.executable, str(ROOT / "tools/build_reference.py")], cwd=ROOT)
    archive = base_archive(bench, engine, model)
    kernel, kernel_record = host_kernel()
    compiler = subprocess.run(["gcc", "--version"], capture_output=True, text=True).stdout
    manifest = {
        "note": "the Linux side of a paired comparison, run as a guest on the same virtual "
                "machine; not native evidence",
        "bench": {"path": str(bench.relative_to(ROOT)), "sha256": sha256(bench),
                  "sources": {str(s.relative_to(ROOT)): sha256(s) for s in BENCH_SOURCES},
                  "flags": LINUX_CFLAGS},
        "engine": {"path": str(engine.relative_to(ROOT)), "sha256": sha256(engine),
                   "source": "engine/thalyx-engine.cpp at the pinned Thalyx revision, unchanged, "
                             "linked statically from the K5 reference objects"},
        "model": {"path": str(model.relative_to(ROOT)), "sha256": sha256(model)},
        "initramfs": {"path": str(archive.relative_to(ROOT)), "sha256": sha256(archive)},
        "kernel": kernel_record | {"path": str(kernel.relative_to(ROOT))},
        "compiler": compiler.splitlines()[0] if compiler else None,
        "glibc": os.confstr("CS_GNU_LIBC_VERSION") if hasattr(os, "confstr") else None,
    }
    (OUT / "linux-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({k: v for k, v in manifest.items() if k in ("bench", "engine", "kernel")},
                     indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
