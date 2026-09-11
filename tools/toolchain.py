#!/usr/bin/env python3
"""Locate the external tools K1 needs and report exactly what was found.

Nothing here guesses silently. Every tool is resolved through an explicit
environment override, then a prefix root, then the system path, and the
resolution is reported so an evidence run records which binary produced it.

Environment:
  THALYX_TOOL_PREFIX  Root of an unpacked tool tree; `<root>/usr/bin` and
                      `<root>/usr/lib` are searched and the library path is
                      extended for child processes. Use this when QEMU, OVMF
                      and mtools are not installed system-wide.
  THALYX_QEMU         Path to qemu-system-x86_64.
  THALYX_OVMF_CODE    Path to the OVMF firmware code image.
  THALYX_OVMF_VARS    Path to the OVMF variable store template.
  THALYX_MFORMAT      Path to mformat.
  THALYX_MMD          Path to mmd.
  THALYX_MCOPY        Path to mcopy.
  THALYX_ACCEL        Accelerator the guest runs on: `tcg` (the default, and the
                      platform every gate's recorded evidence was gathered on) or
                      `kvm`, which runs the same image on the host processor's
                      hardware virtualization and is recorded as a different
                      platform, never as the same one.
  THALYX_CPU          Processor model that replaces each runner's own. Unset, a
                      runner uses the model its phase fixes. Set, the run record
                      says so, because a different model is a different platform.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

OVMF_CODE_CANDIDATES = [
    "usr/share/edk2/x64/OVMF_CODE.4m.fd",
    "usr/share/edk2/x64/OVMF_CODE.fd",
    "usr/share/OVMF/OVMF_CODE_4M.fd",
    "usr/share/OVMF/OVMF_CODE.fd",
    "usr/share/ovmf/x64/OVMF_CODE.fd",
    "usr/share/qemu/edk2-x86_64-code.fd",
]

OVMF_VARS_CANDIDATES = [
    "usr/share/edk2/x64/OVMF_VARS.4m.fd",
    "usr/share/edk2/x64/OVMF_VARS.fd",
    "usr/share/OVMF/OVMF_VARS_4M.fd",
    "usr/share/OVMF/OVMF_VARS.fd",
    "usr/share/ovmf/x64/OVMF_VARS.fd",
    "usr/share/qemu/edk2-i386-vars.fd",
]


class MissingTool(RuntimeError):
    """A tool K1 needs is not installed and was not pointed at."""


ACCELERATORS = ("tcg", "kvm")


def accelerator() -> str:
    """The accelerator this run uses, refused rather than downgraded.

    A run that asked for KVM on a host without it must not quietly become a TCG
    run: the two are different platforms, and evidence recorded under the wrong
    name is worse than no evidence."""
    value = os.environ.get("THALYX_ACCEL", "tcg")
    if value not in ACCELERATORS:
        raise MissingTool(f"THALYX_ACCEL={value!r} is not one of {', '.join(ACCELERATORS)}")
    if value == "kvm" and not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise MissingTool("THALYX_ACCEL=kvm but /dev/kvm is not readable and writable here")
    return value


def machine() -> str:
    """The `-machine` argument: q35 on the selected accelerator."""
    return f"q35,accel={accelerator()}"


def cpu_model(default: str) -> str:
    """The processor model: the runner's own unless `THALYX_CPU` replaces it."""
    return os.environ.get("THALYX_CPU") or default


def irqchip() -> str:
    """What provides the interrupt controllers, for the run record."""
    if accelerator() == "tcg":
        return "in-kernel-not-applicable-under-tcg"
    return "kvm, qemu default for q35"


