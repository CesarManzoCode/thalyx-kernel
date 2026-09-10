#!/usr/bin/env python3
"""Generate the K4 store format bindings and golden vectors from the schema.

`abi/schema/k4-store-v1.json` is the only place where a byte offset, a record
kind or a digest rule is decided. This script emits, from it and from nothing
else:

  user/k4fmt/src/generated.rs     Rust constants, `repr(C)` structures with
                                  compile-time layout assertions, and the
                                  golden vectors the guest checks its own
                                  encoder and its own SHA-256 against.
  tools/k4_format.py              The same constants and offsets for the host,
                                  plus decoders the gate reads a real medium
                                  with.
  abi/fixtures/k4-store-v1/       The golden bytes themselves, one file per
                                  vector, and an index naming their digests.

The layout engine is the ABI generator's: a structure whose fields do not land
on their natural alignment is a schema error rather than something padded
silently, and the same rule that keeps an uninitialised byte from crossing the
system-call boundary keeps one from reaching the medium.

Why golden vectors rather than a round-trip test. A service that encodes and
decodes with the same code agrees with itself whatever it does. These vectors
are bytes and digests this file computes with `hashlib`; the guest builds the
same objects with a hand-written no_std SHA-256 and compares. A disagreement is
then a real disagreement about the format, not two mistakes cancelling.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gen_abi import SCALARS, Layout, SchemaError, doc_lines, rustfmt  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "abi/schema/k4-store-v1.json"
FIXTURE_DIR = ROOT / "abi/fixtures/k4-store-v1"

# Offsets the digest rules refer to. They are consequences of the schema, not
# separate decisions: each is checked against the layout before anything is
# emitted, so a field added to a header moves the rule instead of breaking it.
RECORD_DIGEST_FIELD = "digest"
SUPERBLOCK_DIGEST_FIELD = "digest"
HARNESS_DIGEST_FIELD = "digest"


def digest(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def prefixed(prefix: str) -> bytes:
    """A digest domain prefix: the ASCII bytes and one zero byte."""
    return prefix.encode("ascii") + b"\0"


class Vectors:
    """Golden vectors, resolved in schema order so one may name an earlier one."""

    def __init__(self, schema: dict, layout: Layout):
        self.schema = schema
        self.layout = layout
        self.prefixes = schema["prefixes"]
        self.magics = schema["magics"]
        self.geometry = schema["geometry"]
        self.kinds = {e["name"]: e["value"] for e in schema["enums"]["RecordKind"]}
        self.object_types = {e["name"]: e["value"] for e in schema["enums"]["ObjectType"]}
        self.items: dict[str, dict] = {}
        self.order: list[str] = []
        for definition in schema["golden"]:
            self.add(definition)

    # -- resolution ---------------------------------------------------------

    def value(self, raw):
        """Resolves a schema field value, which may name another vector."""
        if isinstance(raw, str):
            if raw.startswith("digest:"):
                return self.items[raw[len("digest:") :]]["digest"]
            if raw.startswith("bytes:"):
                return self.items[raw[len("bytes:") :]]["bytes"]
            if raw.startswith("hex:"):
                return bytes.fromhex(raw[len("hex:") :])
            if raw.startswith("ascii:"):
                return raw[len("ascii:") :].encode("ascii")
            raise SchemaError(f"unknown vector reference {raw!r}")
        if isinstance(raw, dict) and "repeat" in raw:
            # A pattern repeated to a length. Spelling a kilobyte of content as
            # hex in the schema would make the schema unreadable to hide one
            # fact: that the bytes are arbitrary and only their length matters.
            pattern = self.value(raw["repeat"])
            length = raw["length"]
            if not pattern:
                raise SchemaError("a repeated pattern may not be empty")
            return (pattern * (length // len(pattern) + 1))[:length]
        return raw

    def encode_struct(self, name: str, fields: dict) -> bytes:
        definition = self.layout[name]
        blob = bytearray(definition["size"])
        known = {field["name"] for field in definition["fields"]}
        unknown = set(fields) - known
        if unknown:
            raise SchemaError(f"{name} has no field(s) {sorted(unknown)}")
        for field in definition["fields"]:
            if field["name"] not in fields:
                # Unmentioned fields stay zero. A byte array left out is zeroed
                # rather than refused, which is what a reserved digest field or
                # an absent link actually is on the medium.
                continue
            value = self.value(fields[field["name"]])
            base, count = field["base"], field["count"]
            size, _, _, code = SCALARS[base]
            if isinstance(value, bytes):
                if count is None or base != "u8":
                    raise SchemaError(f"{name}.{field['name']} is not a byte array")
                if len(value) > count:
                    raise SchemaError(
                        f"{name}.{field['name']} takes {count} bytes, given {len(value)}"
                    )
                blob[field["offset"] : field["offset"] + len(value)] = value
                continue
            if count is not None:
                raise SchemaError(f"{name}.{field['name']} needs bytes, given {value!r}")
            struct.pack_into("<" + code, blob, field["offset"], value)
        return bytes(blob)

    def object_digest(self, object_type: str, content: bytes) -> bytes:
        return digest(
            prefixed(self.prefixes["object"])
            + struct.pack("<IIQ", self.object_types[object_type], 0, len(content))
            + content
        )

    # -- vector forms -------------------------------------------------------

    def add(self, definition: dict) -> None:
        name = definition["name"]
        form = definition["form"]
        handler = getattr(self, f"form_{form}", None)
        if handler is None:
            raise SchemaError(f"{name}: unknown golden form {form}")
        item = handler(definition)
        item["name"] = name
        item["form"] = form
        item.setdefault("doc", definition.get("doc"))
        self.items[name] = item
        self.order.append(name)

    def form_object_bytes(self, definition: dict) -> dict:
        content = self.value(definition["content"])
        if len(content) > self.geometry["object_max_bytes"]:
            raise SchemaError(f"{definition['name']}: content exceeds the object bound")
        return {
            "object_type": "BYTES",
            "bytes": content,
            "digest": self.object_digest("BYTES", content),
        }

    def form_tree(self, definition: dict) -> dict:
        entries = definition["entries"]
        if len(entries) > self.geometry["max_tree_entries"]:
            raise SchemaError(f"{definition['name']}: too many entries")
        names = [entry["name"].encode("ascii") for entry in entries]
        for encoded in names:
            if not encoded or len(encoded) > 32:
                raise SchemaError(f"{definition['name']}: name length out of range")
            if b"\0" in encoded or b"/" in encoded or encoded in (b".", b".."):
                raise SchemaError(f"{definition['name']}: {encoded!r} is not a legal name")
        if names != sorted(names):
            raise SchemaError(f"{definition['name']}: entries are not in canonical order")
        if len(set(names)) != len(names):
            raise SchemaError(f"{definition['name']}: duplicate name")
        blob = bytearray(self.encode_struct("TreeHeader", {"entry_count": len(entries)}))
        for entry, encoded in zip(entries, names):
            child = self.items[entry["child"]]
            blob += self.encode_struct(
                "TreeEntry",
                {
                    "child_type": self.object_types[child["object_type"]],
                    "flags": entry.get("flags", 0),
                    "length": len(child["bytes"]),
                    "digest": child["digest"],
                    "name_len": len(encoded),
                    "name": encoded,
                },
            )
        content = bytes(blob)
        return {
            "object_type": "TREE",
            "bytes": content,
            "digest": self.object_digest("TREE", content),
        }

    def form_struct_object(self, definition: dict) -> dict:
        content = self.encode_struct(definition["struct"], definition["fields"])
        return {
            "object_type": definition["object_type"],
            "bytes": content,
            "digest": self.object_digest(definition["object_type"], content),
        }

    def form_record(self, definition: dict) -> dict:
        payload = bytearray(self.encode_struct(definition["struct"], definition["fields"]))
        # A record payload is a head structure and, for the kinds that have one,
        # a tail: the canonical bytes of an object, or the variable-length
        # entries a checkpoint carries. The tail is part of the payload the
        # digest covers, so a vector that stopped at the head would pin a
        # framing the store never writes.
        for entry in definition.get("entries", []):
            payload += self.encode_struct(entry["struct"], entry["fields"])
        body_name = definition.get("body")
        if body_name is not None:
            body = self.items[body_name]["bytes"]
            declared = definition["fields"].get("length")
            if isinstance(declared, int) and declared != len(body):
                raise SchemaError(
                    f"{definition['name']}: declared length {declared} is not the "
                    f"{len(body)} bytes of {body_name}"
                )
            payload += body
        payload = bytes(payload)
        header = self.layout["RecordHeader"]
        cut = next(f for f in header["fields"] if f["name"] == RECORD_DIGEST_FIELD)["offset"]
        block = self.geometry["block_size"]
        blocks = (header["size"] + len(payload) + block - 1) // block
        if blocks > self.geometry["record_max_blocks"]:
            raise SchemaError(
                f"{definition['name']}: {blocks} blocks exceeds record_max_blocks"
            )
        prev = definition.get("prev")
        prev_digest = self.items[prev]["digest"] if prev else b"\0" * 32
        head = bytearray(
            self.encode_struct(
                "RecordHeader",
                {
                    "magic": self.magics["record"].encode("ascii"),
                    "format_major": self.schema["version"]["major"],
                    "format_minor": self.schema["version"]["minor"],
                    "kind": self.kinds[definition["kind"]],
                    "sequence": definition["sequence"],
                    "store_epoch": definition["store_epoch"],
                    "payload_len": len(payload),
                    "block_count": blocks,
                    "arena": definition.get("arena", 0),
                    "prev_digest": prev_digest,
                },
            )
        )
        record_digest = digest(bytes(head[:cut]) + payload)
        head[cut : cut + 32] = record_digest
        framed = bytearray(blocks * block)
        framed[: header["size"]] = head
        framed[header["size"] : header["size"] + len(payload)] = payload
        return {"bytes": bytes(framed), "digest": record_digest, "payload": payload}

    def form_superblock(self, definition: dict) -> dict:
        layout = self.layout["StoreSuperblock"]
        cut = next(f for f in layout["fields"] if f["name"] == SUPERBLOCK_DIGEST_FIELD)["offset"]
        fields = dict(definition["fields"])
        fields.setdefault("magic", "ascii:" + self.magics["superblock"])
        fields.setdefault("format_major", self.schema["version"]["major"])
        fields.setdefault("format_minor", self.schema["version"]["minor"])
        fields.setdefault("block_size", self.geometry["block_size"])
        body = bytearray(self.encode_struct("StoreSuperblock", fields))
        value = digest(bytes(body[:cut]))
        body[cut : cut + 32] = value
        block = bytearray(self.geometry["block_size"])
        block[: len(body)] = body
        return {"bytes": bytes(block), "digest": value}

    def form_harness(self, definition: dict) -> dict:
        layout = self.layout["HarnessDirective"]
        cut = next(f for f in layout["fields"] if f["name"] == HARNESS_DIGEST_FIELD)["offset"]
        fields = dict(definition["fields"])
        fields.setdefault("magic", "ascii:" + self.magics["harness"])
        fields.setdefault("version", self.schema["version"]["major"])
        body = bytearray(self.encode_struct("HarnessDirective", fields))
        value = digest(bytes(body[:cut]))
        body[cut : cut + 32] = value
        block = bytearray(self.geometry["block_size"])
        block[: len(body)] = body
        return {"bytes": bytes(block), "digest": value}

    def form_store_uuid(self, definition: dict) -> dict:
        seed = definition["seed"]
        epoch = definition["store_epoch"]
        value = digest(prefixed(self.prefixes["store_uuid"]) + struct.pack("<QQ", seed, epoch))[:16]
        return {"bytes": value, "digest": value + b"\0" * 16}

    def form_request_digest(self, definition: dict) -> dict:
        body = (
            prefixed(self.prefixes["request"])
            + struct.pack(
                "<QQIIQ",
                definition["principal"],
                definition["request_sequence"],
                definition["op"],
                0,
                definition["expected_generation"],
            )
            + self.value(definition["candidate_root"])
            + self.value(definition["policy_digest"])
            + self.value(definition["validation_digest"])
            + struct.pack("<Q", definition.get("outbox_target", 0))
            + self.value(definition.get("outbox_payload_digest", "hex:" + "00" * 32))
        )
        return {"bytes": body, "digest": digest(body)}


def rust_type(field: dict) -> str:
    base, count = field["base"], field["count"]
    inner = SCALARS[base][1] if base in SCALARS else base[len("struct:") :]
    return f"[{inner}; {count}]" if count is not None else inner


def byte_array(data: bytes) -> str:
    return "[" + ", ".join(f"0x{value:02X}" for value in data) + "]"


def generate_rust(schema: dict, layout: Layout, vectors: Vectors) -> str:
    out: list[str] = []
    out.append("//! K4 store format, generated from `abi/schema/k4-store-v1.json`.")
    out.append("//!")
    out.append("//! Every offset, kind, bound and golden vector below comes from the schema.")
    out.append("//! `tools/check_k4_format.py` regenerates this file and fails if it differs,")
    out.append("//! so an edit here is a difference the checker reports rather than a quiet")
    out.append("//! fork of the format the medium already holds.")
    out.append("")
    out.append("// Generated by tools/gen_k4_format.py. Do not edit.")
    out.append("#![allow(clippy::unreadable_literal)]")
    out.append("")
    version = schema["version"]
    out.append("/// Major format version stamped into every record and superblock.")
    out.append(f"pub const FORMAT_MAJOR: u16 = {version['major']};")
    out.append("/// Minor format version.")
    out.append(f"pub const FORMAT_MINOR: u16 = {version['minor']};")
    out.append("")
    out.append("/// Where the store lives on the medium and how large its parts are.")
    out.append("pub mod geometry {")
    for key, value in schema["geometry"].items():
        out.append(f"    /// `{key}` from the schema.")
        out.append(f"    pub const {key.upper()}: u64 = {value};")
    out.append("}")
    out.append("")
    out.append("/// Digest domain prefixes. Each is these bytes followed by one zero byte, so")
    out.append("/// two kinds of content never share an unlabelled digest space.")
    out.append("pub mod prefix {")
    for key, value in schema["prefixes"].items():
        out.append(f"    /// Prefix for {key} digests.")
        out.append(f'    pub const {key.upper()}: &[u8] = b"{value}\\0";')
    out.append("}")
    out.append("")
    out.append("/// Magic strings. A block that does not start with one is not a record.")
    out.append("pub mod magic {")
    for key, value in schema["magics"].items():
        out.append(f"    /// Magic of a {key}.")
        out.append(f'    pub const {key.upper()}: [u8; 8] = *b"{value}";')
    out.append("}")
    out.append("")
    for enum_name, entries in schema["enums"].items():
        module = "".join(("_" + c.lower()) if c.isupper() else c for c in enum_name).lstrip("_")
        out.append(f"/// `{enum_name}` from the schema.")
        out.append(f"pub mod {module} {{")
        for entry in entries:
            out.extend(doc_lines(entry.get("doc"), "    "))
            if not entry.get("doc"):
                out.append(f"    /// `{entry['name']}`.")
            out.append(f"    pub const {entry['name']}: u32 = {entry['value']};")
        out.append("}")
        out.append("")
    for definition in schema["structs"]:
        info = layout[definition["name"]]
        out.extend(doc_lines(definition.get("doc"), ""))
        out.append("#[repr(C)]")
        out.append("#[derive(Clone, Copy, Debug, PartialEq, Eq)]")
        out.append(f"pub struct {info['name']} {{")
        for field in info["fields"]:
            doc = field.get("doc") or (
                "Reserved, must be zero."
                if field["name"].startswith("reserved")
                else f"Schema field `{field['name']}`, little-endian `{field['type']}`."
            )
            out.append(f"    /// {doc}")
            out.append(f"    pub {field['name']}: {rust_type(field)},")
        out.append("}")
        out.append("")
        out.append(f"impl {info['name']} {{")
        out.append("    /// Encoded size in bytes.")
        out.append(f"    pub const SIZE: usize = {info['size']};")
        for field in info["fields"]:
            out.append(f"    /// Offset of `{field['name']}`.")
            out.append(
                f"    pub const OFFSET_{field['name'].upper()}: usize = {field['offset']};"
            )
        out.append("}")
        out.append("")
        out.append(
            f"const _: () = assert!(core::mem::size_of::<{info['name']}>() == {info['size']});"
        )
        out.append(
            f"const _: () = assert!(core::mem::align_of::<{info['name']}>() == {info['align']});"
        )
        for field in info["fields"]:
            out.append(
                f"const _: () = assert!(core::mem::offset_of!({info['name']}, {field['name']}) "
                f"== {field['offset']});"
            )
        out.append("")
    out.append("/// Bytes and digests this format is pinned to.")
    out.append("///")
    out.append("/// The guest builds each of these with its own encoder and its own SHA-256")
    out.append("/// and compares. Agreement between two implementations that never shared a")
    out.append("/// line of code is the only reason to believe either of them.")
    out.append("pub mod golden {")
    for name in vectors.order:
        item = vectors.items[name]
        upper = name.upper()
        if item.get("doc"):
            out.append(f"    /// {item['doc']}")
        else:
            out.append(f"    /// Golden vector `{name}` ({item['form']}).")
        out.append(
            f"    pub const {upper}: [u8; {len(item['bytes'])}] = {byte_array(item['bytes'])};"
        )
        out.append(f"    /// Digest of `{name}`.")
        out.append(f"    pub const {upper}_DIGEST: [u8; 32] = {byte_array(item['digest'])};")
    out.append("    /// How many vectors this format is pinned by.")
    out.append(f"    pub const COUNT: usize = {len(vectors.order)};")
    out.append("}")
    out.append("")
    return "\n".join(out) + "\n"


def generate_python(schema: dict, layout: Layout, vectors: Vectors) -> str:
    out: list[str] = []
    out.append('"""K4 store format for the host, generated from the schema.')
    out.append("")
    out.append("Generated by tools/gen_k4_format.py. Do not edit.")
    out.append("")
    out.append("The gate decodes a real medium with this module. It shares offsets with the")
    out.append("guest because both come from `abi/schema/k4-store-v1.json`, and shares no")
    out.append("code at all with the guest's encoder or its SHA-256, which is the half that")
    out.append("makes a disagreement mean something.")
    out.append('"""')
    out.append("")
    out.append("from __future__ import annotations")
    out.append("")
    out.append("import hashlib")
    out.append("import struct")
    out.append("")
    out.append(f"FORMAT_MAJOR = {schema['version']['major']}")
    out.append(f"FORMAT_MINOR = {schema['version']['minor']}")
    out.append("")
    for key, value in schema["geometry"].items():
        out.append(f"{key.upper()} = {value}")
    out.append("")
    out.append("PREFIX = {")
    for key, value in schema["prefixes"].items():
        out.append(f'    "{key}": b"{value}\\x00",')
    out.append("}")
    out.append("")
    out.append("MAGIC = {")
    for key, value in schema["magics"].items():
        out.append(f'    "{key}": b"{value}",')
    out.append("}")
    out.append("")
    for enum_name, entries in schema["enums"].items():
        out.append(f"{enum_name.upper()} = {{")
        for entry in entries:
            out.append(f'    "{entry["name"]}": {entry["value"]},')
        out.append("}")
        out.append(f"{enum_name.upper()}_NAME = {{v: k for k, v in {enum_name.upper()}.items()}}")
        out.append("")
    out.append("# name -> (size, [(field, offset, struct code, count)])")
    out.append("STRUCTS = {")
    for definition in schema["structs"]:
        info = layout[definition["name"]]
        out.append(f'    "{info["name"]}": ({info["size"]}, [')
        for field in info["fields"]:
            code = SCALARS[field["base"]][3]
            count = field["count"]
            out.append(
                f'        ("{field["name"]}", {field["offset"]}, "{code}", '
                f"{count if count is not None else 'None'}),"
            )
        out.append("    ]),")
    out.append("}")
    out.append("")
    out.append("GOLDEN = {")
    for name in vectors.order:
        item = vectors.items[name]
        out.append(f'    "{name}": {{')
        out.append(f'        "form": "{item["form"]}",')
        out.append(f'        "bytes": bytes.fromhex("{item["bytes"].hex()}"),')
        out.append(f'        "digest": bytes.fromhex("{item["digest"].hex()}"),')
        out.append("    },")
    out.append("}")
    out.append("")
    out.append('''
def encode(name: str, fields: dict) -> bytes:
    """Encodes one schema structure. Every field not given is zero.

    The gate writes exactly one structure with this, the fault directive, and
    that block is outside the store. Nothing that belongs to a store is ever
    written by the host: what a medium holds is what the guest put there.
    """
    size, layout = STRUCTS[name]
    blob = bytearray(size)
    known = {field[0] for field in layout}
    unknown = set(fields) - known
    if unknown:
        raise ValueError(f"{name} has no field {sorted(unknown)}")
    for field, offset, code, count in layout:
        value = fields.get(field)
        if value is None:
            continue
        if count is None:
            struct.pack_into("<" + code, blob, offset, value)
        elif code == "B":
            if len(value) > count:
                raise ValueError(f"{name}.{field} takes {count} bytes, got {len(value)}")
            blob[offset : offset + len(value)] = value
        else:
            struct.pack_into(f"<{count}{code}", blob, offset, *value)
    return bytes(blob)


def decode(name: str, blob: bytes, offset: int = 0) -> dict:
    """Decodes one schema structure out of `blob` at `offset`."""
    size, fields = STRUCTS[name]
    if len(blob) < offset + size:
        raise ValueError(f"{name} needs {size} bytes at {offset}, {len(blob) - offset} available")
    out: dict = {}
    for field, field_offset, code, count in fields:
        base = offset + field_offset
        if count is None:
            (value,) = struct.unpack_from("<" + code, blob, base)
            out[field] = value
        elif code == "B":
            out[field] = blob[base : base + count]
        else:
            out[field] = list(struct.unpack_from(f"<{count}{code}", blob, base))
    return out


def object_digest(object_type: int, content: bytes) -> bytes:
    """The canonical content digest: domain prefix, type, length, bytes."""
    return hashlib.sha256(
        PREFIX["object"] + struct.pack("<IIQ", object_type, 0, len(content)) + content
    ).digest()


def request_digest(
    principal: int,
    sequence: int,
    op: int,
    expected_generation: int,
    candidate_root: bytes,
    policy_digest: bytes,
    validation_digest: bytes,
    outbox_target: int = 0,
    outbox_payload_digest: bytes = b"\\x00" * 32,
) -> bytes:
    """The identity of a request: every input that decides what it means."""
    return hashlib.sha256(
        PREFIX["request"]
        + struct.pack("<QQIIQ", principal, sequence, op, 0, expected_generation)
        + candidate_root
        + policy_digest
        + validation_digest
        + struct.pack("<Q", outbox_target)
        + outbox_payload_digest
    ).digest()


def record_digest(header: bytes, payload: bytes) -> bytes:
    """A record's digest covers its header up to the digest field, then payload."""
    cut = dict((f[0], f[1]) for f in STRUCTS["RecordHeader"][1])["digest"]
    return hashlib.sha256(header[:cut] + payload).digest()


def superblock_digest(block: bytes) -> bytes:
    cut = dict((f[0], f[1]) for f in STRUCTS["StoreSuperblock"][1])["digest"]
    return hashlib.sha256(block[:cut]).digest()


def harness_digest(block: bytes) -> bytes:
    cut = dict((f[0], f[1]) for f in STRUCTS["HarnessDirective"][1])["digest"]
    return hashlib.sha256(block[:cut]).digest()


def read_superblock(image: bytes, block_index: int) -> dict | None:
    """Decodes a superblock, or None when that block does not hold a valid one."""
    base = (STORE_BASE_BLOCK + block_index) * BLOCK_SIZE
    if len(image) < base + BLOCK_SIZE:
        return None
    block = image[base : base + BLOCK_SIZE]
    if block[:8] != MAGIC["superblock"]:
        return None
    decoded = decode("StoreSuperblock", block)
    if decoded["digest"] != superblock_digest(block):
        return None
    if decoded["format_major"] != FORMAT_MAJOR:
        return None
    decoded["block_index"] = block_index
    return decoded


def read_record(image: bytes, absolute_block: int) -> dict | None:
    """Decodes and verifies one framed record at an absolute device block."""
    base = absolute_block * BLOCK_SIZE
    if len(image) < base + BLOCK_SIZE:
        return None
    head = image[base : base + BLOCK_SIZE]
    if head[:8] != MAGIC["record"]:
        return None
    decoded = decode("RecordHeader", head)
    if decoded["format_major"] != FORMAT_MAJOR:
        return None
    header_size = STRUCTS["RecordHeader"][0]
    blocks = decoded["block_count"]
    if blocks < 1 or blocks > RECORD_MAX_BLOCKS:
        return None
    span = image[base : base + blocks * BLOCK_SIZE]
    if len(span) < blocks * BLOCK_SIZE:
        return None
    payload_len = decoded["payload_len"]
    if header_size + payload_len > len(span):
        return None
    payload = span[header_size : header_size + payload_len]
    if decoded["digest"] != record_digest(span[:header_size], payload):
        return None
    decoded["payload"] = payload
    decoded["block"] = absolute_block
    return decoded
''')
    return "\n".join(out) + "\n"


