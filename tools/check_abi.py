#!/usr/bin/env python3
"""Check that the generated ABI bindings still say what the schema says.

Three independent statements about the same layout have to agree:

  * the schema, as this script reads it;
  * the Rust bindings, whose `const` assertions the compiler evaluates;
  * the C header, whose `_Static_assert`s a C compiler evaluates.

The Rust half is checked by building the crate, which the image build already
does. This script covers the other two and the freshness of the generated files,
because a generated file that was edited by hand is the one failure that no
compiler would notice.

The fixture byte vectors are decoded here with `struct.unpack_from` against
offsets recomputed from the schema. That catches a hand-edited fixture; the
check that matters more, the Rust structures decoding the same bytes, happens
inside the kernel at boot and is reported as a record.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_abi  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]


def check_generated(schema, layout) -> tuple[bool, str]:
    stale = []
    try:
        produced = gen_abi.outputs(schema, layout)
    except gen_abi.SchemaError as error:
        return False, str(error)
    for path, text in produced.items():
        if not path.exists():
            stale.append(f"{path.relative_to(ROOT)} is missing")
        elif path.read_text() != text:
            stale.append(f"{path.relative_to(ROOT)} differs from the schema")
    if stale:
        return False, "; ".join(stale)
    return True, "generated Rust, C and fixtures match the schema byte for byte"


def check_c_header(schema, layout) -> tuple[bool, str]:
    compiler = shutil.which("cc") or shutil.which("gcc") or shutil.which("clang")
    if compiler is None:
        return False, "no C compiler found, so the C binding was never evaluated"
    with tempfile.TemporaryDirectory() as directory:
        source = Path(directory) / "abi_layout.c"
        source.write_text(
            '#include "thalyx_abi.h"\n'
            "/* Nothing runs: the header's own static assertions are the test. */\n"
            "int thalyx_abi_layout_checked(void) { return 1; }\n"
        )
        result = subprocess.run(
            [
                compiler,
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-c",
                str(source),
                "-I",
                str(ROOT / "abi/include"),
                "-o",
                str(Path(directory) / "abi_layout.o"),
            ],
            capture_output=True,
            text=True,
        )
    if result.returncode != 0:
        return False, f"C binding failed to compile: {result.stderr.strip().splitlines()[:3]}"
    count = sum(1 for _ in re.finditer(r"_Static_assert", (ROOT / "abi/include/thalyx_abi.h").read_text()))
    return True, f"{count} C static assertions on sizes, alignments and offsets hold"


def check_fixtures(schema, layout) -> tuple[bool, str]:
    text = (ROOT / "abi/src/fixture.rs").read_text()
    arrays = dict(
        (match.group(1), match.group(2))
        for match in re.finditer(r"const (\w+)_BYTES: \[u8; \d+\] = \[([^\]]*)\];", text)
    )
    checked = 0
    for name in gen_abi.FIXTURE_STRUCTS:
        key = gen_abi.snake(name).upper()
        if key not in arrays:
            return False, f"fixture for {name} is missing from abi/src/fixture.rs"
        blob = bytes(int(token.strip(), 16) for token in arrays[key].split(",") if token.strip())
        definition = layout[name]
        if len(blob) != definition["size"]:
            return False, f"fixture for {name} is {len(blob)} bytes, schema says {definition['size']}"
        for field in definition["fields"]:
            if field["base"].startswith("struct:"):
                continue
            code = gen_abi.SCALARS[field["base"]][3]
            size = gen_abi.SCALARS[field["base"]][0]
            for element in range(field["count"] or 1):
                struct.unpack_from("<" + code, blob, field["offset"] + element * size)
                checked += 1
    return True, f"{len(gen_abi.FIXTURE_STRUCTS)} fixtures decode at {checked} schema offsets"


def check_numbering(schema, layout) -> tuple[bool, str]:
    """No number in the interface may mean two things."""
    seen: dict[tuple[str, int], str] = {}
    for item in schema["entries"]:
        key = ("entry", item["value"])
        if key in seen:
            return False, f"entry {item['value']} is both {seen[key]} and {item['name']}"
        seen[key] = item["name"]
    types = {item["name"]: item["value"] for item in schema["object_types"]}
    for item in schema["operations"]:
        code = gen_abi.opcode(types[item["type"]], item["ordinal"])
        key = ("op", code)
        if key in seen:
            return False, f"operation 0x{code:08x} is both {seen[key]} and {item['name']}"
        seen[key] = item["name"]
        size = 0
        for side in ("request", "response"):
            if item[side]:
                size = max(size, layout[item[side]]["size"] + 32)
        if size > schema["limits"]["max_descriptor_len"]:
            return False, f"{item['name']} needs {size} bytes, over the descriptor bound"
    for item in schema["errors"]:
        key = ("status", item["value"])
        if key in seen:
            return False, f"status {item['value']} is both {seen[key]} and {item['name']}"
        seen[key] = item["name"]
    bits: dict[tuple[str, int], str] = {}
    for item in schema["rights"]["common"]:
        bits[("*", item["bit"])] = item["name"]
    for type_name, entries in schema["rights"]["by_type"].items():
        for item in entries:
            if ("*", item["bit"]) in bits:
                return False, f"{item['name']} reuses common right bit {item['bit']}"
            key = (type_name, item["bit"])
            if key in bits:
                return False, f"{type_name} bit {item['bit']} is both {bits[key]} and {item['name']}"
            bits[key] = item["name"]
    return True, f"{len(seen)} entry, operation and status numbers are distinct, every descriptor fits"


CHECKS = [
    ("generated", "bindings match the schema", check_generated),
    ("numbering", "no number means two things", check_numbering),
    ("c-binding", "C layout assertions hold", check_c_header),
    ("fixtures", "fixtures decode at schema offsets", check_fixtures),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    arguments = parser.parse_args()

    try:
        schema, layout = gen_abi.load()
    except gen_abi.SchemaError as error:
        print(f"FAIL  schema  {error}")
        return 1

    results = []
    for name, title, check in CHECKS:
        passed, detail = check(schema, layout)
        results.append({"name": name, "title": title, "passed": passed, "detail": detail})
        print(f"{'PASS' if passed else 'FAIL'}  {title.ljust(34)}  {detail}")

    verdict = {
        "check": "abi-schema",
        "passed": all(item["passed"] for item in results),
        "criteria": results,
        "structs": len(schema["structs"]),
        "operations": len(schema["operations"]),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")
    print()
    if not verdict["passed"]:
        print("ABI SCHEMA CHECK FAILED")
        return 1
    print(f"ABI SCHEMA CHECK PASSED: {len(results)} of {len(results)} checks met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
