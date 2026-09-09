#!/usr/bin/env python3
"""Build the K1 boot image.

Produces, from sources only:

  build/thalyx-k1/kernel.elf     the kernel image
  build/thalyx-k1/boot.tbp       the initial boot package
  build/thalyx-k1.img            a FAT-formatted EFI system partition image

The package is the initial set of user domains. It carries one deliberately
malformed module: a copy of a valid image whose first loadable segment claims a
kernel address. Nothing in the kernel is written to recognise it. It is there so
that the run shows the validator refusing an image on the same code path that
accepts the others, rather than only showing acceptance.
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
STAGE = BUILD / "thalyx-k1"
IMAGE = BUILD / "thalyx-k1.img"

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
MODULE_FLAG_EXPECT_REJECT = 1

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

# Programs built for the initial package, and the domains instantiated from
# them. `worker` appears twice on purpose: two surviving domains keep preemption
# observable after every faulting domain is gone.
USER_PROGRAMS = ["worker", "trespasser", "wxprobe"]
MODULE_INSTANCES = [
    ("worker-a", "worker"),
    ("worker-b", "worker"),
    ("trespasser", "trespasser"),
    ("wxprobe", "wxprobe"),
]


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


def build_package(entries: list[tuple[str, Path, int]], destination: Path) -> None:
    """Writes a boot package: header, directory, then page-aligned payloads."""
    directory_end = PACKAGE_HEADER_LEN + len(entries) * PACKAGE_ENTRY_LEN
    offset = (directory_end + 4095) // 4096 * 4096

    records = []
    payloads = []
    for name, path, flags in entries:
        payload = path.read_bytes()
        encoded = name.encode("ascii")
        if len(encoded) >= 32:
            raise SystemExit(f"module name too long: {name}")
        records.append((encoded.ljust(32, b"\0"), offset, len(payload), flags))
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
    for index, (name, start, length, flags) in enumerate(records):
        struct.pack_into(
            "<32sQQIIQ",
            blob,
            PACKAGE_HEADER_LEN + index * PACKAGE_ENTRY_LEN,
            name,
            start,
            length,
            MODULE_KIND_USER_ELF,
            flags,
            0,
        )
    for start, payload in payloads:
        blob[start : start + len(payload)] = payload
    destination.write_bytes(bytes(blob))


def build_esp(tools: tc.Toolchain, loader: Path, kernel: Path, package: Path, size_mib: int) -> None:
    if IMAGE.exists():
        IMAGE.unlink()
    with IMAGE.open("wb") as handle:
        handle.truncate(size_mib * 1024 * 1024)

    # mformat derives a volume serial from the clock, so it is pinned here; the
    # directory timestamps come from SOURCE_DATE_EPOCH, which `main` sets.

    def mtool(binary: Path, *args: str) -> None:
        argv = [str(binary), "-i", str(IMAGE), *args]
        print("+", " ".join(argv), file=sys.stderr)
        result = tools.run(argv, capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(f"{argv[0]} failed: {result.stderr.strip()}")

    mtool(tools.mformat, "-F", "-N", IMAGE_SERIAL, "-v", "THALYXK1", "::")
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
    parser.add_argument("--size-mib", type=int, default=64)
    arguments = parser.parse_args()

    try:
        tools = tc.resolve()
    except tc.MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1

    # Read by rust-lld for the loader's PE timestamp and by mtools for the FAT
    # directory entries. Set before the first build so both see it.
    os.environ["SOURCE_DATE_EPOCH"] = str(IMAGE_EPOCH)

    STAGE.mkdir(parents=True, exist_ok=True)

    cargo_build("thalyx-boot-uefi", LOADER_TARGET, arguments.profile)
    cargo_build("thalyx-kernel", KERNEL_TARGET, arguments.profile)
    for program in USER_PROGRAMS:
        cargo_build(f"thalyx-user-{program}", KERNEL_TARGET, arguments.profile)

    loader = artifact(LOADER_TARGET, arguments.profile, "bootx64.efi")
    kernel = artifact(KERNEL_TARGET, arguments.profile, "kernel")
    shutil.copyfile(kernel, STAGE / "kernel.elf")

    for program in USER_PROGRAMS:
        source = artifact(KERNEL_TARGET, arguments.profile, program)
        shutil.copyfile(source, STAGE / f"{program}.elf")

    entries: list[tuple[str, Path, int]] = [
        (name, STAGE / f"{program}.elf", 0) for name, program in MODULE_INSTANCES
    ]

    malformed = STAGE / "malformed.elf"
    patch_malformed(STAGE / "worker.elf", malformed)
    entries.append(("malformed", malformed, MODULE_FLAG_EXPECT_REJECT))

    package = STAGE / "boot.tbp"
    build_package(entries, package)
    build_esp(tools, loader, STAGE / "kernel.elf", package, arguments.size_mib)

    manifest = {
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
            "kernel": {"path": "build/thalyx-k1/kernel.elf", "sha256": digest(STAGE / "kernel.elf")},
            "package": {"path": "build/thalyx-k1/boot.tbp", "sha256": digest(package)},
            "image": {"path": str(IMAGE.relative_to(ROOT)), "sha256": digest(IMAGE)},
        },
        "modules": [
            {
                "name": name,
                "sha256": digest(path),
                "expect_reject": bool(flags & MODULE_FLAG_EXPECT_REJECT),
            }
            for name, path, flags in entries
        ],
        "toolchain": tools.describe(),
    }
    (BUILD / "image-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
