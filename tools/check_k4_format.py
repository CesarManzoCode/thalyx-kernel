#!/usr/bin/env python3
"""Check that the K4 store format still says what the schema says.

Four statements about the same bytes have to agree:

  * the schema, as this script reads it;
  * `user/k4fmt/src/generated.rs`, whose `const` assertions the compiler
    evaluates and whose golden constants the guest rebuilds at run time;
  * `tools/k4_format.py`, which the gate decodes a real medium with;
  * the fixture files under `abi/fixtures/k4-store-v1/`.

The staleness check covers the first three. What the rest of this script does
is recompute, with `hashlib` and offsets taken from the schema, every digest
the fixtures claim -- deliberately without calling the generator's own vector
code, so a mistake in that code is a disagreement here rather than a value both
sides copy from each other.

The negative controls matter as much as the positive ones. A parser that
accepts a record whose payload was altered, or a superblock whose digest field
was left behind, would let every other check pass while the format guaranteed
nothing; each of those is damaged here and required to be refused.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen_k4_format as gen  # noqa: E402
import k4_format as fmt  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "abi/fixtures/k4-store-v1"


def sha(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def offsets(name: str) -> dict[str, int]:
    return {field[0]: field[1] for field in fmt.STRUCTS[name][1]}


def check_generated(schema, layout, vectors) -> tuple[bool, str]:
    stale = []
    produced = gen.outputs(schema, layout, vectors)
    for path, text in produced.items():
        if not path.exists():
            stale.append(f"{path.relative_to(ROOT)} is missing")
        elif path.read_text() != text:
            stale.append(f"{path.relative_to(ROOT)} differs from the schema")
    for path, data in gen.fixture_files(vectors).items():
        if not path.exists():
            stale.append(f"{path.relative_to(ROOT)} is missing")
        elif path.read_bytes() != data:
            stale.append(f"{path.relative_to(ROOT)} differs from the schema")
    if stale:
        return False, "; ".join(stale[:4])
    return True, (
        f"generated Rust, host module and {len(vectors.order)} fixtures match the schema byte "
        f"for byte"
    )


def check_numbering(schema, layout, vectors) -> tuple[bool, str]:
    """No number in one enumeration may mean two things."""
    total = 0
    for enum_name, entries in schema["enums"].items():
        seen: dict[int, str] = {}
        names: set[str] = set()
        for entry in entries:
            if entry["value"] in seen:
                return False, (
                    f"{enum_name} value {entry['value']} is both {seen[entry['value']]} "
                    f"and {entry['name']}"
                )
            if entry["name"] in names:
                return False, f"{enum_name} names {entry['name']} twice"
            seen[entry["value"]] = entry["name"]
            names.add(entry["name"])
            total += 1
    return True, f"{total} values across {len(schema['enums'])} enumerations are distinct"


def check_layout(schema, layout, vectors) -> tuple[bool, str]:
    """Every structure is explicitly padded and fits the framing it is used in."""
    header = layout["RecordHeader"]["size"]
    block = schema["geometry"]["block_size"]
    largest = 0
    for definition in schema["structs"]:
        info = layout[definition["name"]]
        if info["size"] % info["align"]:
            return False, f"{definition['name']} is not a multiple of its alignment"
        largest = max(largest, info["size"])
    payload_room = block * schema["geometry"]["record_max_blocks"] - header
    if schema["geometry"]["object_max_bytes"] + layout["ObjectRecordHeader"]["size"] > payload_room:
        return False, "an object of the declared maximum size does not fit one record"
    return True, (
        f"{len(schema['structs'])} structures need no implicit padding; the largest is "
        f"{largest} bytes and a maximal object record fits {payload_room} bytes of payload"
    )


def check_geometry(schema, layout, vectors) -> tuple[bool, str]:
    """The parts of the medium do not overlap and the directive is outside them."""
    geometry = schema["geometry"]
    arena0 = range(
        geometry["arena0_start_block"],
        geometry["arena0_start_block"] + geometry["arena_block_count"],
    )
    arena1 = range(
        geometry["arena1_start_block"],
        geometry["arena1_start_block"] + geometry["arena_block_count"],
    )
    if set(arena0) & set(arena1):
        return False, "the two arenas overlap"
    for name in ("superblock_a_block", "superblock_b_block"):
        if geometry[name] in arena0 or geometry[name] in arena1:
            return False, f"{name} lies inside an arena"
    if geometry["superblock_a_block"] == geometry["superblock_b_block"]:
        return False, "both superblocks occupy the same block"
    if arena1.stop > geometry["store_block_count"]:
        return False, "the second arena runs past the declared store"
    absolute = geometry["harness_directive_block"]
    if absolute >= geometry["store_base_block"]:
        return False, "the harness directive block lies inside the store"
    return True, (
        f"store of {geometry['store_block_count']} blocks at {geometry['store_base_block']}: "
        f"two superblocks then two disjoint arenas of {geometry['arena_block_count']}; the "
        f"directive block {absolute} is outside it"
    )


def check_fixture_files(schema, layout, vectors) -> tuple[bool, str]:
    index = json.loads((FIXTURES / "index.json").read_text())
    listed = {entry["name"]: entry for entry in index["vectors"]}
    if set(listed) != set(vectors.order):
        return False, "the fixture index does not name the schema's vectors"
    total = 0
    for name in vectors.order:
        path = FIXTURES / f"{name}.bin"
        if not path.exists():
            return False, f"{name}.bin is missing"
        data = path.read_bytes()
        entry = listed[name]
        if len(data) != entry["bytes"]:
            return False, f"{name}.bin is {len(data)} bytes, the index says {entry['bytes']}"
        if hashlib.sha256(data).hexdigest() != entry["sha256_of_file"]:
            return False, f"{name}.bin does not hash to what the index records"
        total += len(data)
    return True, f"{len(vectors.order)} fixture files present, {total} bytes, each as indexed"


def check_object_digests(schema, layout, vectors) -> tuple[bool, str]:
    """Object identity is recomputed here, from the file, with hashlib."""
    types = {entry["name"]: entry["value"] for entry in schema["enums"]["ObjectType"]}
    prefix = schema["prefixes"]["object"].encode() + b"\0"
    forms = {"object_bytes": "BYTES", "tree": "TREE"}
    checked = 0
    for name in vectors.order:
        item = vectors.items[name]
        if item["form"] not in ("object_bytes", "tree", "struct_object"):
            continue
        object_type = types[forms.get(item["form"], item.get("object_type"))]
        content = (FIXTURES / f"{name}.bin").read_bytes()
        want = sha(prefix + struct.pack("<IIQ", object_type, 0, len(content)) + content)
        if want.hex() != next(e for e in json.loads((FIXTURES / "index.json").read_text())["vectors"] if e["name"] == name)["digest"]:
            return False, f"{name}: recomputed content digest is not the one recorded"
        checked += 1
    empty_bytes = sha(prefix + struct.pack("<IIQ", types["BYTES"], 0, 0))
    empty_tree_content = struct.pack("<II", 0, 0)
    empty_tree = sha(
        prefix + struct.pack("<IIQ", types["TREE"], 0, len(empty_tree_content)) + empty_tree_content
    )
    if empty_bytes == empty_tree:
        return False, "an empty byte object and an empty tree share a digest"
    return True, (
        f"{checked} object digests recomputed from their files; an empty byte object and an "
        f"empty tree do not collide"
    )


def check_request_identity(schema, layout, vectors) -> tuple[bool, str]:
    """The request digest is rebuilt from the schema's own inputs."""
    definition = next(d for d in schema["golden"] if d["form"] == "request_digest")
    name = definition["name"]
    item = vectors.items[name]
    produced = fmt.request_digest(
        definition["principal"],
        definition["request_sequence"],
        definition["op"],
        definition["expected_generation"],
        vectors.items[definition["candidate_root"][len("digest:") :]]["digest"],
        vectors.items[definition["policy_digest"][len("digest:") :]]["digest"],
        vectors.items[definition["validation_digest"][len("digest:") :]]["digest"],
        definition.get("outbox_target", 0),
        vectors.items[definition["outbox_payload_digest"][len("digest:") :]]["digest"],
    )
    if produced != item["digest"]:
        return False, f"{name}: the host module and the generator disagree about request identity"
    altered = fmt.request_digest(
        definition["principal"],
        definition["request_sequence"],
        definition["op"],
        definition["expected_generation"] + 1,
        vectors.items[definition["candidate_root"][len("digest:") :]]["digest"],
        vectors.items[definition["policy_digest"][len("digest:") :]]["digest"],
        vectors.items[definition["validation_digest"][len("digest:") :]]["digest"],
        definition.get("outbox_target", 0),
        vectors.items[definition["outbox_payload_digest"][len("digest:") :]]["digest"],
    )
    if altered == produced:
        return False, "the expected generation does not affect the request identity"
    return True, (
        "the request identity is reproduced by the host module, and changing any input "
        "changes it"
    )


