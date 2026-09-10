#!/usr/bin/env python3
"""Generate the Thalyx-Kernel ABI bindings and fixtures from the single schema.

`abi/schema/v0.json` is the only place where a number, a field or a layout is
decided. This script emits, from it and from nothing else:

  abi/src/generated.rs   Rust constants and `repr(C)` structures, with compile
                         time assertions on every size, alignment and offset.
  abi/src/fixture.rs     Encoded sample descriptors and the values they must
                         decode to, so the Rust side is checked against bytes
                         this script produced rather than against itself.
  abi/include/thalyx_abi.h  The same interface for C, with `_Static_assert` on
                         the same layout facts.

Padding is never implicit: a structure whose fields do not land on their natural
alignment is a schema error, not something the generator silently fixes. That is
what makes "explicit padding, reserved fields zero" checkable instead of
aspirational.
"""

from __future__ import annotations

import argparse
import json
import shutil
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "abi/schema/v0.json"

SCALARS = {
    "u8": (1, "u8", "uint8_t", "B"),
    "u16": (2, "u16", "uint16_t", "H"),
    "u32": (4, "u32", "uint32_t", "I"),
    "u64": (8, "u64", "uint64_t", "Q"),
    "i64": (8, "i64", "int64_t", "q"),
}


class SchemaError(RuntimeError):
    pass


def parse_type(text: str):
    """Returns (base, count) where count is None for a scalar."""
    base, count = text, None
    if text.endswith("]"):
        base, _, rest = text.partition("[")
        count = int(rest[:-1])
    return base, count


class Layout:
    """Sizes, alignments and field offsets of every schema structure."""

    def __init__(self, schema: dict):
        self.structs: dict[str, dict] = {}
        for definition in schema["structs"]:
            self.add(definition)

    def add(self, definition: dict) -> None:
        name = definition["name"]
        offset = 0
        align = 1
        fields = []
        for field in definition["fields"]:
            base, count = parse_type(field["type"])
            if base in SCALARS:
                size, _, _, _ = SCALARS[base]
                field_align = size
            elif base.startswith("struct:"):
                target = self.structs.get(base[len("struct:") :])
                if target is None:
                    raise SchemaError(f"{name}.{field['name']}: unknown struct {base}")
                size = target["size"]
                field_align = target["align"]
            else:
                raise SchemaError(f"{name}.{field['name']}: unknown type {field['type']}")
            total = size * (count or 1)
            if offset % field_align:
                raise SchemaError(
                    f"{name}.{field['name']} would need {field_align - offset % field_align} "
                    f"bytes of implicit padding at offset {offset}; add an explicit reserved field"
                )
            fields.append(
                {
                    "name": field["name"],
                    "type": field["type"],
                    "base": base,
                    "count": count,
                    "offset": offset,
                    "size": total,
                    "align": field_align,
                    "doc": field.get("doc"),
                }
            )
            offset += total
            align = max(align, field_align)
        if offset % align:
            raise SchemaError(
                f"{name} has size {offset} which is not a multiple of its alignment {align}; "
                f"add an explicit trailing reserved field"
            )
        self.structs[name] = {
            "name": name,
            "doc": definition.get("doc"),
            "fields": fields,
            "size": offset,
            "align": align,
        }

    def __getitem__(self, name: str) -> dict:
        return self.structs[name]


def opcode(type_value: int, ordinal: int) -> int:
    return (type_value << 16) | ordinal


def rights_value(schema: dict, type_name: str, names: list[str]) -> int:
    bits = {entry["name"]: entry["bit"] for entry in schema["rights"]["common"]}
    for entries in schema["rights"]["by_type"].values():
        for entry in entries:
            bits[entry["name"]] = entry["bit"]
    value = 0
    for name in names:
        if name not in bits:
            raise SchemaError(f"operation on {type_name} requires unknown right {name}")
        value |= 1 << bits[name]
    return value


