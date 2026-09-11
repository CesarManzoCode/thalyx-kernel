#!/usr/bin/env python3
"""Fetch the reference inference workload, pinned and checked.

The workload K5's `engine` stage has to run is the one the Thalyx revision this
port is against actually runs, and nothing else:

  * **llama.cpp at tag `b10665`.** Thalyx's `dev/build-engine.sh` pins exactly
    this tag and builds its engine against it. A port against another tag would
    be a port of an engine nobody uses.
  * **`engine/thalyx-engine.cpp`** from that Thalyx revision: the resident
    engine, which loads the weights once and answers requests until it is told
    to stop. The native engine in `user/nengine` is a port of its `serve_one`,
    and the host comparison builds this file unchanged.
  * **`dev/tiny-model.py`** from the same revision: the model Thalyx itself runs
    its engine against, written with llama.cpp's own `gguf-py` so that the only
    opinion about GGUF in the loop is llama.cpp's.

All three are **fetched rather than vendored**, for the reason QuickJS is:
`vault/roadmap/open-questions.md` OQ-14 says the distribution licence and the
contribution policy are fixed *before* third-party code enters this repository,
and that question is still open. Fetching keeps the code out of the tree while
keeping the build exact: every byte used is checked against a digest written
here before anything is compiled or run.

The llama.cpp archive is checked twice. GitHub regenerates tag archives on its
own schedule and has changed their compression before without changing a file
inside, so the archive digest can move while the source does not. The tree
digest -- over the sorted paths and contents of every file this build uses --
is what cannot move, and it is the one that decides.

Nothing here is executed. What runs natively is the image the cross-toolchain
produces from these sources, inside the guest.

Usage: tools/fetch_engine.py [--into build/vendor]
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CACHE = Path.home() / ".cache" / "thalyx-tools"

LLAMA_TAG = "b10665"
LLAMA_URL = f"https://github.com/ggml-org/llama.cpp/archive/refs/tags/{LLAMA_TAG}.tar.gz"
LLAMA_ARCHIVE_SHA256 = "0f185f32031c7df04144eb27f1f8166a478c02d46a942f5b473bbcf7981827d5"
# Over the files kept below, sorted by path: "path\0sha256\n" for each.
LLAMA_TREE_SHA256 = "4e623890a44101bdc55674b90ab9eea9fce7ef5952ad2f904f58d32106e4f93c"

# What this build uses of the checkout. The server, the examples, the tests and
# every backend other than the CPU one stay out of the tree the build sees.
LLAMA_KEEP = (
    "LICENSE",
    "include/",
    "src/",
    "common/",
    "vendor/nlohmann/",
    "vendor/sheredom/",
    "ggml/include/",
    "ggml/src/ggml-cpu/",
    "gguf-py/",
)
# ggml's own sources sit directly under this directory, beside one directory
# per accelerator backend; only the files are kept.
LLAMA_KEEP_FILES_IN = ("ggml/src/",)
# ...minus what the CPU backend has for other architectures and accelerators.
LLAMA_DROP = (
    "ggml/src/ggml-cpu/arch/arm/",
    "ggml/src/ggml-cpu/arch/loongarch/",
    "ggml/src/ggml-cpu/arch/powerpc/",
    "ggml/src/ggml-cpu/arch/riscv/",
    "ggml/src/ggml-cpu/arch/s390/",
    "ggml/src/ggml-cpu/arch/wasm/",
    "ggml/src/ggml-cpu/kleidiai/",
    "ggml/src/ggml-cpu/spacemit/",
)

# The port's changes to llama.cpp, all of them. Each is an exact text the file
# must contain and what replaces it; a file that does not contain the text is a
# different llama.cpp and the fetch stops.
#
# There is one. Two functions in `common/common.cpp` find where to keep
# downloaded models and configuration by naming every operating system they
# know and failing to compile on any other. This system has no home directory,
# no environment and no writable filesystem, so the port adds it to the list
# with the answer the same code already gives when a home directory cannot be
# found: an exception saying so. Neither function is on the engine's path.
_NO_PLACE = (
    "#elif defined(__thalyx__)\n"
    "        // Thalyx-Kernel port: no home, no environment, nothing writable.\n"
    "        throw std::runtime_error(\"no {what} directory on this platform\");\n"
    "#else\n"
    "#  error Unknown architecture\n"
)
LLAMA_PATCHES = [
    (
        "common/common.cpp",
        "        GGML_ABORT(\"not implemented on this platform\");\n#else\n#  error Unknown architecture\n",
        "        GGML_ABORT(\"not implemented on this platform\");\n" + _NO_PLACE.format(what="cache"),
    ),
    (
        "common/common.cpp",
        "    throw std::runtime_error(\"not implemented on this platform\");\n#else\n#  error Unknown architecture\n",
        "    throw std::runtime_error(\"not implemented on this platform\");\n"
        + _NO_PLACE.format(what="config").replace("        ", "    "),
    ),
]

THALYX_REVISION = "0492f72e487e2463b0d7b938365a8b3383364cb9"
THALYX_RAW = f"https://raw.githubusercontent.com/CesarManzoCode/thalyx/{THALYX_REVISION}"
THALYX_FILES = {
    "engine/thalyx-engine.cpp": "9e95201635ebe74442c3be9d6ef8abf99bc10fc3946455a72ef4c55f5321c709",
    "dev/tiny-model.py": "c8579a8e9d102c5204a4405f32ebf865e3efce9d9c77da0ea307ef45d16822e4",
}


def curl(url: str, into: Path) -> None:
    print(f"+ fetching {url}", file=sys.stderr)
    result = subprocess.run(["curl", "-sSfL", "-o", str(into), url],
                            capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"could not fetch {url}: {result.stderr.strip()}")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def kept(name: str) -> bool:
    if any(name.startswith(prefix) for prefix in LLAMA_DROP):
        return False
    for directory in LLAMA_KEEP_FILES_IN:
        if name.startswith(directory) and "/" not in name[len(directory):]:
            return True
    return any(name == prefix or name.startswith(prefix) for prefix in LLAMA_KEEP)


def tree_digest(files: dict[str, str]) -> str:
    lines = "".join(f"{path}\0{digest}\n" for path, digest in sorted(files.items()))
    return sha256(lines.encode())


def fetch_llama(into: Path) -> dict:
    archive = CACHE / "llama.cpp" / f"{LLAMA_TAG}.tar.gz"
    archive.parent.mkdir(parents=True, exist_ok=True)
    if not archive.exists():
        curl(LLAMA_URL, archive)
    archive_digest = sha256(archive.read_bytes())

    if into.exists():
        shutil.rmtree(into)
    into.mkdir(parents=True)
    files: dict[str, str] = {}
    prefix = f"llama.cpp-{LLAMA_TAG}/"
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            name = member.name[len(prefix):]
            if not kept(name):
                continue
            extracted = tar.extractfile(member)
            if extracted is None:
                continue
            data = extracted.read()
            target = into / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            files[name] = sha256(data)

    digest = tree_digest(files)
    if LLAMA_TREE_SHA256 is not None and digest != LLAMA_TREE_SHA256:
        shutil.rmtree(into)
        raise SystemExit(
            f"llama.cpp {LLAMA_TAG}: the tree hashes to {digest}, not {LLAMA_TREE_SHA256}; "
            "refusing to build from it"
        )
    if archive_digest != LLAMA_ARCHIVE_SHA256:
        # The tree decides. Said, so a moved archive is a fact on the record
        # rather than a surprise the next time somebody compares.
        print(f"note: the archive now hashes to {archive_digest} (pinned "
              f"{LLAMA_ARCHIVE_SHA256}); the tree digest matched", file=sys.stderr)

    for relative, old, new in LLAMA_PATCHES:
        target = into / relative
        text = target.read_text()
        if text.count(old) != 1:
            raise SystemExit(f"{relative} does not contain the text a port patch replaces")
        text = text.replace(old, new)
        target.write_text(text)
        files[relative] = sha256(text.encode())
    return {
        "tag": LLAMA_TAG,
        "url": LLAMA_URL,
        "archive_sha256": archive_digest,
        "archive_sha256_pinned": LLAMA_ARCHIVE_SHA256,
        "tree_sha256": digest,
        "files": len(files),
        "port_patches": [
            {"file": relative, "replaces_sha256": sha256(old.encode()),
             "with_sha256": sha256(new.encode())}
            for relative, old, new in LLAMA_PATCHES
        ],
        "patched_tree_sha256": tree_digest(files),
    }


def fetch_thalyx(into: Path) -> dict:
    record = {}
    for relative, pinned in THALYX_FILES.items():
        cached = CACHE / "thalyx" / THALYX_REVISION / relative
        cached.parent.mkdir(parents=True, exist_ok=True)
        if not cached.exists():
            curl(f"{THALYX_RAW}/{relative}", cached)
        data = cached.read_bytes()
        digest = sha256(data)
        if digest != pinned:
            cached.unlink()
            raise SystemExit(f"{relative} at {THALYX_REVISION} hashes to {digest}, not {pinned}")
        target = into / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        record[relative] = digest
    return {"revision": THALYX_REVISION, "files": record}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--into", type=Path, default=ROOT / "build/vendor")
    arguments = parser.parse_args()

    record = {
        "llama.cpp": fetch_llama(arguments.into / "llama.cpp"),
        "thalyx": fetch_thalyx(arguments.into / "thalyx-ref"),
        "note": "fetched, not vendored; see OQ-14",
    }
    (arguments.into / "engine-manifest.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, indent=2))
    _ = io
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