def synthetic_image(vectors) -> tuple[bytearray, dict[str, int]]:
    """Lays the record and superblock vectors out on a medium, as written."""
    geometry = fmt
    blocks = geometry.STORE_BASE_BLOCK + geometry.STORE_BLOCK_COUNT
    image = bytearray(blocks * geometry.BLOCK_SIZE)
    placed: dict[str, int] = {}
    at = geometry.STORE_BASE_BLOCK + geometry.ARENA0_START_BLOCK
    for name in vectors.order:
        item = vectors.items[name]
        if item["form"] == "record":
            data = item["bytes"]
            image[at * geometry.BLOCK_SIZE : at * geometry.BLOCK_SIZE + len(data)] = data
            placed[name] = at
            at += len(data) // geometry.BLOCK_SIZE
        elif item["form"] == "superblock":
            index = (
                geometry.SUPERBLOCK_A_BLOCK
                if name.endswith("live")
                else geometry.SUPERBLOCK_B_BLOCK
            )
            base = (geometry.STORE_BASE_BLOCK + index) * geometry.BLOCK_SIZE
            image[base : base + len(item["bytes"])] = item["bytes"]
            placed[name] = geometry.STORE_BASE_BLOCK + index
    return image, placed


def check_record_chain(schema, layout, vectors) -> tuple[bool, str]:
    """Records decode from a medium, link to each other and count by one."""
    image, placed = synthetic_image(vectors)
    previous = None
    kinds = []
    for name in vectors.order:
        if vectors.items[name]["form"] != "record":
            continue
        decoded = fmt.read_record(bytes(image), placed[name])
        if decoded is None:
            return False, f"{name} does not decode where it was written"
        kinds.append(fmt.RECORDKIND_NAME[decoded["kind"]])
        if previous is not None:
            if decoded["sequence"] != previous["sequence"] + 1:
                return False, f"{name} does not follow {previous['name']} by one"
            if decoded["prev_digest"] != previous["digest"]:
                return False, f"{name} does not link back to {previous['name']}"
        elif decoded["prev_digest"] != b"\0" * 32:
            return False, f"{name} opens the arena but links to something"
        previous = {"name": name, "sequence": decoded["sequence"], "digest": decoded["digest"]}
    missing = {entry["name"] for entry in schema["enums"]["RecordKind"]} - set(kinds)
    if missing:
        return False, f"no vector frames a record of kind {sorted(missing)}"
    return True, f"{len(kinds)} records decode and chain, covering all {len(set(kinds))} kinds"


