#!/usr/bin/env python3
"""Build the Linux reference: Thalyx's own engine and model, on the host.

K5's evidence asks for the same semantics on Linux and on this kernel. The
Linux side of that comparison is not a program this repository wrote. It is
`engine/thalyx-engine.cpp` from the pinned Thalyx revision, **unchanged**,
built against the same llama.cpp tree the native engine is built against, with
the configuration Thalyx's `dev/build-engine.sh` uses -- the CPU backend only,
no native tuning, llamafile and repacking at their defaults -- and the model
is the one Thalyx's `dev/tiny-model.py` writes with llama.cpp's own `gguf-py`.

**This is host execution and it is not native evidence.** It runs on the
development machine, under Linux, and the gate uses what it produces only as
the other side of a comparison: what the reference engine answers for a prompt
is what the native engine is expected to answer for the same prompt and the
same weights. Nothing here says anything ran inside the kernel.

Outputs, under build/reference/:
  thalyx-engine        the reference engine, a host executable
  tiny.gguf            the model, byte-identical to what the image carries
  reference.json       digests of both, the toolchain, and the Python packages
                       the model was written with

Usage: tools/build_reference.py
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "build" / "vendor"
LLAMA = VENDOR / "llama.cpp"
THALYX = VENDOR / "thalyx-ref"
OUT = ROOT / "build" / "reference"
PYENV = Path.home() / ".cache" / "thalyx-tools" / "pyenv"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_native as native  # noqa: E402

# The model, pinned. `tiny-model.py` seeds its generator, so the same numpy
# writes the same bytes; a different result stops the build instead of
# silently changing what both engines are compared on.
MODEL_SHA256 = "1b726566329f3aaca2f6d89ee0ec4efc31986936628ca6d68591077c857d3267"
NUMPY = "numpy==2.3.3"
PYYAML = "pyyaml==6.0.2"

# The host has no reason to diverge from the native arithmetic where it can
# be avoided: the baseline instruction set, as GGML_NATIVE=OFF gives Thalyx.
HOST_MACHINE = ["-O2", "-march=x86-64", "-mtune=generic", "-ffunction-sections",
                "-fdata-sections"]


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("+", " ".join(str(a) for a in argv), file=sys.stderr)
    result = subprocess.run(argv, **kwargs)
    if result.returncode != 0:
        raise SystemExit(f"failed: {' '.join(str(a) for a in argv)}")
    return result


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compile_host(jobs: list[tuple[str, Path, Path, list[str]]]) -> None:
    def one(job):
        compiler, source, obj, flags = job
        obj.parent.mkdir(parents=True, exist_ok=True)
        if obj.exists() and obj.stat().st_mtime > source.stat().st_mtime:
            return None
        result = subprocess.run([compiler, "-c", str(source), "-o", str(obj), *flags],
                                capture_output=True, text=True)
        return None if result.returncode == 0 else f"{source}:\n{result.stderr[-4000:]}"

    with ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
        failures = [f for f in pool.map(one, jobs) if f]
    if failures:
        print(failures[0], file=sys.stderr)
        raise SystemExit(f"{len(failures)} host translation unit(s) failed")


def build_engine() -> Path:
    if not (LLAMA / "src" / "llama.cpp").exists():
        run([sys.executable, str(ROOT / "tools/fetch_engine.py")])
    objdir = OUT / "obj"
    ggml_inc = [f"-I{LLAMA / 'ggml/include'}", f"-I{LLAMA / 'ggml/src'}",
                f"-I{LLAMA / 'ggml/src/ggml-cpu'}"]
    llama_inc = [f"-I{LLAMA / 'include'}", f"-I{LLAMA / 'src'}", f"-I{LLAMA / 'ggml/include'}"]
    common_inc = [f"-I{LLAMA / 'common'}", f"-I{LLAMA / 'vendor'}"] + llama_inc
    cflags = ["-std=gnu11", "-w", *HOST_MACHINE]
    cxxflags = ["-std=gnu++17", "-w", *HOST_MACHINE]

    jobs = []

    def unit(relative: str, extra: list[str]) -> None:
        path = LLAMA / relative
        is_c = path.suffix == ".c"
        jobs.append(("gcc" if is_c else "g++", path,
                     objdir / (relative.replace("/", "_") + ".o"),
                     (cflags if is_c else cxxflags) + extra))

    for relative in native.GGML_BASE_SOURCES:
        unit(relative, native.GGML_DEFINES + ggml_inc)
    for relative in native.GGML_CPU_SOURCES:
        unit(relative, native.GGML_DEFINES + native.GGML_CPU_DEFINES + ggml_inc)
    llama_defs = ["-DNDEBUG", "-DGGML_USE_CPU", f'-DLLAMA_VERSION="{native.LLAMA_VERSION}"',
                  f'-DLLAMA_COMMIT="{native.LLAMA_COMMIT}"']
    for path in sorted((LLAMA / "src").glob("*.cpp")) + sorted((LLAMA / "src/models").glob("*.cpp")):
        unit(str(path.relative_to(LLAMA)), llama_defs + llama_inc)
    for relative in native.COMMON_SOURCES:
        unit(relative, ["-DNDEBUG", "-DGGML_USE_CPU"] + common_inc)
    info = native.build_info_source()
    jobs.append(("g++", info, objdir / "common_build-info.o",
                 cxxflags + ["-DNDEBUG", "-DGGML_USE_CPU"] + common_inc))
    engine_source = THALYX / "engine" / "thalyx-engine.cpp"
    jobs.append(("g++", engine_source, objdir / "thalyx-engine.o",
                 cxxflags + ["-DNDEBUG", "-DGGML_USE_CPU"] + common_inc))
    compile_host(jobs)

    engine = OUT / "thalyx-engine"
    run(["g++", "-o", str(engine), *[str(j[2]) for j in jobs], "-Wl,--gc-sections",
         "-lpthread", "-lm"])
    return engine


def python_env() -> Path:
    python = PYENV / "bin" / "python3"
    if not python.exists():
        run([sys.executable, "-m", "venv", str(PYENV)])
    probe = subprocess.run([str(python), "-c", "import numpy, yaml; print(numpy.__version__)"],
                           capture_output=True, text=True)
    if probe.returncode != 0:
        run([str(python), "-m", "pip", "install", "--quiet", NUMPY, PYYAML])
    return python


def build_model(python: Path) -> Path:
    model = OUT / "tiny.gguf"
    run([str(python), str(THALYX / "dev" / "tiny-model.py"), str(LLAMA), str(model)])
    digest = sha256(model)
    if MODEL_SHA256 is not None and digest != MODEL_SHA256:
        raise SystemExit(f"tiny.gguf hashes to {digest}, not {MODEL_SHA256}")
    return model


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    engine = build_engine()
    python = python_env()
    model = build_model(python)
    packages = subprocess.run([str(python), "-m", "pip", "freeze"], capture_output=True,
                              text=True).stdout.split()
    record = {
        "note": "host execution; the Linux side of a comparison, not native evidence",
        "engine": {"path": str(engine.relative_to(ROOT)), "sha256": sha256(engine),
                   "source": "engine/thalyx-engine.cpp at the pinned Thalyx revision, unchanged"},
        "model": {"path": str(model.relative_to(ROOT)), "sha256": sha256(model),
                  "bytes": model.stat().st_size,
                  "source": "dev/tiny-model.py at the pinned Thalyx revision, unchanged"},
        "python": {"version": subprocess.run([str(python), "--version"], capture_output=True,
                                             text=True).stdout.strip(),
                   "packages": packages},
        "compiler": subprocess.run(["g++", "--version"], capture_output=True,
                                   text=True).stdout.splitlines()[0],
        "flags": HOST_MACHINE,
    }
    (OUT / "reference.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({k: v for k, v in record.items() if k != "python"}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