@dataclass
class Toolchain:
    """Resolved external tools and the environment child processes need."""

    prefix: Path | None
    qemu: Path
    ovmf_code: Path
    ovmf_vars: Path
    mformat: Path
    mmd: Path
    mcopy: Path
    env: dict[str, str] = field(default_factory=dict)

    def run(self, argv: list[str], **kwargs) -> subprocess.CompletedProcess:
        """Runs a child process with the resolved library path."""
        environment = dict(os.environ)
        environment.update(self.env)
        return subprocess.run(argv, env=environment, **kwargs)

    def describe(self) -> dict:
        """Identity of every resolved tool, for the evidence manifest."""
        return {
            "prefix": str(self.prefix) if self.prefix else None,
            "qemu": {"path": str(self.qemu), "version": _version(self, [str(self.qemu), "--version"])},
            "ovmf_code": {"path": str(self.ovmf_code), "sha256": _digest(self.ovmf_code)},
            "ovmf_vars": {"path": str(self.ovmf_vars), "sha256": _digest(self.ovmf_vars)},
            "mtools": {
                "mformat": str(self.mformat),
                "version": _version(self, [str(self.mformat), "--version"]),
            },
        }


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _version(toolchain: Toolchain, argv: list[str]) -> str:
    try:
        result = toolchain.run(argv, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError) as error:
        return f"unavailable: {error}"
    output = (result.stdout or result.stderr).strip().splitlines()
    return output[0] if output else "unknown"


def _find_binary(name: str, override: str, prefix: Path | None) -> Path:
    explicit = os.environ.get(override)
    if explicit:
        path = Path(explicit)
        if not path.exists():
            raise MissingTool(f"{override}={explicit} does not exist")
        return path
    if prefix is not None:
        for relative in (f"usr/bin/{name}", f"usr/sbin/{name}", f"bin/{name}"):
            candidate = prefix / relative
            if candidate.exists():
                return candidate
    found = shutil.which(name)
    if found:
        return Path(found)
    raise MissingTool(
        f"{name} not found. Install it, or set {override}, or set THALYX_TOOL_PREFIX "
        f"to a tree containing usr/bin/{name}."
    )


def _find_firmware(candidates: list[str], override: str, prefix: Path | None) -> Path:
    explicit = os.environ.get(override)
    if explicit:
        path = Path(explicit)
        if not path.exists():
            raise MissingTool(f"{override}={explicit} does not exist")
        return path
    roots = [prefix] if prefix is not None else []
    roots.append(Path("/"))
    for root in roots:
        for relative in candidates:
            candidate = root / relative
            if candidate.exists():
                return candidate
    raise MissingTool(
        f"OVMF firmware not found. Install an edk2/OVMF package, or set {override}, "
        f"or set THALYX_TOOL_PREFIX to a tree containing one of: {', '.join(candidates)}"
    )


def resolve() -> Toolchain:
    """Resolves every external tool or raises [`MissingTool`]."""
    prefix_value = os.environ.get("THALYX_TOOL_PREFIX")
    prefix = Path(prefix_value).resolve() if prefix_value else None
    if prefix is not None and not prefix.is_dir():
        raise MissingTool(f"THALYX_TOOL_PREFIX={prefix_value} is not a directory")

    env: dict[str, str] = {}
    if prefix is not None:
        library = prefix / "usr/lib"
        if library.is_dir():
            existing = os.environ.get("LD_LIBRARY_PATH", "")
            env["LD_LIBRARY_PATH"] = f"{library}:{existing}" if existing else str(library)

    return Toolchain(
        prefix=prefix,
        qemu=_find_binary("qemu-system-x86_64", "THALYX_QEMU", prefix),
        ovmf_code=_find_firmware(OVMF_CODE_CANDIDATES, "THALYX_OVMF_CODE", prefix),
        ovmf_vars=_find_firmware(OVMF_VARS_CANDIDATES, "THALYX_OVMF_VARS", prefix),
        mformat=_find_binary("mformat", "THALYX_MFORMAT", prefix),
        mmd=_find_binary("mmd", "THALYX_MMD", prefix),
        mcopy=_find_binary("mcopy", "THALYX_MCOPY", prefix),
        env=env,
    )


def main() -> int:
    import json

    try:
        toolchain = resolve()
    except MissingTool as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps(toolchain.describe(), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