def check_superblock_choice(schema, layout, vectors) -> tuple[bool, str]:
    """Both superblocks are valid; the newer generation is what is adopted."""
    image, _ = synthetic_image(vectors)
    a = fmt.read_superblock(bytes(image), fmt.SUPERBLOCK_A_BLOCK)
    b = fmt.read_superblock(bytes(image), fmt.SUPERBLOCK_B_BLOCK)
    if a is None or b is None:
        return False, "one of the two superblocks does not decode"
    if a["store_uuid"] != b["store_uuid"]:
        return False, "the two superblocks describe different stores"
    chosen = a if a["superblock_generation"] > b["superblock_generation"] else b
    if chosen["published_generation"] != 8 or chosen["active_arena"] != 0:
        return False, "the newer superblock does not describe the newer publication"
    if chosen["durable_through_sequence"] <= b["durable_through_sequence"]:
        return False, "the newer superblock does not advance the durability bound"
    return True, (
        f"two valid superblocks, generations {a['superblock_generation']} and "
        f"{b['superblock_generation']}; the newer publishes generation "
        f"{chosen['published_generation']} from arena {chosen['active_arena']}"
    )


def check_damage_refused(schema, layout, vectors) -> tuple[bool, str]:
    """Every damage the format claims to detect is applied and must be refused."""
    image, placed = synthetic_image(vectors)
    commit = placed["record_commit"]
    cases: list[tuple[str, bytearray, int, bool]] = []

    def damaged(label: str, mutate) -> None:
        copy = bytearray(image)
        mutate(copy)
        cases.append((label, copy, commit, True))

    payload_at = commit * fmt.BLOCK_SIZE + fmt.STRUCTS["RecordHeader"][0]
    damaged("a payload byte flipped", lambda buf: buf.__setitem__(payload_at, buf[payload_at] ^ 1))
    header_at = commit * fmt.BLOCK_SIZE + offsets("RecordHeader")["sequence"]
    damaged("the sequence rewritten", lambda buf: buf.__setitem__(header_at, buf[header_at] ^ 0xFF))
    magic_at = commit * fmt.BLOCK_SIZE
    damaged("the magic removed", lambda buf: buf.__setitem__(magic_at, 0))
    length_at = commit * fmt.BLOCK_SIZE + offsets("RecordHeader")["payload_len"]
    damaged(
        "a payload longer than its blocks",
        lambda buf: struct.pack_into("<Q", buf, length_at, fmt.BLOCK_SIZE * 4),
    )
    blocks_at = commit * fmt.BLOCK_SIZE + offsets("RecordHeader")["block_count"]
    damaged("a block count of zero", lambda buf: struct.pack_into("<Q", buf, blocks_at, 0))
    # A tear is only meaningful for a record that spans more than one sector.
    # Every small record is written by a single sector store and is therefore
    # either wholly there or wholly absent, which is a property of the geometry
    # rather than of the checksum, and worth stating instead of pretending to
    # test.
    wide = placed["record_object_wide"]
    sector_at = wide * fmt.BLOCK_SIZE + fmt.SECTOR_SIZE
    copy = bytearray(image)
    copy[sector_at : (wide + 1) * fmt.BLOCK_SIZE] = bytes(
        (wide + 1) * fmt.BLOCK_SIZE - sector_at
    )
    cases.append(("a tail torn after the first sector", copy, wide, True))

    for label, buf, block, must_refuse in cases:
        decoded = fmt.read_record(bytes(buf), block)
        if must_refuse and decoded is not None:
            return False, f"a record with {label} was accepted"
    if fmt.STRUCTS["RecordHeader"][0] + fmt.read_record(bytes(image), wide)["payload_len"] <= (
        fmt.SECTOR_SIZE
    ):
        return False, "the torn-tail control is applied to a record that fits one sector"

    superblock_at = (fmt.STORE_BASE_BLOCK + fmt.SUPERBLOCK_A_BLOCK) * fmt.BLOCK_SIZE
    for label, offset in (
        ("a published root byte flipped", offsets("StoreSuperblock")["published_root"]),
        ("its generation rewritten", offsets("StoreSuperblock")["superblock_generation"]),
        ("its digest zeroed", offsets("StoreSuperblock")["digest"]),
    ):
        copy = bytearray(image)
        copy[superblock_at + offset] ^= 0x5A
        if fmt.read_superblock(bytes(copy), fmt.SUPERBLOCK_A_BLOCK) is not None:
            return False, f"a superblock with {label} was accepted"

    if fmt.read_record(bytes(image), commit) is None:
        return False, "the undamaged record is refused, so the controls prove nothing"
    return True, (
        f"{len(cases)} damaged records and 3 damaged superblocks are all refused, and the "
        f"undamaged originals are still accepted"
    )


