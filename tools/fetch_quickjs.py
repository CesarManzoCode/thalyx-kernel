#!/usr/bin/env python3
"""Fetch the language runtime this port executes, pinned and checked.

The runtime is QuickJS, and specifically the QuickJS that the Thalyx revision
this port is against actually runs: Thalyx's `thalyx-program` crate depends on
`rquickjs 0.12`, whose `rquickjs-sys 0.12.2` vendors quickjs-ng v0.15.1. Porting
a different QuickJS would be porting a runtime nobody uses.

It is **fetched rather than vendored**, on purpose. `vault/roadmap/open-questions.md`
OQ-14 says the distribution licence and the contribution policy are to be fixed
*before* third-party code is incorporated into this repository, and that question
is still open. Fetching keeps the code out of the tree while making the build
exactly reproducible: one version, one digest, checked before anything is
compiled, and the checkout cached where the other external tools live.

Nothing here is executed. What runs natively is the image the cross-toolchain
produces from these sources, inside the guest.

Usage: tools/fetch_quickjs.py [--into build/vendor/quickjs]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

CRATE = "rquickjs-sys"
VERSION = "0.12.2"
URL = f"https://static.crates.io/crates/{CRATE}/{CRATE}-{VERSION}.crate"
SHA256 = "a13ac243b86a74120814ef7e9e30ad5a2c1199b7b9963b1cf7c84e4cdc1cad99"

# What quickjs-ng calls itself at this version, for the record a build writes.
ENGINE = "quickjs-ng 0.15.1"

# The four translation units. `cutils` is header-only at this version, which is
# why it is not among them.
SOURCES = ["quickjs.c", "libregexp.c", "libunicode.c", "dtoa.c"]

CACHE = Path.home() / ".cache" / "thalyx-tools" / "crates"


def fetch(destination: Path) -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    archive = CACHE / f"{CRATE}-{VERSION}.crate"
    if not archive.exists():
        print(f"+ fetching {URL}", file=sys.stderr)
        result = subprocess.run(
            ["curl", "-sSL", "-o", str(archive), URL], capture_output=True, text=True
        )
        if result.returncode != 0:
            raise SystemExit(f"could not fetch {URL}: {result.stderr.strip()}")
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != SHA256:
        archive.unlink()
        raise SystemExit(
            f"{archive.name} hashes to {digest}, not {SHA256}; refusing to unpack it"
        )
    return archive


def unpack(archive: Path, into: Path) -> dict:
    if into.exists():
        shutil.rmtree(into)
    into.mkdir(parents=True)
    with tarfile.open(archive, "r:gz") as tar:
        prefix = f"{CRATE}-{VERSION}/quickjs/"
        for member in tar.getmembers():
            if not member.name.startswith(prefix) or not member.isfile():
                continue
            name = member.name[len(prefix) :]
            if "/" in name:
                continue
            if not (name.endswith(".c") or name.endswith(".h") or name in ("LICENSE",)):
                continue
            extracted = tar.extractfile(member)
            if extracted is None:
                continue
            (into / name).write_bytes(extracted.read())

    files = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
             for path in sorted(into.iterdir())}
    missing = [name for name in SOURCES if name not in files]
    if missing:
        raise SystemExit(f"the crate did not carry {', '.join(missing)}")
    return files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--into", type=Path, default=ROOT / "build/vendor/quickjs")
    arguments = parser.parse_args()

    archive = fetch(arguments.into)
    files = unpack(archive, arguments.into)
    record = {
        "engine": ENGINE,
        "crate": f"{CRATE} {VERSION}",
        "url": URL,
        "sha256": SHA256,
        "sources": SOURCES,
        "files": files,
        "note": "fetched, not vendored; see OQ-14",
    }
    (arguments.into.parent / "quickjs-manifest.json").write_text(
        json.dumps(record, indent=2) + "\n"
    )
    print(json.dumps({k: v for k, v in record.items() if k != "files"}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