def check_digest_rules(layout: Layout) -> None:
    """The digest rules name fields; this refuses a schema where they moved away."""
    for name, field in (
        ("RecordHeader", RECORD_DIGEST_FIELD),
        ("StoreSuperblock", SUPERBLOCK_DIGEST_FIELD),
        ("HarnessDirective", HARNESS_DIGEST_FIELD),
    ):
        info = layout[name]
        last = info["fields"][-1]
        if last["name"] != field:
            raise SchemaError(
                f"{name}.{field} must be the last field: a digest that does not cover "
                f"everything before it and nothing after it is not a checksum of the record"
            )
        if last["size"] != 32:
            raise SchemaError(f"{name}.{field} must be 32 bytes")


def outputs(schema: dict, layout: Layout, vectors: Vectors) -> dict[Path, str]:
    return {
        ROOT / "user/k4fmt/src/generated.rs": rustfmt(generate_rust(schema, layout, vectors)),
        ROOT / "tools/k4_format.py": generate_python(schema, layout, vectors),
    }


def fixture_files(vectors: Vectors) -> dict[Path, bytes]:
    files: dict[Path, bytes] = {}
    index = {"schema": "thalyx-k4-store/v1", "vectors": []}
    for name in vectors.order:
        item = vectors.items[name]
        files[FIXTURE_DIR / f"{name}.bin"] = item["bytes"]
        index["vectors"].append(
            {
                "name": name,
                "form": item["form"],
                "bytes": len(item["bytes"]),
                "sha256_of_file": hashlib.sha256(item["bytes"]).hexdigest(),
                "digest": item["digest"].hex(),
            }
        )
    files[FIXTURE_DIR / "index.json"] = (json.dumps(index, indent=2) + "\n").encode()
    return files


def load() -> tuple[dict, Layout, Vectors]:
    schema = json.loads(SCHEMA.read_text())
    layout = Layout(schema)
    check_digest_rules(layout)
    return schema, layout, Vectors(schema, layout)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if a file would change")
    arguments = parser.parse_args()

    try:
        schema, layout, vectors = load()
        produced = outputs(schema, layout, vectors)
        binaries = fixture_files(vectors)
    except SchemaError as error:
        print(f"schema error: {error}", file=sys.stderr)
        return 2

    stale = []
    for path, text in produced.items():
        current = path.read_text() if path.exists() else None
        if current == text:
            continue
        if arguments.check:
            stale.append(path.relative_to(ROOT))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
            print(f"wrote {path.relative_to(ROOT)}", file=sys.stderr)
    for path, data in binaries.items():
        current = path.read_bytes() if path.exists() else None
        if current == data:
            continue
        if arguments.check:
            stale.append(path.relative_to(ROOT))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            print(f"wrote {path.relative_to(ROOT)}", file=sys.stderr)

    if stale:
        for path in stale:
            print(f"stale generated file: {path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