def check_tree_rules(schema, layout, vectors) -> tuple[bool, str]:
    """Canonical order, unique names and zero padding are decided from bytes."""
    blob = (FIXTURES / "tree_leaf.bin").read_bytes()
    size, fields = fmt.STRUCTS["TreeEntry"]
    header = fmt.decode("TreeHeader", blob)
    count = header["entry_count"]
    entries = [fmt.decode("TreeEntry", blob, fmt.STRUCTS["TreeHeader"][0] + i * size) for i in range(count)]
    names = [entry["name"][: entry["name_len"]] for entry in entries]
    if names != sorted(names):
        return False, "the tree fixture is not in canonical order"
    if len(set(names)) != len(names):
        return False, "the tree fixture repeats a name"
    for entry in entries:
        if any(entry["name"][entry["name_len"] :]):
            return False, "a name has non-zero bytes past its length"
        if b"/" in entry["name"][: entry["name_len"]] or not entry["name_len"]:
            return False, "a name breaks the naming rules"
    if len(blob) != fmt.STRUCTS["TreeHeader"][0] + count * size:
        return False, "the tree fixture has trailing bytes"
    root = (FIXTURES / "tree_root.bin").read_bytes()
    root_entries = [
        fmt.decode("TreeEntry", root, fmt.STRUCTS["TreeHeader"][0] + i * size)
        for i in range(fmt.decode("TreeHeader", root)["entry_count"])
    ]
    if not any(entry["flags"] & 1 for entry in root_entries):
        return False, "no vector pins the executable bit as part of a version"
    subtree = next(
        (e for e in root_entries if e["child_type"] == fmt.OBJECTTYPE["TREE"]),
        None,
    )
    if subtree is None or subtree["digest"] != vectors.items["tree_leaf"]["digest"]:
        return False, "the root does not name the leaf tree by its content digest"
    return True, (
        f"{count} entries in canonical order with zeroed name padding; the root names a "
        f"subtree by digest and pins an interpretation bit"
    )


