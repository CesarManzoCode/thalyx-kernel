#!/usr/bin/env python3
"""Record the physical machine the virtual one runs on, as it reports itself.

`vault/architecture/hardware.md` asks that first physical acceptance inventory
the CPU, firmware, RAM, IOMMU groups and storage of a concrete machine. This
writes that inventory for the one physical machine this work has: the
development host. It is an inventory and not a bring-up. Nothing here boots
this kernel on the hardware it describes; it records what the KVM platform's
guest instructions actually execute on, so a KVM result names the silicon it
came from, and what a later bare-metal attempt would have to contend with.

Everything is read from files an unprivileged user can read. What cannot be
read is recorded as unreadable, not guessed.

Usage: tools/inventory_host.py [--out build/k6/host-inventory.json]
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

sys.path.insert(0, str(Path(__file__).resolve().parent))
import toolchain as tc  # noqa: E402


def read(path: str) -> str | None:
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def cpu() -> dict:
    info: dict = {}
    text = read("/proc/cpuinfo") or ""
    first = text.split("\n\n", 1)[0]
    for line in first.splitlines():
        key, _, value = line.partition(":")
        key = key.strip()
        if key in ("vendor_id", "model name", "cpu family", "model", "stepping", "microcode",
                   "cpu MHz", "cache size", "siblings", "cpu cores"):
            info[key.replace(" ", "_")] = value.strip()
        if key == "flags":
            flags = set(value.split())
            info["features_of_interest"] = {
                name: name in flags
                for name in ("svm", "vmx", "smep", "smap", "x2apic", "pdpe1gb", "invariant_tsc",
                             "constant_tsc", "nonstop_tsc", "avx", "avx2", "xsave", "sse4_2", "pcid",
                             "invpcid")
            }
            # `invariant_tsc` is not a /proc flag name; the pair that implies it is.
            info["features_of_interest"]["invariant_tsc"] = (
                "constant_tsc" in flags and "nonstop_tsc" in flags)
    info["logical_processors"] = os.cpu_count()
    siblings = set()
    for entry in Path("/sys/devices/system/cpu").glob("cpu[0-9]*/topology/thread_siblings_list"):
        siblings.add(entry.read_text().strip())
    info["smt_sibling_groups"] = sorted(siblings)
    info["governor"] = read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    info["boost"] = read("/sys/devices/system/cpu/cpufreq/boost")
    vulnerabilities = {}
    for entry in sorted(Path("/sys/devices/system/cpu/vulnerabilities").glob("*")):
        vulnerabilities[entry.name] = read(str(entry))
    info["host_vulnerabilities"] = vulnerabilities
    return info


def memory() -> dict:
    out = {}
    for line in (read("/proc/meminfo") or "").splitlines():
        key, _, value = line.partition(":")
        if key in ("MemTotal", "MemAvailable", "SwapTotal"):
            out[key] = value.strip()
    return out


def firmware() -> dict:
    dmi = {}
    for name in ("bios_vendor", "bios_version", "bios_date", "board_vendor", "board_name",
                 "board_version", "sys_vendor", "product_name", "product_version"):
        dmi[name] = read(f"/sys/class/dmi/id/{name}")
    return {
        "boot": "uefi" if Path("/sys/firmware/efi").is_dir() else "legacy_or_unknown",
        "secure_boot_efivar_present": any(Path("/sys/firmware/efi/efivars").glob("SecureBoot-*"))
        if Path("/sys/firmware/efi/efivars").is_dir() else False,
        "dmi": dmi,
    }


def iommu() -> dict:
    groups = {}
    base = Path("/sys/kernel/iommu_groups")
    if base.is_dir():
        for group in sorted(base.iterdir(), key=lambda p: int(p.name) if p.name.isdigit() else 0):
            devices = []
            for device in sorted((group / "devices").glob("*")):
                vendor = read(str(device / "vendor"))
                dev = read(str(device / "device"))
                klass = read(str(device / "class"))
                devices.append({"address": device.name, "vendor": vendor, "device": dev,
                                "class": klass})
            groups[group.name] = devices
    return {
        "groups": len(groups),
        "iommu_units": sorted(p.name for p in Path("/sys/class/iommu").glob("*"))
        if Path("/sys/class/iommu").is_dir() else [],
        "group_devices": groups,
    }


def storage() -> list[dict]:
    out = []
    for block in sorted(Path("/sys/block").glob("*")):
        if block.name.startswith(("loop", "ram", "zram", "dm-")):
            continue
        out.append({
            "name": block.name,
            "model": read(str(block / "device/model")),
            "sectors": read(str(block / "size")),
            "rotational": read(str(block / "queue/rotational")),
            "write_cache": read(str(block / "queue/write_cache")),
            "fua": read(str(block / "queue/fua")),
        })
    return out


def kvm() -> dict:
    out = {"device": os.access("/dev/kvm", os.R_OK | os.W_OK)}
    for module in ("kvm_amd", "kvm_intel"):
        params = Path(f"/sys/module/{module}/parameters")
        if params.is_dir():
            out["module"] = module
            out["parameters"] = {p.name: read(str(p)) for p in sorted(params.glob("*"))
                                 if p.name in ("npt", "avic", "nested", "ept", "enable_apicv",
                                               "vls", "vgif")}
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "build/k6/host-inventory.json")
    arguments = parser.parse_args()
    try:
        tools = tc.resolve()
        qemu = tools.describe()["qemu"]
    except tc.MissingTool as error:
        qemu = {"unavailable": str(error)}
    inventory = {
        "note": "an inventory of the development host, the physical machine under the KVM "
                "platform; this kernel was not booted on it directly",
        "kernel": {"release": platform.release(), "version": platform.version()},
        "cpu": cpu(),
        "memory": memory(),
        "firmware": firmware(),
        "iommu": iommu(),
        "storage": storage(),
        "kvm": kvm(),
        "qemu": qemu,
        "load_average": read("/proc/loadavg"),
    }
    arguments.out.parent.mkdir(parents=True, exist_ok=True)
    arguments.out.write_text(json.dumps(inventory, indent=2) + "\n")
    summary = {k: inventory[k] for k in ("kernel", "memory", "kvm")}
    summary["cpu"] = inventory["cpu"].get("model_name")
    summary["iommu_groups"] = inventory["iommu"]["groups"]
    summary["storage"] = [s["model"] for s in inventory["storage"]]
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
