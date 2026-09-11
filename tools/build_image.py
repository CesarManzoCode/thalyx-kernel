#!/usr/bin/env python3
"""Build a Thalyx boot image.

Produces, from sources only:

  build/thalyx-<phase>/kernel.elf   the kernel image
  build/thalyx-<phase>/boot.tbp     the boot package
  build/thalyx-<phase>.img          a FAT-formatted EFI system partition image

The kernel is the same binary in every phase. What differs is the package, and
the package is what selects the kernel's boot path: a module declared
`SUPERVISOR` takes the K2 route, and its absence keeps the K1 domains. Building
both from one script is deliberate -- if the K2 image were produced by a
different toolchain path, "K1 still passes" would be a claim about two kernels.

The K1 package carries one deliberately malformed module: a copy of a valid
image whose first loadable segment claims a kernel address. Nothing in the
kernel is written to recognise it. It is there so that the run shows the
validator refusing an image on the same code path that accepts the others,
rather than only showing acceptance.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import toolchain as tc  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"

KERNEL_TARGET = "x86_64-unknown-none"
LOADER_TARGET = "x86_64-unknown-uefi"

# Mirrors `thalyx_boot_protocol::package`. The two definitions are checked
# against each other by the loader, which refuses a header whose declared sizes
# do not match its own.
PACKAGE_MAGIC = b"THLXPKG0"
PACKAGE_VERSION = (0, 1)
PACKAGE_HEADER_LEN = 40
PACKAGE_ENTRY_LEN = 64
MODULE_KIND_USER_ELF = 1
MODULE_KIND_SUPERVISOR = 2
MODULE_FLAG_EXPECT_REJECT = 1
MODULE_FLAG_TRACE_OFF = 2

# Mirrors `thalyx_boot_protocol::USER_MAX_ADDR`: the patched module's segment is
# placed above it so the kernel's range check is what rejects the image.
KERNEL_RANGE_ADDRESS = 0xFFFFFFFF80000000

# Fixed FAT volume serial and build timestamp. Both are what make the image a
# function of its inputs rather than of the clock: mtools stamps directory
# entries with the current time, and rust-lld stamps the loader's PE header
# with it. Both read SOURCE_DATE_EPOCH. 1980-01-01T00:00:00Z is the earliest
# instant FAT can encode.
IMAGE_SERIAL = "54484c58"
IMAGE_EPOCH = 315532800

# Programs built for the K1 package, and the domains instantiated from them.
# `worker` appears twice on purpose: two surviving domains keep preemption
# observable after every faulting domain is gone.
K1_PROGRAMS = ["worker", "trespasser", "wxprobe"]
K1_INSTANCES = [
    ("worker-a", "worker"),
    ("worker-b", "worker"),
    ("trespasser", "trespasser"),
    ("wxprobe", "wxprobe"),
]

# Programs built for the K2 package. Exactly one carries the supervisor kind:
# the kernel refuses a package that declares more than one, because "the first
# supervisor" has to be a single answer.
#
# The module names are what the supervisor matches on. It is not told which slot
# holds which image; it asks each sealed object for its label, so these names
# are part of the interface between the package and the program, not a layout
# the program assumes.
K2_PROGRAMS = ["k2super", "k2server", "k2client"]
K2_INSTANCES = [
    ("k2super", "k2super", MODULE_KIND_SUPERVISOR),
    ("k2server", "k2server", MODULE_KIND_USER_ELF),
    ("k2client", "k2client", MODULE_KIND_USER_ELF),
]

# Programs built for the K3 package. One worker image serves four roles: the
# supervisor writes each worker's role into a page and maps it read-only, so a
# domain cannot choose to be a different one. Building four images instead
# would have made "the role is not the program's choice" a claim about the
# build rather than about the kernel.
K3_PROGRAMS = ["k3super", "k3worker", "k3driver"]
K3_INSTANCES = [
    ("k3super", "k3super", MODULE_KIND_SUPERVISOR),
    ("k3worker", "k3worker", MODULE_KIND_USER_ELF),
    ("k3driver", "k3driver", MODULE_KIND_USER_ELF),
]


# Programs built for the K4 package. One client image serves four roles, as in
# K3: the supervisor writes each client's role into a page and maps it
# read-only. The service and the driver are separate images because they are
# separate authorities -- the service has no device capability at all, and that
# is the whole reason a run can be cut at a named write.
K4_PROGRAMS = ["k4super", "k4disk", "k4store", "k4client"]
K4_INSTANCES = [
    ("k4super", "k4super", MODULE_KIND_SUPERVISOR),
    ("k4disk", "k4disk", MODULE_KIND_USER_ELF),
    ("k4store", "k4store", MODULE_KIND_USER_ELF),
    ("k4client", "k4client", MODULE_KIND_USER_ELF),
]


# Programs built for the K5 package. Two toolchains meet here: the supervisor
# and the services are Rust on the kernel's own target, and the language
# runtime, the validation tool and the inference engine are C on the
# `x86_64-thalyx` target that `tools/build_native.py` defines. The package is
# what puts them in one image; nothing about building them is evidence that any
# of them ran.
K5_PROGRAMS = ["k5super"]
# The Rust programs each stage needs beside the supervisor. The block driver and
# the state service are K4's, rebuilt from the same sources: K5's obligation is
# to put Thalyx's semantics on the durable service that exists.
K5_STAGE_PROGRAMS: dict[str, list[str]] = {
    "smoke": [],
    "surface": ["k4disk", "k4store", "k5work"],
    "work": ["k4disk", "k4store", "k5work"],
    "engine": ["k4disk", "k4store", "k5work"],
}
# What each stage's modules are called in the package, which is what the
# supervisor matches on. A K4 program keeps its own image and gets the name the
# K5 supervisor looks for.
K5_MODULE_NAMES = {"k4disk": "k5disk", "k4store": "k5store", "k5work": "k5work"}
K5_NATIVE: dict[str, list[str]] = {
    "smoke": ["nsmoke"],
    "surface": [],
    "work": ["nhacer", "ncheck"],
    "engine": ["nhacer", "ncheck", "nengine"],
}
K5_STAGES = {"smoke": 1, "surface": 2, "work": 3, "engine": 4}

# The K6 package: the native half of the paired benchmarks. One supervisor
# that builds and audits, one C program that is every role of the benchmark,
# and K5's engine with K5's model, so a benchmark of the engine measures that
# engine. The plan -- which entries, in which order, how many samples -- is
# written by tools/run_k6.py for each boot and enters the package as a module,
# as K5's plan does, so one image runs one boot of one round.
K6_PROGRAMS = ["k6super"]
K6_NATIVE = ["nbench", "nengine"]

# The model the engine stage carries: the one Thalyx's own `dev/tiny-model.py`
# writes, produced by tools/build_reference.py and pinned there. It enters the
# package as a module like any other, and the kernel seals it like any other.
REFERENCE_MODEL = BUILD / "reference" / "tiny.gguf"


def reference_model() -> Path:
    sys.path.insert(0, str(ROOT / "tools"))
    import build_reference  # noqa: E402

    if not REFERENCE_MODEL.exists() or digest(REFERENCE_MODEL) != build_reference.MODEL_SHA256:
        run([sys.executable, str(ROOT / "tools/build_reference.py")], cwd=ROOT)
    if digest(REFERENCE_MODEL) != build_reference.MODEL_SHA256:
        raise SystemExit("the reference model does not have its pinned digest")
    return REFERENCE_MODEL

# Mirrors `thalyx_user_k5pkg::native::Plan`.
PLAN_MAGIC = 0x31304E414C50354B
PLAN_FORMAT = "<QIIQQQQQ"


def build_plan(stage: str, seed: int, model_bytes: int = 0) -> bytes:
    # A sealed module is whole pages; the model is not. The host that put the
    # model in the package says how long it is, and the supervisor checks the
    # number against the object before the engine is told it.
    return struct.pack(
        PLAN_FORMAT, PLAN_MAGIC, 1, K5_STAGES[stage], seed, model_bytes, 0, 0, 0
    )


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("+", " ".join(argv), file=sys.stderr)
    result = subprocess.run(argv, **kwargs)
    if result.returncode != 0:
        raise SystemExit(f"command failed with status {result.returncode}: {' '.join(argv)}")
    return result


def config_rustflags(target: str) -> list[str]:
    """Reads the flags `.cargo/config.toml` sets for `target`.

    They are read rather than repeated because the per-target environment
    variable below replaces that table instead of adding to it, and the config
    file stays the one place where a target's correctness flags are stated.
    """
    path = ROOT / ".cargo/config.toml"
    if not path.exists():
        return []
    with path.open("rb") as handle:
        config = tomllib.load(handle)
    return list(config.get("target", {}).get(target, {}).get("rustflags", []))


def remap_flags() -> list[str]:
    """Flags that keep absolute build paths out of the images.

    Without these the same sources built in two directories, or on two
    machines, produce different digests: the compiler records its working
    directory and the standard library's source paths in debug info. The
    manifest's digests are only worth recording if they can be compared, so
    both roots are rewritten to fixed names.
    """
    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, cwd=ROOT
    ).stdout.strip()
    flags = [f"--remap-path-prefix={ROOT}=/thalyx-kernel"]
    if sysroot:
        flags.append(f"--remap-path-prefix={sysroot}=/rust")
    home = os.environ.get("CARGO_HOME") or str(Path.home() / ".cargo")
    flags.append(f"--remap-path-prefix={home}=/cargo")
    return flags


def cargo_environment(target: str) -> dict[str, str]:
    variable = "CARGO_TARGET_" + target.upper().replace("-", "_") + "_RUSTFLAGS"
    environment = dict(os.environ)
    environment[variable] = " ".join(config_rustflags(target) + remap_flags())
    return environment


def cargo_build(package: str, target: str, profile: str) -> None:
    argv = ["cargo", "build", "-p", package, "--target", target]
    if profile == "release":
        argv.append("--release")
    run(argv, cwd=ROOT, env=cargo_environment(target))


def artifact(target: str, profile: str, name: str) -> Path:
    return BUILD / "cargo" / target / profile / name


def patch_malformed(source: Path, destination: Path) -> None:
    """Copies an image and moves its first loadable segment into kernel space."""
    data = bytearray(source.read_bytes())
    phoff = struct.unpack_from("<Q", data, 32)[0]
    phentsize = struct.unpack_from("<H", data, 54)[0]
    phnum = struct.unpack_from("<H", data, 56)[0]
    for index in range(phnum):
        base = phoff + index * phentsize
        p_type = struct.unpack_from("<I", data, base)[0]
        if p_type != 1:
            continue
        struct.pack_into("<Q", data, base + 16, KERNEL_RANGE_ADDRESS)
        destination.write_bytes(bytes(data))
        return
    raise SystemExit(f"{source} has no loadable segment to patch")


def build_package(entries: list[tuple[str, Path, int, int]], destination: Path) -> None:
    """Writes a boot package: header, directory, then page-aligned payloads."""
    directory_end = PACKAGE_HEADER_LEN + len(entries) * PACKAGE_ENTRY_LEN
    offset = (directory_end + 4095) // 4096 * 4096

    records = []
    payloads = []
    for name, path, kind, flags in entries:
        payload = path.read_bytes()
        encoded = name.encode("ascii")
        if len(encoded) >= 32:
            raise SystemExit(f"module name too long: {name}")
        records.append((encoded.ljust(32, b"\0"), offset, len(payload), kind, flags))
        payloads.append((offset, payload))
        offset += (len(payload) + 4095) // 4096 * 4096

    total = offset
    blob = bytearray(total)
    struct.pack_into(
        "<8sHHIIIQQ",
        blob,
        0,
        PACKAGE_MAGIC,
        PACKAGE_VERSION[0],
        PACKAGE_VERSION[1],
        PACKAGE_HEADER_LEN,
        len(entries),
        PACKAGE_ENTRY_LEN,
        total,
        0,
    )
    for index, (name, start, length, kind, flags) in enumerate(records):
        struct.pack_into(
            "<32sQQIIQ",
            blob,
            PACKAGE_HEADER_LEN + index * PACKAGE_ENTRY_LEN,
            name,
            start,
            length,
            kind,
            flags,
            0,
        )
    for start, payload in payloads:
        blob[start : start + len(payload)] = payload
    destination.write_bytes(bytes(blob))


def build_esp(
    tools: tc.Toolchain,
    loader: Path,
    kernel: Path,
    package: Path,
    size_mib: int,
    image: Path,
    volume: str,
) -> None:
    if image.exists():
        image.unlink()
    with image.open("wb") as handle:
        handle.truncate(size_mib * 1024 * 1024)

    # mformat derives a volume serial from the clock, so it is pinned here; the
    # directory timestamps come from SOURCE_DATE_EPOCH, which `main` sets.

    def mtool(binary: Path, *args: str) -> None:
        argv = [str(binary), "-i", str(image), *args]
        print("+", " ".join(argv), file=sys.stderr)
        result = tools.run(argv, capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(f"{argv[0]} failed: {result.stderr.strip()}")

    mtool(tools.mformat, "-F", "-N", IMAGE_SERIAL, "-v", volume, "::")
    mtool(tools.mmd, "::/EFI")
    mtool(tools.mmd, "::/EFI/BOOT")
    mtool(tools.mcopy, str(loader), "::/EFI/BOOT/BOOTX64.EFI")
    mtool(tools.mmd, "::/thalyx")
    mtool(tools.mcopy, str(kernel), "::/thalyx/kernel.elf")
    mtool(tools.mcopy, str(package), "::/thalyx/boot.tbp")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", default="release", choices=["debug", "release"])
    parser.add_argument("--phase", default="k1", choices=["k1", "k2", "k3", "k4", "k5", "k6"])
    parser.add_argument("--plan", type=Path, default=None,
                        help="K6 only: the plan this boot runs, as tools/run_k6.py writes it")
    parser.add_argument("--stage", default="smoke", choices=sorted(K5_STAGES),
                        help="K5 only: which stage of the port the image runs")
    parser.add_argument("--seed", type=lambda text: int(text, 0), default=0x5EED0001,
                        help="K5 only: the seed the plan carries")
    parser.add_argument("--size-mib", type=int, default=64)
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    phase = arguments.phase
    stage = BUILD / f"thalyx-{phase}"
    image = BUILD / f"thalyx-{phase}.img"
    programs = {
        "k1": K1_PROGRAMS,
        "k2": K2_PROGRAMS,
        "k3": K3_PROGRAMS,
        "k4": K4_PROGRAMS,
        "k5": K5_PROGRAMS + K5_STAGE_PROGRAMS[arguments.stage],
        "k6": K6_PROGRAMS,
    }[phase]
    if phase == "k6" and (arguments.plan is None or not arguments.plan.exists()):
        print("the k6 phase needs --plan: a plan tools/run_k6.py wrote", file=sys.stderr)
        return 1

    # Read by rust-lld for the loader's PE timestamp and by mtools for the FAT
    # directory entries. Set before the first build so both see it.
    os.environ["SOURCE_DATE_EPOCH"] = str(IMAGE_EPOCH)

    stage.mkdir(parents=True, exist_ok=True)

    cargo_build("thalyx-boot-uefi", LOADER_TARGET, arguments.profile)
    cargo_build("thalyx-kernel", KERNEL_TARGET, arguments.profile)
    for program in programs:
        cargo_build(f"thalyx-user-{program}", KERNEL_TARGET, arguments.profile)

    loader = artifact(LOADER_TARGET, arguments.profile, "bootx64.efi")
    kernel = artifact(KERNEL_TARGET, arguments.profile, "kernel")
    shutil.copyfile(kernel, stage / "kernel.elf")

    for program in programs:
        source = artifact(KERNEL_TARGET, arguments.profile, program)
        shutil.copyfile(source, stage / f"{program}.elf")

    native_images: list[str] = []
    if phase == "k6":
        native_images = K6_NATIVE
        native = subprocess.run(
            [sys.executable, str(ROOT / "tools/build_native.py"), *native_images],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if native.returncode != 0:
            print(native.stdout, native.stderr, file=sys.stderr)
            raise SystemExit("native build failed")
        for name in native_images:
            shutil.copyfile(BUILD / "native" / f"{name}.elf", stage / f"{name}.elf")
        shutil.copyfile(arguments.plan, stage / "k6plan.bin")
        shutil.copyfile(reference_model(), stage / "k5model.gguf")
    if phase == "k5":
        native_images = K5_NATIVE[arguments.stage]
        native = subprocess.run(
            [sys.executable, str(ROOT / "tools/build_native.py"), *native_images],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if native.returncode != 0:
            print(native.stdout, native.stderr, file=sys.stderr)
            raise SystemExit("native build failed")
        for name in native_images:
            shutil.copyfile(BUILD / "native" / f"{name}.elf", stage / f"{name}.elf")
        model_bytes = reference_model().stat().st_size if arguments.stage == "engine" else 0
        (stage / "k5plan.bin").write_bytes(build_plan(arguments.stage, arguments.seed, model_bytes))

    entries: list[tuple[str, Path, int, int]] = []
    if phase == "k1":
        entries = [
            (name, stage / f"{program}.elf", MODULE_KIND_USER_ELF, 0)
            for name, program in K1_INSTANCES
        ]
        malformed = stage / "malformed.elf"
        patch_malformed(stage / "worker.elf", malformed)
        entries.append(("malformed", malformed, MODULE_KIND_USER_ELF, MODULE_FLAG_EXPECT_REJECT))
    elif phase == "k5":
        entries = [("k5super", stage / "k5super.elf", MODULE_KIND_SUPERVISOR, 0)]
        entries += [
            (K5_MODULE_NAMES[name], stage / f"{name}.elf", MODULE_KIND_USER_ELF, 0)
            for name in K5_STAGE_PROGRAMS[arguments.stage]
        ]
        entries += [
            (name, stage / f"{name}.elf", MODULE_KIND_USER_ELF, 0) for name in native_images
        ]
        entries.append(("k5plan", stage / "k5plan.bin", MODULE_KIND_USER_ELF, 0))
        if arguments.stage == "engine":
            shutil.copyfile(reference_model(), stage / "k5model.gguf")
            entries.append(("k5model", stage / "k5model.gguf", MODULE_KIND_USER_ELF, 0))
    elif phase == "k6":
        # Summaries only: under KVM one trace record costs about a thousand of
        # the operations it describes, and this package exists to time them.
        # The kernel counts what it withholds and says so in diag.summary.
        entries = [("k6super", stage / "k6super.elf", MODULE_KIND_SUPERVISOR,
                    MODULE_FLAG_TRACE_OFF)]
        entries += [
            (name, stage / f"{name}.elf", MODULE_KIND_USER_ELF, 0) for name in native_images
        ]
        entries.append(("k6plan", stage / "k6plan.bin", MODULE_KIND_USER_ELF, 0))
        entries.append(("k5model", stage / "k5model.gguf", MODULE_KIND_USER_ELF, 0))
    else:
        instances = {"k2": K2_INSTANCES, "k3": K3_INSTANCES, "k4": K4_INSTANCES}[phase]
        entries = [(name, stage / f"{program}.elf", kind, 0) for name, program, kind in instances]

    package = stage / "boot.tbp"
    build_package(entries, package)
    build_esp(
        tools,
        loader,
        stage / "kernel.elf",
        package,
        arguments.size_mib,
        image,
        f"THALYX{phase.upper()}",
    )

    manifest = {
        "phase": phase,
        "profile": arguments.profile,
        "rust": subprocess.run(
            ["rustc", "--version"], capture_output=True, text=True, cwd=ROOT
        ).stdout.strip(),
        "cargo": subprocess.run(
            ["cargo", "--version"], capture_output=True, text=True, cwd=ROOT
        ).stdout.strip(),
        "targets": {"loader": LOADER_TARGET, "kernel": KERNEL_TARGET, "user": KERNEL_TARGET},
        "artifacts": {
            "loader": {"path": str(loader.relative_to(ROOT)), "sha256": digest(loader)},
            "kernel": {
                "path": f"build/thalyx-{phase}/kernel.elf",
                "sha256": digest(stage / "kernel.elf"),
            },
            "package": {"path": f"build/thalyx-{phase}/boot.tbp", "sha256": digest(package)},
            "image": {"path": str(image.relative_to(ROOT)), "sha256": digest(image)},
        },
        "modules": [
            {
                "name": name,
                "kind": kind,
                "sha256": digest(path),
                "expect_reject": bool(flags & MODULE_FLAG_EXPECT_REJECT),
            }
            for name, path, kind, flags in entries
        ],
        "toolchain": tools.describe(),
    }
    if phase == "k5":
        manifest["stage"] = arguments.stage
        manifest["seed"] = arguments.seed
        native_manifest = BUILD / "native" / "native-manifest.json"
        if native_manifest.exists():
            manifest["native"] = json.loads(native_manifest.read_text())
    if phase == "k6":
        manifest["plan"] = {"source": str(arguments.plan), "sha256": digest(stage / "k6plan.bin")}
        native_manifest = BUILD / "native" / "native-manifest.json"
        if native_manifest.exists():
            manifest["native"] = json.loads(native_manifest.read_text())
    name = "image-manifest.json" if phase == "k1" else f"image-manifest-{phase}.json"
    (BUILD / name).write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
