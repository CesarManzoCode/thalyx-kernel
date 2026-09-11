#!/usr/bin/env python3
"""Pack a K6 plan: the bytes both backends read, laid out by the schema.

A plan is what one boot runs: which benchmark entries, in which order, with
how many samples and how much unrecorded warm-up. The native supervisor reads
it as a module of its package; the Linux guest reads it as /plan.bin. They are
the same bytes, and neither side's layout is written here by hand: the offsets
come from abi/schema/k6-bench-v1.json through the same layout function the
generators use, so the host cannot pack a plan the C and Rust structures would
read differently.
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "abi/schema/k6-bench-v1.json"

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_k5_proto as k5  # noqa: E402

FORMAT = {"u8": "B", "u16": "H", "u32": "I", "u64": "Q", "i64": "q"}


def _plan_layout() -> tuple[list[dict], int]:
    schema = json.loads(SCHEMA.read_text())
    definition = next(item for item in schema["structs"] if item["name"] == "Plan")
    fields, size, _ = k5.layout(definition)
    return fields, size


def bench_ids() -> dict[str, int]:
    schema = json.loads(SCHEMA.read_text())
    return {bench["name"]: bench["id"] for bench in schema["benchmarks"]}


def pack(entries: list[tuple[str, int, int, int]], seed: int, round_: int,
         model_bytes: int = 0) -> bytes:
    """`entries` are (benchmark name, parameter, samples, warm-up), in order."""
    schema = json.loads(SCHEMA.read_text())
    constants = schema["constants"]
    if len(entries) > constants["PLAN_ENTRIES"]:
        raise SystemExit(f"a plan holds {constants['PLAN_ENTRIES']} entries, not {len(entries)}")
    ids = bench_ids()
    values = {
        "magic": constants["PLAN_MAGIC"],
        "version": constants["PLAN_VERSION"],
        "count": len(entries),
        "seed": seed,
        "round": round_,
        "flags": 0,
        "model_bytes": model_bytes,
        "bench": [ids[name] for name, _, _, _ in entries],
        "param": [param for _, param, _, _ in entries],
        "samples": [samples for _, _, samples, _ in entries],
        "warmup": [warmup for _, _, _, warmup in entries],
    }
    fields, size = _plan_layout()
    blob = bytearray(size)
    for field in fields:
        code = FORMAT[field["base"]]
        value = values[field["name"]]
        if field["count"]:
            padded = list(value) + [0] * (field["count"] - len(value))
            struct.pack_into(f"<{field['count']}{code}", blob, field["offset"], *padded)
        else:
            struct.pack_into(f"<{code}", blob, field["offset"], value)
    return bytes(blob)


def unpack(blob: bytes) -> dict:
    fields, size = _plan_layout()
    if len(blob) < size:
        raise ValueError("short plan")
    out = {}
    for field in fields:
        code = FORMAT[field["base"]]
        if field["count"]:
            out[field["name"]] = list(struct.unpack_from(f"<{field['count']}{code}", blob,
                                                         field["offset"]))
        else:
            out[field["name"]] = struct.unpack_from(f"<{code}", blob, field["offset"])[0]
    return out