def sample_scalar(base: str, index: int) -> int:
    """Deterministic, type-appropriate sample value for a fixture field."""
    size, _, _, _ = SCALARS[base]
    if base == "i64":
        return -(index * 7 + 3)
    return (0x0102030405060708 * (index + 1) + index) % (1 << (8 * size))


def encode(layout: Layout, name: str, seed: int) -> tuple[bytes, list[dict]]:
    """Encodes one sample instance of `name`, returning bytes and expectations."""
    definition = layout[name]
    blob = bytearray(definition["size"])
    expectations = []
    counter = seed
    for field in definition["fields"]:
        base, count = field["base"], field["count"]
        if base.startswith("struct:"):
            inner = base[len("struct:") :]
            for element in range(count or 1):
                nested, nested_expect = encode(layout, inner, counter + element * 3 + 1)
                start = field["offset"] + element * layout[inner]["size"]
                blob[start : start + len(nested)] = nested
                for item in nested_expect:
                    expectations.append(
                        {
                            "path": f"{field['name']}"
                            + (f"[{element}]" if count is not None else "")
                            + f".{item['path']}",
                            "base": item["base"],
                            "value": item["value"],
                        }
                    )
            counter += 5
            continue
        size, _, _, code = SCALARS[base]
        for element in range(count or 1):
            value = sample_scalar(base, counter)
            counter += 1
            struct.pack_into(
                "<" + code, blob, field["offset"] + element * size, value
            )
            expectations.append(
                {
                    "path": f"{field['name']}" + (f"[{element}]" if count is not None else ""),
                    "base": base,
                    "value": value,
                }
            )
    return bytes(blob), expectations


def rust_type(field: dict) -> str:
    base, count = field["base"], field["count"]
    inner = SCALARS[base][1] if base in SCALARS else base[len("struct:") :]
    return f"[{inner}; {count}]" if count is not None else inner


def c_type(field: dict) -> str:
    base = field["base"]
    return SCALARS[base][2] if base in SCALARS else "thalyx_" + snake(base[len("struct:") :]) + "_t"


def snake(name: str) -> str:
    out = []
    for index, char in enumerate(name):
        if char.isupper() and index and not name[index - 1].isupper():
            out.append("_")
        out.append(char.lower())
    return "".join(out)


def doc_lines(text: str | None, indent: str) -> list[str]:
    return [f"{indent}/// {text}"] if text else []


def field_doc(field: dict) -> str:
    """Documentation for a field, defaulting to a statement of the schema fact.

    Every field carries a doc comment because the crate warns on a missing one,
    and because a field with nothing to say about it should say that it is a
    reserved byte rather than stay silent.
    """
    if field.get("doc"):
        return field["doc"]
    if field["name"].startswith("reserved"):
        return "Reserved, must be zero."
    return f"Schema field `{field['name']}`, little-endian `{field['type']}`."


