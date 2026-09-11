#!/usr/bin/env python3
"""Build native C and C++ programs for the x86_64-thalyx target.

This is **host build tooling**. It runs a cross-compilation on the development
machine and produces ELF images that only this kernel can load; nothing it does
is evidence that anything executed. What executes natively is the image it
writes, inside the guest, and the evidence for that comes from the kernel's own
records.

The target is defined here and nowhere else, because a target is a set of
promises rather than a triple:

  * ELF64, `ET_EXEC`, static, no interpreter, no relocations, entry `_start`,
    three page-aligned `PT_LOAD` segments split R-X / R-- / RW-, plus a
    `PT_TLS` template and a `PT_GNU_EH_FRAME` index that the kernel ignores
    and the runtime inside the image uses.
  * Base 0x400000, small code model, no PIC. The kernel's bounded loader does
    not relocate.
  * SSE and SSE2 in hardware, `-mfpmath=sse`, and **no AVX**: the kernel saves
    and restores the legacy `FXSAVE` area eagerly and does not enable `XSAVE`,
    so a program using wide registers would have them silently dropped or
    leaked across a context switch. This is the one flag whose absence would be
    a correctness bug rather than a performance choice, and it is checked on
    the linked image, not assumed from the flags.
  * No stack protector in code this repository compiles. The prebuilt C++
    library does use one; its guard lives in each thread's control block at
    FS:0x28, which the runtime sets up through `THREAD_POINTER_SET`.
  * A C library that is `user/native`, and no other. `-nostdinc` is what makes
    that true rather than intended: the host's C headers are not on the search
    path, so a program that reaches for a function this system lacks fails to
    compile here instead of failing to link in the guest.

C programs are compiled freestanding, as they always were. C++ programs, and
the C sources of the libraries they link, are compiled **hosted** against the
host GCC's own libstdc++ headers, and linked against the host GCC's prebuilt
`libstdc++.a`, `libgcc_eh.a` and `libgcc.a`. Those archives were built for
glibc, so they reference glibc's interface; `user/native` implements the part
of it they reach, and the link proves the closure: every reference resolves or
the image is not produced. They are the host toolchain's, recorded by path and
digest, and producing an image with them is not running one.

The red zone is left enabled on purpose: the CPU switches to the per-processor
stack from the TSS on every interrupt and to the kernel's `syscall` stack on
every entry, so nothing writes below a user program's stack pointer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build" / "native"
NATIVE = ROOT / "user" / "native"
VENDOR = ROOT / "build" / "vendor"

TARGET = "x86_64-thalyx"

# What the target promises about generated code, shared by every language.
MACHINE = [
    "-m64",
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
    "-fno-common",
    "-fno-strict-aliasing",
    "-fno-ident",
]

CFLAGS = [
    "-std=gnu11",
    "-O2",
    "-g",
    "-ffreestanding",
    *MACHINE,
    "-fno-asynchronous-unwind-tables",
    "-fno-unwind-tables",
]

# Third-party C inside a C++ program: hosted, so the compiler may treat
# `memcpy` and `sqrtf` as what they are, with unwind tables so an exception can
# pass through its frames, and one section per function so the link keeps only
# what is reached.
HOSTED_CFLAGS = [
    "-std=gnu11",
    "-O2",
    "-g1",
    *MACHINE,
    "-funwind-tables",
    "-ffunction-sections",
    "-fdata-sections",
]

CXXFLAGS = [
    "-std=gnu++17",
    "-O2",
    "-g1",
    *MACHINE,
    "-ffunction-sections",
    "-fdata-sections",
]

# This is not Linux. Code that keys a Linux path on these macros -- `O_DIRECT`
# file reads, `/proc` probes, `fork` for a backtrace -- must not take it.
NOT_LINUX = [
    "-U__linux__", "-U__linux", "-Ulinux", "-U__gnu_linux__",
    "-U__unix__", "-U__unix", "-Uunix",
    "-D__thalyx__",
]

# The one libstdc++ header setting that `os_defines.h` only makes under
# `__linux__`: the prebuilt archive calls pthreads directly, never through weak
# references, and inline code in these headers has to agree with it.
CXX_ABI = ["-D_GLIBCXX_GTHREAD_USE_WEAK=0"]

WARNINGS = ["-Wall", "-Wextra", "-Wno-unused-parameter"]

RUNTIME_SOURCES = [
    "start.S",
    "civil.c",
    "boot.c",
    "sys.c",
    "string.c",
    "heap.c",
    "mem.c",
    "printf.c",
    "math.c",
    "conv.c",
    "time.c",
    "timefmt.c",
    "ipc.c",
    "thread.c",
    "cxxrt.c",
    "ctype.c",
    "locale.c",
    "wchar.c",
    "pthread.c",
    "file.c",
    "posix.c",
    "scanf.c",
    "dl.c",
    "fortify.c",
    "dirent.c",
    "iconv.c",
]


def gcc() -> str:
    return os.environ.get("THALYX_CC", "gcc")


def cxx() -> str:
    return os.environ.get("THALYX_CXX", "g++")


def tool_output(argv: list[str]) -> str:
    return subprocess.run(argv, capture_output=True, text=True).stdout.strip()


def freestanding_include() -> Path:
    return Path(tool_output([gcc(), "-print-file-name=include"]))


def libstdcxx_include() -> list[Path]:
    """The C++ header directories the host compiler would search, in order."""
    result = subprocess.run([cxx(), "-x", "c++", "-E", "-v", "-"], input="",
                            capture_output=True, text=True)
    dirs: list[Path] = []
    reading = False
    for line in result.stderr.splitlines():
        if line.startswith("#include <...> search starts here"):
            reading = True
            continue
        if line.startswith("End of search list"):
            break
        if reading and "/c++/" in line:
            dirs.append(Path(line.strip()))
    if not dirs:
        raise SystemExit("could not find the C++ standard library headers of " + cxx())
    return dirs


def archive_of(name: str, compiler: str) -> Path:
    path = Path(tool_output([compiler, f"-print-file-name={name}"]))
    if not path.is_absolute() or not path.exists():
        raise SystemExit(f"{compiler} has no {name}")
    return path


def includes() -> list[str]:
    return [
        "-nostdinc",
        "-isystem", str(freestanding_include()),
        "-I", str(NATIVE / "include"),
        "-I", str(ROOT / "abi" / "include"),
    ]


def hosted_includes(with_cxx: bool) -> list[str]:
    dirs: list[Path] = libstdcxx_include() if with_cxx else []
    dirs += [NATIVE / "include", freestanding_include(), ROOT / "abi" / "include"]
    argv = ["-nostdinc"]
    for directory in dirs:
        argv += ["-isystem", str(directory)]
    return argv


def run(argv: list[str], quiet: bool = False) -> None:
    if not quiet:
        print("+", " ".join(argv), file=sys.stderr)
    result = subprocess.run(argv, cwd=ROOT)
    if result.returncode != 0:
        raise SystemExit(f"command failed with status {result.returncode}: {' '.join(argv)}")


# ---------------------------------------------------------------- compiling

def dependencies(depfile: Path) -> list[Path]:
    text = depfile.read_text().replace("\\\n", " ")
    _, _, rest = text.partition(":")
    return [Path(item) for item in rest.split()]


def up_to_date(obj: Path, depfile: Path, cmdfile: Path, command: str) -> bool:
    if not (obj.exists() and depfile.exists() and cmdfile.exists()):
        return False
    if cmdfile.read_text() != command:
        return False
    built = obj.stat().st_mtime
    for dependency in dependencies(depfile):
        if not dependency.exists() or dependency.stat().st_mtime > built:
            return False
    return True


def compile_job(compiler: str, source: Path, obj: Path, flags: list[str]) -> None:
    """Compiles one unit unless the object is newer than everything it read
    and was built by the same command. Rebuilding from a clean tree produces
    the same bytes: the dependency list only decides whether to run."""
    obj.parent.mkdir(parents=True, exist_ok=True)
    depfile = obj.with_name(obj.name + ".d")
    cmdfile = obj.with_name(obj.name + ".cmd")
    argv = [compiler, "-c", str(source), "-o", str(obj), *flags,
            f"-ffile-prefix-map={ROOT}=/thalyx-kernel"]
    command = "\0".join(argv)
    if up_to_date(obj, depfile, cmdfile, command):
        return
    result = subprocess.run(argv + ["-MD", "-MF", str(depfile)], cwd=ROOT,
                            capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"{source.relative_to(ROOT)}:\n{result.stderr[-6000:]}")
    cmdfile.write_text(command)


def compile_all(jobs: list[tuple[str, Path, Path, list[str]]]) -> None:
    failures: list[str] = []
    with ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as pool:
        futures = [pool.submit(compile_job, *job) for job in jobs]
        for future in futures:
            try:
                future.result()
            except RuntimeError as error:
                failures.append(str(error))
    if failures:
        for failure in failures[:8]:
            print(failure, file=sys.stderr)
        raise SystemExit(f"{len(failures)} translation unit(s) failed to compile")


def make_archive(archive: Path, objects: list[Path]) -> Path:
    if archive.exists():
        archive.unlink()
    run(["ar", "rcsD", str(archive)] + [str(o) for o in objects], quiet=True)
    return archive


def build_runtime() -> Path:
    """The native runtime, as one archive every program links."""
    jobs = []
    objects: list[Path] = []
    for name in RUNTIME_SOURCES:
        obj = BUILD / "runtime" / (name + ".o")
        jobs.append((gcc(), NATIVE / "src" / name, obj, CFLAGS + WARNINGS + includes()))
        objects.append(obj)
    compile_all(jobs)
    return make_archive(BUILD / "libthalyx-native.a", objects)


# ------------------------------------------------------------------ QuickJS

# The language runtime is fetched rather than vendored; see tools/fetch_quickjs.py
# and OQ-14. It is compiled here for the native target with the same flags as
# everything else, plus the two the engine's own build defines.
QUICKJS_DIR = VENDOR / "quickjs"
QUICKJS_SOURCES = ["quickjs.c", "libregexp.c", "libunicode.c", "dtoa.c"]
QUICKJS_CFLAGS = ["-DNDEBUG", "-D_GNU_SOURCE", "-Wno-implicit-fallthrough",
                  "-Wno-sign-compare", "-Wno-unused-but-set-variable"]


def ensure_quickjs() -> Path:
    """Fetches the pinned runtime if it is not already unpacked."""
    if not (QUICKJS_DIR / "quickjs.c").exists():
        run([sys.executable, str(ROOT / "tools/fetch_quickjs.py")])
    return QUICKJS_DIR


def build_quickjs() -> Path:
    source_dir = ensure_quickjs()
    jobs = []
    objects = []
    for name in QUICKJS_SOURCES:
        obj = BUILD / "quickjs" / (name + ".o")
        jobs.append((gcc(), source_dir / name, obj,
                     CFLAGS + ["-w"] + includes() + QUICKJS_CFLAGS + [f"-I{source_dir}"]))
        objects.append(obj)
    compile_all(jobs)
    return make_archive(BUILD / "libquickjs.a", objects)


# ---------------------------------------------------------------- llama.cpp

# The reference engine's library, at the tag the pinned Thalyx builds against.
# Fetched and checked by tools/fetch_engine.py; see OQ-14.
LLAMA_DIR = VENDOR / "llama.cpp"
LLAMA_TAG = "b10665"
LLAMA_BUILD_NUMBER = 10665
LLAMA_COMMIT = "ca3d5a3"        # the commit tag b10665 names, abbreviated as its build does
LLAMA_VERSION = "0.3.0"
GGML_VERSION = "0.22.0"

# What llama.cpp's own CMake configure defines for this build, which is the one
# `dev/build-engine.sh` in Thalyx asks for: the CPU backend only, repacking and
# llamafile's kernels on (their defaults), no OpenMP, no native tuning.
GGML_DEFINES = [
    "-DNDEBUG",
    "-DGGML_USE_CPU",
    "-DGGML_SCHED_MAX_COPIES=4",
    "-D_XOPEN_SOURCE=600",
    "-D_GNU_SOURCE",
    f'-DGGML_VERSION="{GGML_VERSION}"',
    f'-DGGML_COMMIT="{LLAMA_COMMIT}"',
]
GGML_CPU_DEFINES = ["-DGGML_USE_CPU_REPACK", "-DGGML_USE_LLAMAFILE"]

GGML_BASE_SOURCES = [
    "ggml/src/ggml.c", "ggml/src/ggml.cpp", "ggml/src/ggml-alloc.c",
    "ggml/src/ggml-backend.cpp", "ggml/src/ggml-backend-meta.cpp", "ggml/src/ggml-opt.cpp",
    "ggml/src/ggml-threading.cpp", "ggml/src/ggml-quants.c", "ggml/src/gguf.cpp",
    "ggml/src/ggml-backend-dl.cpp", "ggml/src/ggml-backend-reg.cpp",
]
GGML_CPU_SOURCES = [
    "ggml/src/ggml-cpu/ggml-cpu.c", "ggml/src/ggml-cpu/ggml-cpu.cpp",
    "ggml/src/ggml-cpu/repack.cpp", "ggml/src/ggml-cpu/hbm.cpp",
    "ggml/src/ggml-cpu/quants.c", "ggml/src/ggml-cpu/traits.cpp",
    "ggml/src/ggml-cpu/amx/amx.cpp", "ggml/src/ggml-cpu/amx/mmq.cpp",
    "ggml/src/ggml-cpu/binary-ops.cpp", "ggml/src/ggml-cpu/unary-ops.cpp",
    "ggml/src/ggml-cpu/vec.cpp", "ggml/src/ggml-cpu/ops.cpp",
    "ggml/src/ggml-cpu/arch/x86/quants.c", "ggml/src/ggml-cpu/arch/x86/repack.cpp",
    "ggml/src/ggml-cpu/llamafile/sgemm.cpp",
]
# The part of llama.cpp's `common` library the reference engine calls and what
# that reaches. The rest -- argument parsing, downloads, a terminal console,
# subprocesses -- is about a program this is not.
COMMON_SOURCES = [
    "common/common.cpp", "common/sampling.cpp", "common/log.cpp",
    "common/reasoning-budget.cpp", "common/speculative.cpp", "common/fit.cpp",
    "common/unicode.cpp", "common/json-schema-to-grammar.cpp", "common/ngram-cache.cpp",
    "common/ngram-map.cpp", "common/ngram-mod.cpp", "common/trie.cpp",
]


def ensure_engine_sources() -> Path:
    if not (LLAMA_DIR / "src" / "llama.cpp").exists():
        run([sys.executable, str(ROOT / "tools/fetch_engine.py")])
    return LLAMA_DIR


def build_info_source() -> Path:
    """`common/build-info.cpp`, which llama.cpp's configure writes from a
    template. Written here from the same template with the same four values."""
    template = (LLAMA_DIR / "common" / "build-info.cpp.in").read_text()
    compiler = tool_output([cxx(), "--version"]).splitlines()[0]
    text = (template.replace("@LLAMA_BUILD_NUMBER@", str(LLAMA_BUILD_NUMBER))
            .replace("@LLAMA_BUILD_COMMIT@", LLAMA_COMMIT)
            .replace("@BUILD_COMPILER@", compiler)
            .replace("@BUILD_TARGET@", TARGET))
    out = BUILD / "llama" / "generated" / "build-info.cpp"
    out.parent.mkdir(parents=True, exist_ok=True)
    if not out.exists() or out.read_text() != text:
        out.write_text(text)
    return out


def build_llama() -> list[Path]:
    """ggml, llama and common as three archives, in link order."""
    source = ensure_engine_sources()
    cxx_base = CXXFLAGS + ["-w"] + NOT_LINUX + CXX_ABI + hosted_includes(True)
    c_base = HOSTED_CFLAGS + ["-w"] + NOT_LINUX + hosted_includes(False)
    ggml_inc = [f"-I{source / 'ggml/include'}", f"-I{source / 'ggml/src'}",
                f"-I{source / 'ggml/src/ggml-cpu'}"]
    llama_inc = [f"-I{source / 'include'}", f"-I{source / 'src'}", f"-I{source / 'ggml/include'}"]
    common_inc = [f"-I{source / 'common'}", f"-I{source / 'vendor'}"] + llama_inc

    def unit(relative: str, extra: list[str]) -> tuple[str, Path, Path, list[str]]:
        path = source / relative
        is_c = path.suffix == ".c"
        obj = BUILD / "llama" / (relative.replace("/", "_") + ".o")
        flags = (c_base if is_c else cxx_base) + extra
        return (gcc() if is_c else cxx(), path, obj, flags)

    groups: dict[str, list[tuple[str, Path, Path, list[str]]]] = {"ggml": [], "llama": [], "common": []}
    for relative in GGML_BASE_SOURCES:
        groups["ggml"].append(unit(relative, GGML_DEFINES + ggml_inc))
    for relative in GGML_CPU_SOURCES:
        groups["ggml"].append(unit(relative, GGML_DEFINES + GGML_CPU_DEFINES + ggml_inc))
    llama_defs = ["-DNDEBUG", "-DGGML_USE_CPU", f'-DLLAMA_VERSION="{LLAMA_VERSION}"',
                  f'-DLLAMA_COMMIT="{LLAMA_COMMIT}"']
    sources = sorted((source / "src").glob("*.cpp")) + sorted((source / "src/models").glob("*.cpp"))
    for path in sources:
        groups["llama"].append(unit(str(path.relative_to(source)), llama_defs + llama_inc))
    common_defs = ["-DNDEBUG", "-DGGML_USE_CPU"]
    for relative in COMMON_SOURCES:
        groups["common"].append(unit(relative, common_defs + common_inc))
    info = build_info_source()
    groups["common"].append((cxx(), info, BUILD / "llama" / "common_build-info.cpp.o",
                             cxx_base + common_defs + common_inc))

    compile_all([job for jobs in groups.values() for job in jobs])
    archives = []
    for name in ("common", "llama", "ggml"):
        archives.append(make_archive(BUILD / f"lib{name}.a", [job[2] for job in groups[name]]))
    return archives


# ------------------------------------------------------------------ linking

def link(name: str, objects: list[Path], archive: Path, extra_archives: list[Path],
         with_cxx: bool) -> Path:
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
        "-Wl,--eh-frame-hdr",
        "-Wl,-Map," + str(BUILD / f"{name}.map"),
        "-o",
        str(image),
    ]
    argv += [str(o) for o in objects]
    argv += [str(a) for a in extra_archives]
    if with_cxx:
        argv += [
            "-Wl,--start-group",
            str(archive_of("libstdc++.a", cxx())),
            str(archive),
            str(archive_of("libgcc_eh.a", gcc())),
            str(archive_of("libgcc.a", gcc())),
            "-Wl,--end-group",
        ]
    else:
        argv += [str(archive), "-lgcc"]
    run(argv, quiet=True)
    # The unstripped image stays beside the stripped one, so a fault address the
    # kernel records can be resolved to a line without rebuilding anything.
    shutil.copyfile(image, BUILD / f"{name}.debug.elf")
    subprocess.run(["strip", "--strip-debug", str(image)], cwd=ROOT, check=False)
    return image


# Instructions this target does not have. VEX and EVEX encodings need XSAVE
# state the kernel does not save; SSSE3 and SSE4 are not in the `qemu64` model
# the evidence runs on. A linked image containing any of them is refused.
FORBIDDEN_MNEMONIC = re.compile(
    r"^(v(?!err|erw)[a-z0-9]+|pshufb|palignr|pmaddubsw|pmulhrsw|phaddw|phaddd|phaddsw|"
    r"phsubw|phsubd|psign[bwd]|pabs[bwd]|pblendvb|blendv?p[sd]|pblendw|ptest|pmin[su][bdw]|"
    r"pmax[su][bdw]|pmulld|pmovzx[a-z]+|pmovsx[a-z]+|round[sp][sd]|insertps|extractps|"
    r"pextr[bdq]|pinsr[bdq]|dpp[sd]|pcmpeqq|packusdw|pcmpgtq|crc32[a-z]*|mpsadbw|"
    r"phminposuw|movntdqa)$"
)


def closure_report(image: Path, with_cxx: bool) -> dict:
    """What the linked image is, checked rather than assumed."""
    headers = tool_output(["readelf", "-lW", str(image)])
    sections = tool_output(["readelf", "-SW", str(image)])
    symbols = tool_output(["readelf", "-sW", str(image)])
    problems: list[str] = []
    if "INTERP" in headers:
        problems.append("the image asks for an interpreter")
    if "DYNAMIC" in headers:
        problems.append("the image has a dynamic section")
    for line in sections.splitlines():
        match = re.search(r"\]\s+(\.rela?\.\S+)\s+\S+\s+\S+\s+\S+\s+([0-9a-f]+)", line)
        if match and int(match.group(2), 16) != 0:
            problems.append(f"the image carries relocations: {match.group(1)}")
    weak_undefined = sorted({
        parts[-1] for parts in (line.split() for line in symbols.splitlines())
        if len(parts) >= 8 and parts[6] == "UND" and parts[4] == "WEAK" and parts[-1] != "UND"
    })
    strong_undefined = sorted({
        parts[-1] for parts in (line.split() for line in symbols.splitlines())
        if len(parts) >= 8 and parts[6] == "UND" and parts[4] == "GLOBAL"
    })
    disassembly = subprocess.run(["objdump", "-d", "--no-show-raw-insn", str(image)],
                                 capture_output=True, text=True).stdout
    # A static link relaxes every general-dynamic TLS access to local-exec and
    # leaves `__tls_get_addr` in the symbol table with nothing calling it. It is
    # accepted on exactly that condition, checked on the instructions.
    relaxed_tls = "__tls_get_addr" in strong_undefined and "tls_get_addr" not in disassembly
    if relaxed_tls:
        strong_undefined.remove("__tls_get_addr")
    if strong_undefined:
        problems.append(f"unresolved symbols: {', '.join(strong_undefined[:8])}")

    forbidden: dict[str, int] = {}
    for line in disassembly.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        mnemonic = parts[1].split()[0] if parts[1].split() else ""
        operands = parts[1]
        if FORBIDDEN_MNEMONIC.match(mnemonic) or "%ymm" in operands or "%zmm" in operands:
            forbidden[mnemonic] = forbidden.get(mnemonic, 0) + 1
    if forbidden:
        shown = ", ".join(f"{k}×{v}" for k, v in sorted(forbidden.items())[:10])
        problems.append(f"instructions this target does not have: {shown}")

    segments = []
    footprint = 0
    tls = None
    for line in headers.splitlines():
        parts = line.split()
        if parts and parts[0] in ("LOAD", "TLS", "GNU_EH_FRAME"):
            memsz = int(parts[5], 16)
            segments.append({"type": parts[0], "vaddr": parts[2], "memsz": memsz,
                             "flags": " ".join(parts[6:-1])})
            if parts[0] == "LOAD":
                footprint += (memsz + 4095) // 4096
            if parts[0] == "TLS":
                tls = memsz
    report = {
        "segments": segments,
        "footprint_pages": footprint,
        "tls_bytes": tls,
        "tls_calls_relaxed": relaxed_tls,
        "weak_undefined": weak_undefined,
        "problems": problems,
    }
    if with_cxx:
        report["cxx_runtime"] = {
            name: {"path": str(path), "sha256": digest(path)}
            for name, path in (("libstdc++.a", archive_of("libstdc++.a", cxx())),
                               ("libgcc_eh.a", archive_of("libgcc_eh.a", gcc())),
                               ("libgcc.a", archive_of("libgcc.a", gcc())))
        }
    return report


def describe() -> dict:
    version = tool_output([gcc(), "--version"])
    cxx_version = tool_output([cxx(), "--version"])
    return {
        "target": TARGET,
        "host_compiler": version.splitlines()[0] if version else "unknown",
        "host_cxx_compiler": cxx_version.splitlines()[0] if cxx_version else "unknown",
        "cflags": CFLAGS,
        "cxxflags": CXXFLAGS + NOT_LINUX + CXX_ABI,
        "note": "host build tooling; producing an image is not executing one",
    }


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


PROGRAMS: dict[str, dict] = {}


def embed_text(source: Path, destination: Path, symbol: str) -> None:
    """Turns a text file into a C string, so a program carries it in `.rodata`.

    The prelude is JavaScript and belongs in a `.js` file that an editor and a
    reader can treat as one; this is how it reaches the image without a
    filesystem to read it from."""
    text = source.read_bytes()
    destination.parent.mkdir(parents=True, exist_ok=True)
    lines = [f"/* Generated from {source.name}. Do not edit. */",
             f"static const char {symbol}[] ="]
    for line in text.split(b"\n"):
        escaped = (
            line.decode("utf-8")
            .replace("\\", "\\\\")
            .replace('"', '\\"')
        )
        lines.append(f'    "{escaped}\\n"')
    lines.append(";")
    rendered = "\n".join(lines) + "\n"
    if not destination.exists() or destination.read_text() != rendered:
        destination.write_text(rendered)


def register(name: str, sources: list[str], extra_cflags: list[str] | None = None,
             warnings: list[str] | None = None, extra_dirs: list[str] | None = None,
             quickjs: bool = False, embed: list[tuple[str, str, str]] | None = None,
             llama: bool = False) -> None:
    PROGRAMS[name] = {
        "sources": sources,
        "cflags": extra_cflags or [],
        "warnings": WARNINGS if warnings is None else warnings,
        "dirs": extra_dirs or [],
        "quickjs": quickjs,
        "embed": embed or [],
        "llama": llama,
    }


def build_program(name: str, archive: Path) -> tuple[Path, dict]:
    spec = PROGRAMS[name]
    generated = BUILD / name / "generated"
    for symbol, source, destination in spec["embed"]:
        embed_text(ROOT / source, generated / destination, symbol)
    extra = spec["cflags"] + [f"-I{ROOT / d}" for d in spec["dirs"]]
    if spec["embed"]:
        extra.append(f"-I{generated}")
    extras: list[Path] = []
    if spec["quickjs"]:
        extra.append(f"-I{QUICKJS_DIR}")
        extras.append(build_quickjs())
    with_cxx = any(Path(s).suffix == ".cpp" for s in spec["sources"])
    if spec["llama"]:
        extras += build_llama()
        extra += [f"-I{LLAMA_DIR / 'include'}", f"-I{LLAMA_DIR / 'ggml/include'}",
                  f"-I{LLAMA_DIR / 'common'}", f"-I{LLAMA_DIR / 'vendor'}",
                  "-DGGML_USE_CPU"]
    jobs = []
    objects: list[Path] = []
    for relative in spec["sources"]:
        source = ROOT / relative
        obj = BUILD / name / (Path(relative).name + ".o")
        if source.suffix == ".cpp":
            flags = CXXFLAGS + spec["warnings"] + NOT_LINUX + CXX_ABI + hosted_includes(True) + extra
            jobs.append((cxx(), source, obj, flags))
        else:
            jobs.append((gcc(), source, obj, CFLAGS + spec["warnings"] + includes() + extra))
        objects.append(obj)
    compile_all(jobs)
    image = link(name, objects, archive, extras, with_cxx)
    report = closure_report(image, with_cxx)
    if report["problems"]:
        for problem in report["problems"]:
            print(f"{name}: {problem}", file=sys.stderr)
        raise SystemExit(f"{name}: the linked image breaks the target's promises")
    return image, report


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
        image, report = build_program(name, archive)
        manifest["programs"][name] = {
            "path": str(image.relative_to(ROOT)),
            "sha256": digest(image),
            "bytes": image.stat().st_size,
            "closure": report,
        }
        if PROGRAMS[name]["llama"]:
            engine_manifest = VENDOR / "engine-manifest.json"
            if engine_manifest.exists():
                manifest["programs"][name]["sources"] = json.loads(engine_manifest.read_text())
    (BUILD / "native-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({name: {k: v for k, v in entry.items() if k != "closure"}
                      for name, entry in manifest["programs"].items()}, indent=2))
    return 0


register("nsmoke", ["user/nsmoke/main.c"])
register("ncheck", ["user/ncheck/main.c"], quickjs=True)
register(
    "nhacer",
    ["user/nhacer/main.c"],
    quickjs=True,
    embed=[("PRELUDE", "user/nhacer/prelude.js", "prelude.inc")],
)
register("nengine", ["user/nengine/engine.cpp"], llama=True)
# K6's paired benchmarks, native half. The other half is the same bench.c
# compiled for Linux by tools/build_k6_linux.py with this target's MACHINE
# flags, so the loop that times a primitive is the same code on both sides.
register("nbench", ["tests/k6/bench.c", "tests/k6/plat_native.c"], extra_dirs=["tests/k6"])

if __name__ == "__main__":
    raise SystemExit(main())