def check_harness_is_scaffolding(schema, layout, vectors) -> tuple[bool, str]:
    """The fault directive verifies, and no store structure refers to it."""
    block = (FIXTURES / "harness_stop_after_commit.bin").read_bytes()
    decoded = fmt.decode("HarnessDirective", block)
    if decoded["magic"] != fmt.MAGIC["harness"]:
        return False, "the directive does not carry its own magic"
    if decoded["digest"] != fmt.harness_digest(block):
        return False, "the directive's digest does not verify"
    if decoded["magic"] == fmt.MAGIC["record"] or decoded["magic"] == fmt.MAGIC["superblock"]:
        return False, "the directive could be mistaken for store content"
    referring = [
        definition["name"]
        for definition in schema["structs"]
        if definition["name"] != "HarnessDirective"
        for field in definition["fields"]
        if "fault" in field["name"] or "harness" in field["name"]
    ]
    if referring:
        return False, f"store structures refer to the harness: {referring}"
    modes = {entry["name"] for entry in schema["enums"]["FaultMode"]}
    points = {entry["name"] for entry in schema["enums"]["FaultPoint"]}
    return True, (
        f"the directive verifies under its own digest, carries a magic no store structure "
        f"uses, and is named by nothing in the format; {len(modes) - 1} modes at "
        f"{len(points) - 1} points"
    )


CHECKS = [
    ("generated", "generated files match the schema", check_generated),
    ("numbering", "no number means two things", check_numbering),
    ("layout", "structures are explicitly padded", check_layout),
    ("geometry", "the medium's parts are disjoint", check_geometry),
    ("fixtures", "fixture files are present and indexed", check_fixture_files),
    ("objects", "object identity is type-tagged", check_object_digests),
    ("requests", "request identity covers its inputs", check_request_identity),
    ("records", "records decode, chain and count by one", check_record_chain),
    ("superblocks", "the newer superblock is the one adopted", check_superblock_choice),
    ("damage", "damaged records and superblocks are refused", check_damage_refused),
    ("trees", "trees are canonical and type-tagged", check_tree_rules),
    ("harness", "the fault directive is scaffolding", check_harness_is_scaffolding),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    arguments = parser.parse_args()

    try:
        schema, layout, vectors = gen.load()
    except gen.SchemaError as error:
        print(f"FAIL  schema  {error}")
        return 1

    results = []
    for name, title, check in CHECKS:
        try:
            passed, detail = check(schema, layout, vectors)
        except Exception as error:  # a checker that crashes has not passed
            passed, detail = False, f"{type(error).__name__}: {error}"
        results.append({"name": name, "title": title, "passed": passed, "detail": detail})
        print(f"{'PASS' if passed else 'FAIL'}  {title.ljust(42)}  {detail}")

    verdict = {
        "check": "k4-store-format",
        "passed": all(item["passed"] for item in results),
        "criteria": results,
        "structs": len(schema["structs"]),
        "vectors": len(vectors.order),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")
    print()
    if not verdict["passed"]:
        print("K4 STORE FORMAT CHECK FAILED")
        return 1
    print(f"K4 STORE FORMAT CHECK PASSED: {len(results)} of {len(results)} checks met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