def generate_rust(schema: dict, layout: Layout) -> str:
    version = schema["version"]
    limits = schema["limits"]
    out: list[str] = []
    out.append("//! Assigned V0 interface, generated from `abi/schema/v0.json`.")
    out.append("//!")
    out.append("//! Every constant, structure and layout assertion below comes from the schema.")
    out.append("//! `tools/check_abi.py` regenerates this file and fails if it differs, so an")
    out.append("//! edit here is a difference the checker reports rather than a silent fork.")
    out.append("")
    out.append("// Generated by tools/gen_abi.py. Do not edit.")
    out.append("#![allow(clippy::unreadable_literal)]")
    out.append("")
    out.append("/// Major interface version.")
    out.append(f"pub const VERSION_MAJOR: u16 = {version['major']};")
    out.append("/// Minor interface version.")
    out.append(f"pub const VERSION_MINOR: u16 = {version['minor']};")
    out.append("")
    out.append("/// Interface limits fixed by V0. Reported by the limits query entry.")
    out.append("pub mod limit {")
    for key, value in limits.items():
        out.append(f"    /// `{key}` from the schema.")
        out.append(f"    pub const {key.upper()}: u64 = {value};")
    out.append("}")
    out.append("")
    out.append("/// Kernel entry identifiers carried in RAX.")
    out.append("pub mod entry {")
    for item in schema["entries"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: u64 = {item['value']};")
    out.append("}")
    out.append("")
    out.append("/// Object type codes. The high 16 bits of every operation code.")
    out.append("pub mod object_type {")
    for item in schema["object_types"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: u32 = {item['value']};")
    out.append("}")
    out.append("")
    out.append("/// Rights bits. Bits 0..7 are common to every type; bits 8 and above are")
    out.append("/// type specific, so one bit never means two things for one object.")
    out.append("pub mod right {")
    for item in schema["rights"]["common"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: u32 = 1 << {item['bit']};")
    for type_name, entries in schema["rights"]["by_type"].items():
        out.append(f"    // {type_name}")
        for item in entries:
            out.extend(doc_lines(item.get("doc"), "    "))
            out.append(f"    pub const {item['name']}: u32 = 1 << {item['bit']};")
    out.append("}")
    out.append("")
    out.append("/// Operation codes: `(object type << 16) | ordinal`.")
    out.append("pub mod op {")
    types = {item["name"]: item["value"] for item in schema["object_types"]}
    for item in schema["operations"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        code = opcode(types[item["type"]], item["ordinal"])
        out.append(f"    pub const {item['name']}: u32 = 0x{code:08X};")
    out.append("}")
    out.append("")
    out.append("/// Flags accepted in the flags register. Unknown bits are refused.")
    out.append("pub mod flag {")
    for item in schema["flags"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: u64 = 1 << {item['bit']};")
    known = 0
    for item in schema["flags"]:
        known |= 1 << item["bit"]
    out.append("    /// Every bit this revision defines.")
    out.append(f"    pub const KNOWN: u64 = 0x{known:X};")
    out.append("}")
    out.append("")
    out.append("/// Rights every operation requires, checked before any effect.")
    out.append("pub mod op_rights {")
    for item in schema["operations"]:
        value = rights_value(schema, item["type"], item["rights"])
        out.append(f"    /// Rights required by `op::{item['name']}`.")
        out.append(f"    pub const {item['name']}: u32 = 0x{value:08X};")
    out.append("}")
    out.append("")
    out.append("/// Descriptor sizes every operation expects, header included.")
    out.append("pub mod op_sizes {")
    for item in schema["operations"]:
        request = layout[item["request"]]["size"] + 32 if item["request"] else 0
        response = layout[item["response"]]["size"] + 32 if item["response"] else 0
        size = max(request, response)
        out.append(
            f"    /// Descriptor bytes for `op::{item['name']}`; zero when it takes none."
        )
        out.append(f"    pub const {item['name']}: u32 = {size};")
    out.append("}")
    out.append("")
    out.append("/// What one operation requires, looked up before anything is validated.")
    out.append("///")
    out.append("/// The kernel reads this table instead of repeating the schema in a match, so")
    out.append("/// an operation cannot exist with rights or a descriptor size that differ")
    out.append("/// from the ones the interface publishes.")
    out.append("#[derive(Clone, Copy, Debug)]")
    out.append("pub struct OpSpec {")
    out.append("    /// Operation code.")
    out.append("    pub code: u32,")
    out.append("    /// Object type the handle must have.")
    out.append("    pub object_type: u32,")
    out.append("    /// Rights the grant must carry.")
    out.append("    pub rights: u32,")
    out.append("    /// Descriptor length in bytes, header included; zero for none.")
    out.append("    pub descriptor_len: u32,")
    out.append("    /// Whether the operation writes a response body.")
    out.append("    pub writes_response: bool,")
    out.append("    /// Name, for diagnostic records.")
    out.append("    pub name: &\'static str,")
    out.append("}")
    out.append("")
    specs = []
    for item in schema["operations"]:
        code = opcode(types[item["type"]], item["ordinal"])
        request = layout[item["request"]]["size"] + 32 if item["request"] else 0
        response = layout[item["response"]]["size"] + 32 if item["response"] else 0
        specs.append(
            "    OpSpec { code: 0x%08X, object_type: %d, rights: 0x%08X, descriptor_len: %d, "
            "writes_response: %s, name: \"%s\" },"
            % (
                code,
                types[item["type"]],
                rights_value(schema, item["type"], item["rights"]),
                max(request, response),
                "true" if response else "false",
                item["name"],
            )
        )
    out.append("/// Every operation this revision assigns, ordered by code.")
    out.append(f"pub const OPERATIONS: [OpSpec; {len(specs)}] = [")
    out.extend(specs)
    out.append("];")
    out.append("")
    out.append("/// Looks up an operation code, or `None` when the number is not assigned.")
    out.append("///")
    out.append("/// An unassigned number is refused rather than ignored, so a program built")
    out.append("/// against a later table fails visibly instead of silently succeeding.")
    out.append("#[must_use]")
    out.append("pub fn spec(code: u32) -> Option<&\'static OpSpec> {")
    out.append("    let mut index = 0;")
    out.append("    while index < OPERATIONS.len() {")
    out.append("        if OPERATIONS[index].code == code {")
    out.append("            return Some(&OPERATIONS[index]);")
    out.append("        }")
    out.append("        index += 1;")
    out.append("    }")
    out.append("    None")
    out.append("}")
    out.append("")
    out.append("/// Status values returned in RAX. Zero is success, every error is negative.")
    out.append("pub mod status {")
    for item in schema["errors"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: i64 = {item['value']};")
    out.append("}")
    out.append("")
    for enum_name, entries in schema["enums"].items():
        out.append(f"/// `{enum_name}` values crossing the boundary as integers.")
        out.append(f"pub mod {snake(enum_name)} {{")
        for item in entries:
            out.append(f"    /// `{enum_name}::{item['name']}`.")
            out.append(f"    pub const {item['name']}: u32 = {item['value']};")
        out.append("}")
        out.append("")
    out.append("/// Slots the kernel installs the first supervisor's boot capabilities in.")
    out.append("pub mod boot_slot {")
    for item in schema["boot_slots"]:
        out.extend(doc_lines(item.get("doc"), "    "))
        out.append(f"    pub const {item['name']}: u32 = {item['slot']};")
    out.append("}")
    out.append("")
    for definition in layout.structs.values():
        out.extend(doc_lines(definition.get("doc"), ""))
        out.append("#[repr(C)]")
        out.append("#[derive(Clone, Copy, Debug, PartialEq, Eq)]")
        out.append(f"pub struct {definition['name']} {{")
        for field in definition["fields"]:
            out.extend(doc_lines(field_doc(field), "    "))
            out.append(f"    pub {field['name']}: {rust_type(field)},")
        out.append("}")
        out.append("")
        out.append(
            f"const _: () = assert!(core::mem::size_of::<{definition['name']}>() == {definition['size']});"
        )
        out.append(
            f"const _: () = assert!(core::mem::align_of::<{definition['name']}>() == {definition['align']});"
        )
        for field in definition["fields"]:
            out.append(
                f"const _: () = assert!(core::mem::offset_of!({definition['name']}, "
                f"{field['name']}) == {field['offset']});"
            )
        out.append("")
        out.append(f"impl {definition['name']} {{")
        out.append("    /// Size in bytes, as fixed by the schema.")
        out.append(f"    pub const SIZE: usize = {definition['size']};")
        out.append("    /// Size of a descriptor carrying this body, header included.")
        out.append(f"    pub const DESCRIPTOR_LEN: u32 = {definition['size'] + 32};")
        out.append("    /// A zeroed value.")
        out.append("    #[must_use]")
        out.append("    pub const fn zeroed() -> Self {")
        out.append("        // SAFETY: every field is an integer or an array of integers, so the")
        out.append("        // all-zero bit pattern is a valid value of this type.")
        out.append("        unsafe { core::mem::zeroed() }")
        out.append("    }")
        out.append("}")
        out.append("")
    return "\n".join(out) + "\n"


def generate_fixture(schema: dict, layout: Layout, names: list[str]) -> str:
    out: list[str] = []
    out.append("//! Layout fixtures, generated from `abi/schema/v0.json`.")
    out.append("//!")
    out.append("//! The byte vectors below were produced by the generator, not by this crate's")
    out.append("//! own encoder. Decoding them with the generated structures therefore checks")
    out.append("//! the Rust layout against an independent implementation of the same schema,")
    out.append("//! which is what a fixture is for. [`verify`] runs inside the kernel at boot")
    out.append("//! so the check happens on the real path, not only in a host test.")
    out.append("")
    out.append("// Generated by tools/gen_abi.py. Do not edit.")
    out.append("")
    out.append("use crate::generated::*;")
    out.append("")
    out.append("/// One fixture: the encoded bytes and the value they must decode to.")
    out.append("pub struct Fixture {")
    out.append("    /// Structure name, as written in the schema.")
    out.append("    pub name: &'static str,")
    out.append("    /// Bytes the generator produced.")
    out.append("    pub bytes: &'static [u8],")
    out.append("    /// Size the schema fixes for the structure.")
    out.append("    pub size: usize,")
    out.append("}")
    out.append("")
    checks = []
    for index, name in enumerate(names):
        blob, expectations = encode(layout, name, index * 17 + 1)
        literal = ", ".join(f"0x{byte:02X}" for byte in blob)
        out.append(f"const {snake(name).upper()}_BYTES: [u8; {len(blob)}] = [{literal}];")
        checks.append((name, expectations))
    out.append("")
    out.append("/// Every fixture the schema defines.")
    out.append(f"pub const FIXTURES: [Fixture; {len(names)}] = [")
    for name in names:
        out.append(
            f"    Fixture {{ name: \"{name}\", bytes: &{snake(name).upper()}_BYTES, "
            f"size: {layout[name]['size']} }},"
        )
    out.append("];")
    out.append("")
    out.append("/// Decodes every fixture and compares each field with the generator's value.")
    out.append("///")
    out.append("/// Returns `(checked, failed)`. A failure means this build's layout differs")
    out.append("/// from the schema the bytes were produced from.")
    out.append("#[must_use]")
    out.append("pub fn verify() -> (u32, u32) {")
    out.append("    let mut checked = 0u32;")
    out.append("    let mut failed = 0u32;")
    for name, expectations in checks:
        out.append(f"    // {name}")
        out.append("    {")
        out.append(f"        let bytes = &{snake(name).upper()}_BYTES;")
        out.append(
            f"        // SAFETY: the array is {layout[name]['size']} bytes, the exact size of"
        )
        out.append("        // the structure, every field of which is an integer, so any bit")
        out.append("        // pattern is a valid value. The read is unaligned by construction.")
        out.append(
            f"        let value: {name} = unsafe {{ core::ptr::read_unaligned(bytes.as_ptr().cast::<{name}>()) }};"
        )
        for item in expectations:
            path = item["path"]
            expected = item["value"]
            suffix = SCALARS[item["base"]][1]
            out.append("        checked += 1;")
            out.append(
                f"        if value.{path} != {expected}{suffix} {{ failed += 1; }}"
            )
        out.append("    }")
    out.append("    (checked, failed)")
    out.append("}")
    return "\n".join(out) + "\n"


def generate_c(schema: dict, layout: Layout) -> str:
    version = schema["version"]
    out: list[str] = []
    out.append("/* Generated by tools/gen_abi.py from abi/schema/v0.json. Do not edit. */")
    out.append("/*")
    out.append(" * C binding of the Thalyx-Kernel V0 interface. It exists so the boundary is")
    out.append(" * expressed in a language with no Rust representation rules: if a structure")
    out.append(" * only laid out correctly under `repr(C)` by accident, the assertions below")
    out.append(" * would disagree with the Rust ones.")
    out.append(" */")
    out.append("#ifndef THALYX_ABI_H")
    out.append("#define THALYX_ABI_H")
    out.append("")
    out.append("#include <stdint.h>")
    out.append("#include <stddef.h>")
    out.append("")
    out.append(f"#define THALYX_ABI_VERSION_MAJOR {version['major']}u")
    out.append(f"#define THALYX_ABI_VERSION_MINOR {version['minor']}u")
    for key, value in schema["limits"].items():
        out.append(f"#define THALYX_{key.upper()} {value}ull")
    out.append("")
    for item in schema["entries"]:
        out.append(f"#define THALYX_ENTRY_{item['name']} {item['value']}ull")
    out.append("")
    for item in schema["flags"]:
        out.append(f"#define THALYX_FLAG_{item['name']} (1ull << {item['bit']})")
    out.append("")
    for item in schema["object_types"]:
        out.append(f"#define THALYX_TYPE_{item['name']} {item['value']}u")
    out.append("")
    for item in schema["rights"]["common"]:
        out.append(f"#define THALYX_RIGHT_{item['name']} (1u << {item['bit']})")
    for entries in schema["rights"]["by_type"].values():
        for item in entries:
            out.append(f"#define THALYX_RIGHT_{item['name']} (1u << {item['bit']})")
    out.append("")
    types = {item["name"]: item["value"] for item in schema["object_types"]}
    for item in schema["operations"]:
        out.append(
            f"#define THALYX_OP_{item['name']} 0x{opcode(types[item['type']], item['ordinal']):08X}u"
        )
    out.append("")
    for item in schema["errors"]:
        out.append(f"#define THALYX_STATUS_{item['name']} ({item['value']})")
    out.append("")
    for enum_name, entries in schema["enums"].items():
        for item in entries:
            out.append(
                f"#define THALYX_{snake(enum_name).upper()}_{item['name']} {item['value']}u"
            )
    out.append("")
    for item in schema["boot_slots"]:
        out.append(f"#define THALYX_BOOT_SLOT_{item['name']} {item['slot']}u")
    out.append("")
    out.append("/* One assigned operation, as the schema publishes it. The Rust side has the")
    out.append(" * same table; a native C program needs it for the same reason the kernel")
    out.append(" * does -- the descriptor header carries a length the kernel checks against")
    out.append(" * R10, and a program that guessed that length would be refused. */")
    out.append("typedef struct {")
    out.append("    uint32_t code;")
    out.append("    uint32_t object_type;")
    out.append("    uint32_t rights;")
    out.append("    uint32_t descriptor_len; /* Header included; zero when the operation carries none. */")
    out.append("    uint32_t writes_response;")
    out.append("    const char *name;")
    out.append("} thalyx_op_spec_t;")
    out.append("")
    out.append(f"#define THALYX_OPERATION_COUNT {len(schema['operations'])}u")
    out.append("static const thalyx_op_spec_t thalyx_operations[THALYX_OPERATION_COUNT] = {")
    for item in schema["operations"]:
        code = opcode(types[item["type"]], item["ordinal"])
        request = layout[item["request"]]["size"] + 32 if item["request"] else 0
        response = layout[item["response"]]["size"] + 32 if item["response"] else 0
        out.append(
            "    { 0x%08Xu, %du, 0x%08Xu, %du, %du, \"%s\" },"
            % (
                code,
                types[item["type"]],
                rights_value(schema, item["type"], item["rights"]),
                max(request, response),
                1 if response else 0,
                item["name"],
            )
        )
    out.append("};")
    out.append("")
    out.append("/* Looks up an operation code, or NULL when the number is not assigned. */")
    out.append("static inline const thalyx_op_spec_t *thalyx_op_spec(uint32_t code) {")
    out.append("    for (unsigned i = 0; i < THALYX_OPERATION_COUNT; i++) {")
    out.append("        if (thalyx_operations[i].code == code) {")
    out.append("            return &thalyx_operations[i];")
    out.append("        }")
    out.append("    }")
    out.append("    return NULL;")
    out.append("}")
    out.append("")
    for definition in layout.structs.values():
        typedef = "thalyx_" + snake(definition["name"]) + "_t"
        if definition.get("doc"):
            out.append(f"/* {definition['doc']} */")
        out.append("typedef struct {")
        for field in definition["fields"]:
            base, count = field["base"], field["count"]
            declaration = c_type(field)
            suffix = f"[{count}]" if count is not None else ""
            comment = f" /* {field_doc(field)} */"
            out.append(f"    {declaration} {field['name']}{suffix};{comment}")
        out.append(f"}} {typedef};")
        out.append(
            f"_Static_assert(sizeof({typedef}) == {definition['size']}, "
            f'"{definition["name"]} size");'
        )
        out.append(
            f"_Static_assert(_Alignof({typedef}) == {definition['align']}, "
            f'"{definition["name"]} alignment");'
        )
        for field in definition["fields"]:
            out.append(
                f"_Static_assert(offsetof({typedef}, {field['name']}) == {field['offset']}, "
                f'"{definition["name"]}.{field["name"]} offset");'
            )
        out.append("")
    out.append("#endif /* THALYX_ABI_H */")
    return "\n".join(out) + "\n"


FIXTURE_STRUCTS = [
    "DescriptorHeader",
    "Limits",
    "CapInfo",
    "DeriveRequest",
    "ScopeInfo",
    "DrainReport",
    "MapRequest",
    "MemoryInfo",
    "SendRequest",
    "MessageHeader",
    "ReceiveResult",
    "InvocationInfo",
    "ReceiptRecord",
    "LogReadResult",
    "FaultReport",
]


def rustfmt(text: str) -> str:
    """Formats generated Rust with the project's own formatter.

    The generated files are part of the tree that `cargo fmt --check` inspects,
    so they have to come out of the generator already formatted; otherwise every
    regeneration would leave the format check failing on a file nobody edits.

    A missing formatter is refused rather than skipped. Returning the
    unformatted text instead makes every comparison against a committed file
    fail, and fail saying the bindings disagree with the schema -- which is a
    refusal arriving with the wrong reason, and sends the reader to look at a
    schema that is fine.
    """
    binary = shutil.which("rustfmt")
    if binary is None:
        raise SchemaError(
            "rustfmt was not found on PATH, so the generated Rust cannot be produced in the "
            "form the tree stores it. Install it or put the toolchain's bin directory on PATH"
        )
    result = subprocess.run(
        [binary, "--edition", "2024", "--emit", "stdout", "--quiet"],
        input=text,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SchemaError(f"rustfmt refused the generated file: {result.stderr.strip()}")
    return result.stdout


def outputs(schema: dict, layout: Layout) -> dict[Path, str]:
    return {
        ROOT / "abi/src/generated.rs": rustfmt(generate_rust(schema, layout)),
        ROOT / "abi/src/fixture.rs": rustfmt(generate_fixture(schema, layout, FIXTURE_STRUCTS)),
        ROOT / "abi/include/thalyx_abi.h": generate_c(schema, layout),
    }


def load() -> tuple[dict, Layout]:
    schema = json.loads(SCHEMA.read_text())
    return schema, Layout(schema)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if a file would change")
    arguments = parser.parse_args()

    try:
        schema, layout = load()
    except SchemaError as error:
        print(f"schema error: {error}", file=sys.stderr)
        return 2

    try:
        produced = outputs(schema, layout)
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
    if stale:
        for path in stale:
            print(f"stale generated file: {path}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
