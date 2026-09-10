#!/usr/bin/env python3
"""Evaluate the K4 case matrix against the gate.

K4 is the first phase whose claims are about something that outlives the run
that made them. That changes what evidence is worth: a program's account of
what it wrote is not evidence that anything was written, and a service's
account of what it recovered is not evidence that a medium said so. So the
criteria below are decided from two sources the guest does not get to narrate
-- the kernel's own records, and the bytes on the medium, decoded here by the
module the schema generates rather than by anything the guest links.

Where a criterion has to read a program's notes it says so in its title: a
status only the caller received, an answer only the client saw. Those are the
minority and they are named.

Each criterion is evaluated separately, so it fails on its own rather than
being carried by the others, and each carries the lines and blocks it was
decided from.

The K1, K2 and K3 regressions are criteria here too. K4 grew inside the kernel
that boots K1, serves K2 and drives K3's devices, and a K4 gate that passed
while any of them had quietly broken would be measuring the wrong thing.

Usage: tools/check_k4.py [--cases build/k4-cases] [--self-test]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import k4_format as fmt  # noqa: E402

# Mirrors `thalyx_kernel::diag::FORMAT`.
FORMAT = "THLX1"

RECORD = re.compile(
    r"^" + FORMAT + r" (?P<source>loader|kernel) (?P<seq>\d+) (?P<ns>\d+|-) (?P<event>\S+)(?P<rest>.*)$"
)

# `run_k4.py` maps the kernel's completion status to QEMU's `(value << 1) | 1`.
EXIT_COMPLETE = 33

# Mirrors `thalyx_user_rt::k2::report`. Repeated rather than imported so a
# renumbering would fail this gate instead of redefining what it checks.
REPORT = {
    "refused_as_expected": 0x2007,
    "not_refused": 0x2008,
    "done": 0x200B,
    "unexpected": 0x200C,
}

# Mirrors `thalyx_user_k4fmt::pkg::note`, for the same reason.
NOTE = {
    "disk_ready": 0x4001,
    "disk_write": 0x4002,
    "disk_flush": 0x4003,
    "disk_suppressed": 0x4004,
    "disk_latched": 0x4005,
    "disk_io_error": 0x4006,
    "disk_reordered": 0x4007,
    "disk_read": 0x4008,
    "disk_unreachable": 0x4009,
    "store_formatted": 0x4010,
    "store_recovered": 0x4011,
    "recovery_scanned": 0x4012,
    "recovery_aborted": 0x4013,
    "recovery_dependency_missing": 0x4014,
    "recovery_integrity_failed": 0x4015,
    "published": 0x4016,
    "publish_refused": 0x4017,
    "record_written": 0x4018,
    "checkpoint_written": 0x4019,
    "superblock_switched": 0x401A,
    "compacted": 0x401B,
    "result_answered": 0x401C,
    "golden_verified": 0x401D,
    "effect_admitted": 0x401E,
    "effect_refused": 0x401F,
    "outbox_recorded": 0x4020,
    "fault_directive": 0x4021,
    "fault_applied": 0x4022,
    "exhausted": 0x4023,
    "store_ready": 0x4024,
    "root_head": 0x4025,
    "high_water": 0x4026,
    "integrity_refused": 0x4027,
    "control_incomplete": 0x4028,
    "conflict_refused": 0x4029,
    "service_stopped": 0x402A,
    "workspaces_exhausted": 0x402B,
    "staging_reclaimed": 0x402C,
    "staging_exhausted": 0x402D,
    "client_role": 0x4030,
    "client_published": 0x4031,
    "client_refused": 0x4032,
    "client_result": 0x4033,
    "client_read": 0x4034,
    "client_aba": 0x4035,
    "broker_answered": 0x4036,
    "validation_refused": 0x4037,
    "publish_forbidden": 0x4038,
    "client_resumed": 0x4039,
    "service_gone": 0x403A,
    "super_built": 0x4040,
    "super_crash": 0x4041,
    "super_restarted": 0x4042,
    "super_finished": 0x4043,
    "audit_drained": 0x4044,
    "audit_effect": 0x4045,
    "audit_gap": 0x4046,
    "audit_lost": 0x4047,
    "audit_high_water": 0x4048,
}

STORE = "k4store"
DISK = "k4disk"
SUPERVISOR = "supervisor"

# Operations K4's claims rest on. Not the whole interface: this package is not
# the K2 package and does not pretend to walk what it never touches. What it
# must not do is claim a durable path it never took, so everything a
# publication passes through is named here.
K4_REQUIRED_OPERATIONS = {
    "ENDPOINT_CALL",
    "ENDPOINT_RECEIVE",
    "ENDPOINT_BIND_FACET",
    "INVOCATION_REPLY",
    "INVOCATION_BEGIN_EFFECT",
    "LOG_READ",
    "LOG_ACK",
    "LOG_APPEND",
    "LOG_QUERY",
    "DEVICE_QUERY",
    "DEVICE_MAP_REGION",
    "DEVICE_BIND_IRQ",
    "DEVICE_SET_MASTER",
    "DEVICE_DMA_MAP",
    "MEMORY_QUERY",
    "DOMAIN_MAP",
    "DOMAIN_INSTALL_CAP",
    "DOMAIN_ACTIVATE",
    "SIGNAL_RAISE",
    "SIGNAL_WAIT",
    "CAP_DERIVE",
    "CAP_CLOSE",
    "CAP_INSPECT",
}


@dataclass
class Record:
    source: str
    seq: int
    ns: int | None
    event: str
    fields: dict[str, str]

    def get(self, key: str) -> str | None:
        return self.fields.get(key)

    def number(self, key: str) -> int | None:
        raw = self.fields.get(key)
        if raw is None:
            return None
        try:
            return int(raw, 16) if raw.startswith("0x") else int(raw)
        except ValueError:
            return None

    def signed(self, key: str) -> int | None:
        value = self.number(key)
        if value is None:
            return None
        return value - (1 << 64) if value >= (1 << 63) else value


@dataclass
class Leg:
    """One boot: what the kernel recorded, and the medium it left behind."""

    case: str
    index: int
    records: list[Record]
    exit_status: int | None
    timed_out: bool
    medium: bytes
    directive: dict


@dataclass
class Case:
    name: str
    spec: dict
    legs: list[Leg]


@dataclass
class Result:
    name: str
    title: str
    passed: bool = False
    detail: str = ""
    evidence: list[str] = field(default_factory=list)


def parse(text: str) -> list[Record]:
    records = []
    for line in text.splitlines():
        start = line.find(FORMAT + " ")
        if start < 0:
            continue
        match = RECORD.match(line[start:])
        if match is None:
            continue
        fields = {}
        for token in match.group("rest").split():
            key, separator, value = token.partition("=")
            if separator:
                fields[key] = value
        records.append(
            Record(
                source=match.group("source"),
                seq=int(match.group("seq")),
                ns=None if match.group("ns") == "-" else int(match.group("ns")),
                event=match.group("event"),
                fields=fields,
            )
        )
    return records


def by_event(records: list[Record], event: str) -> list[Record]:
    return [record for record in records if record.event == event]


def notes(records: list[Record], code: int, domain: str | None = None) -> list[Record]:
    out = []
    for record in by_event(records, "user.note"):
        if record.get("kind") != "self_check" or record.number("a") != code:
            continue
        if domain is not None and record.get("name") != domain:
            continue
        out.append(record)
    return out


def note_values(records: list[Record], key: str, domain: str | None = None) -> list[int]:
    return [
        value
        for value in (record.number("b") for record in notes(records, NOTE[key], domain))
        if value is not None
    ]


def report_values(records: list[Record], key: str, domain: str | None = None) -> list[int]:
    return [
        value
        for value in (record.number("b") for record in notes(records, REPORT[key], domain))
        if value is not None
    ]


def line_of(record: Record) -> str:
    rendered = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{record.seq} {record.event} {rendered}".rstrip()


# --- the medium ------------------------------------------------------------
#
# Everything below decodes a medium with the module the schema generates. The
# guest never wrote any of this code and the gate never links any of the
# guest's, so a format the two disagree about is a disagreement the golden
# vectors already decide -- not something either side gets to settle here.


@dataclass
class Store:
    """A medium, as far as it decodes."""

    superblocks: list[dict]
    chosen: dict | None
    records: list[dict]
    """Records of the chosen arena's valid prefix, oldest first."""
    broken_at: int | None
    """Block where the prefix stopped, or None if the arena was walked out."""


def arena_bounds(superblock: dict) -> tuple[int, int]:
    if superblock["active_arena"] == 0:
        return superblock["arena0_start_block"], superblock["arena0_block_count"]
    return superblock["arena1_start_block"], superblock["arena1_block_count"]


def read_store(image: bytes) -> Store:
    """Decodes the medium: both superblocks, and the prefix the newer names."""
    found = []
    for index in (fmt.SUPERBLOCK_A_BLOCK, fmt.SUPERBLOCK_B_BLOCK):
        decoded = fmt.read_superblock(image, index)
        if decoded is not None:
            found.append(decoded)
    chosen = max(found, key=lambda s: s["superblock_generation"], default=None)
    if chosen is None:
        return Store(found, None, [], None)

    start, count = arena_bounds(chosen)
    block = fmt.STORE_BASE_BLOCK + start
    end = block + count
    walked: list[dict] = []
    previous = bytes(32)
    expected = None
    broken = None
    while block < end:
        record = fmt.read_record(image, block)
        if record is None:
            broken = block
            break
        if record["prev_digest"] != previous:
            broken = block
            break
        if expected is not None and record["sequence"] != expected:
            broken = block
            break
        if record["store_epoch"] != chosen["store_epoch"]:
            broken = block
            break
        walked.append(record)
        previous = record["digest"]
        expected = record["sequence"] + 1
        block += record["block_count"]
    return Store(found, chosen, walked, broken)


def payload_of(record: dict, name: str) -> dict | None:
    """Decodes a record's body as the structure its kind implies."""
    try:
        return fmt.decode(name, record["payload"], 0)
    except Exception:  # noqa: BLE001 - a malformed body is a failed criterion
        return None


def records_of_kind(store: Store, kind: str) -> list[dict]:
    return [r for r in store.records if r["kind"] == fmt.RECORDKIND[kind]]


def commits(store: Store) -> list[dict]:
    out = []
    for record in records_of_kind(store, "COMMIT"):
        body = payload_of(record, "CommitRecord")
        if body is not None:
            body["sequence"] = record["sequence"]
            out.append(body)
    return out


def prepares(store: Store) -> list[dict]:
    out = []
    for record in records_of_kind(store, "PREPARE"):
        body = payload_of(record, "PrepareRecord")
        if body is not None:
            body["sequence"] = record["sequence"]
            out.append(body)
    return out


def aborts(store: Store) -> list[dict]:
    out = []
    for record in records_of_kind(store, "ABORT"):
        body = payload_of(record, "AbortRecord")
        if body is not None:
            body["sequence"] = record["sequence"]
            out.append(body)
    return out


def outboxes(store: Store) -> list[dict]:
    out = []
    for record in records_of_kind(store, "OUTBOX"):
        body = payload_of(record, "OutboxRecord")
        if body is not None:
            body["sequence"] = record["sequence"]
            out.append(body)
    return out


def checkpoints(store: Store) -> list[dict]:
    out = []
    for record in records_of_kind(store, "CHECKPOINT"):
        body = payload_of(record, "CheckpointRecord")
        if body is not None:
            body["sequence"] = record["sequence"]
            out.append(body)
    return out


def objects(store: Store) -> dict[bytes, dict]:
    """Every object record in the prefix, by content digest."""
    out = {}
    for record in records_of_kind(store, "OBJECT"):
        head = payload_of(record, "ObjectRecordHeader")
        if head is None:
            continue
        size = fmt.STRUCTS["ObjectRecordHeader"][0]
        content = record["payload"][size : size + head["length"]]
        if fmt.object_digest(head["object_type"], content) != head["content_digest"]:
            continue
        out[head["content_digest"]] = {
            "object_type": head["object_type"],
            "content": content,
            "sequence": record["sequence"],
        }
    return out


def reachable_from(store: Store, root: bytes) -> set[bytes] | None:
    """Walks a manifest to its tree, policy, validation and contents.

    Returns None when a digest the walk needs is not on the medium, because
    "the version is there" and "everything it names is there" are different
    claims and only the second one makes it readable.
    """
    held = objects(store)
    seen: set[bytes] = set()
    queue = [root]
    while queue:
        digest = queue.pop()
        if digest in seen:
            continue
        entry = held.get(digest)
        if entry is None:
            return None
        seen.add(digest)
        if entry["object_type"] == fmt.OBJECTTYPE["MANIFEST"]:
            body = fmt.decode("Manifest", entry["content"], 0)
            queue += [body["tree_digest"], body["policy_digest"], body["validation_digest"]]
        elif entry["object_type"] == fmt.OBJECTTYPE["TREE"]:
            head = fmt.decode("TreeHeader", entry["content"], 0)
            size = fmt.STRUCTS["TreeHeader"][0]
            entry_size = fmt.STRUCTS["TreeEntry"][0]
            for index in range(head["entry_count"]):
                at = size + index * entry_size
                child = fmt.decode("TreeEntry", entry["content"], at)
                queue.append(child["digest"])
    return seen


# --- criteria ---------------------------------------------------------------
#
# Each function receives the case matrix, fills in a Result, and never raises:
# a missing record is a failed criterion, not a crash.


def case_named(cases: list[Case], name: str) -> Case | None:
    for case in cases:
        if case.name == name:
            return case
    return None


def all_legs(cases: list[Case]) -> list[Leg]:
    return [leg for case in cases for leg in case.legs]


def check_legs_completed(cases: list[Case]) -> Result:
    result = Result("legs", "cada tramo terminó por decisión del kernel")
    legs = all_legs(cases)
    if not legs:
        result.detail = "no legs were run"
        return result
    bad = []
    for leg in legs:
        terminal = by_event(leg.records, "k1.terminal")
        if leg.timed_out:
            bad.append(f"{leg.case}/{leg.index}: timed out")
        elif leg.exit_status != EXIT_COMPLETE:
            bad.append(f"{leg.case}/{leg.index}: exit {leg.exit_status}")
        elif len(terminal) != 1 or terminal[0].get("status") != "complete":
            bad.append(f"{leg.case}/{leg.index}: no single terminal record")
    if bad:
        result.detail = "; ".join(bad[:4])
        return result
    result.passed = True
    result.detail = f"{len(legs)} legs across {len(cases)} cases, every one ended at k1.terminal"
    result.evidence = [f"{leg.case}/{leg.index} {line_of(by_event(leg.records, 'k1.terminal')[0])}" for leg in legs[:3]]
    return result


def check_no_user_faults(cases: list[Case]) -> Result:
    result = Result("faults", "ningún dominio falló, en ningún tramo")
    bad = []
    for leg in all_legs(cases):
        summary = by_event(leg.records, "k1.summary")
        if len(summary) != 1:
            bad.append(f"{leg.case}/{leg.index}: no summary")
            continue
        faults = summary[0].number("user_faults")
        if faults is None or faults != 0:
            bad.append(f"{leg.case}/{leg.index}: user_faults={faults}")
        for record in by_event(leg.records, "domain.terminated"):
            # `terminated` is the supervisor stopping a domain on purpose, which
            # is a K4 case rather than a failure. Anything else is a fault.
            if record.get("reason") not in ("voluntary", "terminated", None):
                bad.append(f"{leg.case}/{leg.index}: {record.get('name')} {record.get('reason')}")
    if bad:
        result.detail = "; ".join(bad[:4])
        return result
    result.passed = True
    result.detail = (
        "user_faults=0 in every leg, and every domain that ended did so voluntarily or because "
        "its supervisor stopped it"
    )
    return result


def is_cut(case: Case) -> bool:
    """Whether any leg of this case was handed a directive that names a point."""
    return any(directive_of(leg)[0] != "NONE" for leg in case.legs)


def check_nothing_unexpected(cases: list[Case]) -> Result:
    result = Result("unexpected", "ningún programa registró un resultado no esperado")
    bad = []
    gone = 0
    for case in cases:
        for leg in case.legs:
            for key in ("unexpected", "not_refused"):
                for record in notes(leg.records, REPORT[key]):
                    bad.append(
                        f"{leg.case}/{leg.index} {record.get('name')} {key}=0x{record.number('b'):x}"
                    )
            # A client whose service disappeared is not reporting a surprise: it
            # is reporting the cut. What would be a surprise is finding one in a
            # case that asked for no cut at all.
            for record in notes(leg.records, NOTE["service_gone"]):
                if not is_cut(case):
                    bad.append(
                        f"{leg.case}/{leg.index} {record.get('name')} lost the service with no cut"
                    )
                else:
                    gone += 1
    if bad:
        result.detail = f"{len(bad)}: " + "; ".join(bad[:4])
        return result
    result.passed = True
    result.detail = (
        f"no `unexpected` and no `not_refused` note in any leg of any case; the {gone} times a "
        f"client lost its service were all in cases that asked to be cut"
    )
    return result


def check_format_agreement(cases: list[Case]) -> Result:
    result = Result("format", "el invitado reprodujo los vectores dorados del formato")
    values = []
    for leg in all_legs(cases):
        values += note_values(leg.records, "golden_verified", STORE)
    if not values:
        result.detail = "no service reported verifying the golden vectors"
        return result
    failed = [value >> 32 for value in values]
    checked = [value & 0xFFFF_FFFF for value in values]
    if any(failed):
        result.detail = f"a service failed {max(failed)} golden vectors"
        return result
    if min(checked) == 0:
        result.detail = "a service checked no vectors at all"
        return result
    result.passed = True
    result.detail = (
        f"{len(values)} services each rebuilt {checked[0]} golden vectors with the SHA-256 in "
        f"this repository and failed none"
    )
    return result


def check_medium_formatted(cases: list[Case]) -> Result:
    result = Result("formatted", "un medio en blanco quedó con un almacén que decodifica")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    if not note_values(leg.records, "store_formatted", STORE):
        result.detail = "the service did not report formatting a store"
        return result
    store = read_store(leg.medium)
    if store.chosen is None:
        result.detail = "no valid superblock on the medium the run left"
        return result
    if store.chosen["block_size"] != fmt.BLOCK_SIZE:
        result.detail = f"superblock block size {store.chosen['block_size']}"
        return result
    if not store.records:
        result.detail = "the active arena holds no valid record"
        return result
    result.passed = True
    result.detail = (
        f"{len(store.superblocks)} valid superblocks, the newer at generation "
        f"{store.chosen['superblock_generation']}, {len(store.records)} records in arena "
        f"{store.chosen['active_arena']}"
    )
    result.evidence = [
        f"baseline superblock generation={store.chosen['superblock_generation']} "
        f"published_generation={store.chosen['published_generation']} "
        f"durable_through={store.chosen['durable_through_sequence']}"
    ]
    return result


def check_chain(cases: list[Case]) -> Result:
    result = Result("chain", "cada registro encadena con el anterior y su secuencia sube de uno")
    walked = 0
    kinds: set[int] = set()
    for leg in all_legs(cases):
        store = read_store(leg.medium)
        if store.chosen is None:
            continue
        if not store.records:
            continue
        previous = bytes(32)
        expected = store.records[0]["sequence"]
        for record in store.records:
            if record["prev_digest"] != previous or record["sequence"] != expected:
                result.detail = f"{leg.case}/{leg.index} broke at sequence {record['sequence']}"
                return result
            previous = record["digest"]
            expected += 1
            kinds.add(record["kind"])
            walked += 1
    if walked == 0:
        result.detail = "no records to walk"
        return result
    if len(kinds) < 4:
        result.detail = f"only {len(kinds)} record kinds appear across the matrix"
        return result
    names = sorted(fmt.RECORDKIND_NAME[k] for k in kinds)
    result.passed = True
    result.detail = (
        f"{walked} records across the matrix verify under their own digest, chain by the "
        f"previous one and count by one; {len(kinds)} kinds present: {', '.join(names)}"
    )
    return result


def check_publications_are_durable(cases: list[Case]) -> Result:
    result = Result("published", "cada publicación que el servicio afirma está en el medio")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    claimed = note_values(leg.records, "published", STORE)
    if not claimed:
        result.detail = "the service published nothing"
        return result
    store = read_store(leg.medium)
    if store.chosen is None:
        result.detail = "no valid superblock"
        return result
    # A superseded version is not kept: compaction copies what the published
    # root reaches and nothing else, so the medium accounts for the last
    # publication rather than holding every one. What it may not do is publish
    # a generation nobody claimed, or claim one it never reached.
    if store.chosen["published_generation"] != max(claimed):
        result.detail = (
            f"the superblock publishes {store.chosen['published_generation']} and the last claim "
            f"was {max(claimed)}"
        )
        return result
    for commit in commits(store):
        if commit["new_generation"] not in claimed:
            result.detail = f"the medium holds a commit for generation {commit['new_generation']} nobody claimed"
            return result
    walked = reachable_from(store, store.chosen["published_root"])
    if walked is None:
        result.detail = "the published root names an object the medium does not hold"
        return result
    head = note_values(leg.records, "root_head", STORE)
    if head:
        first = int.from_bytes(store.chosen["published_root"][:8], "little")
        if head[-1] != first:
            result.detail = "the root the service named and the root on the medium differ"
            return result
    uncompacted = None
    for other in cases:
        for other_leg in other.legs:
            candidate = read_store(other_leg.medium)
            if candidate.chosen and commits(candidate):
                uncompacted = (other.name, other_leg.index, commits(candidate))
    if uncompacted is None:
        result.detail = "no leg in the matrix left a commit record to read"
        return result
    result.passed = True
    result.detail = (
        f"{len(claimed)} publications claimed; the superblock publishes "
        f"{store.chosen['published_generation']}, its root reaches {len(walked)} objects that are "
        f"all on the medium, its first eight bytes are the ones the service named, and no commit "
        f"on the medium names a generation nobody claimed"
    )
    result.evidence = [
        f"{uncompacted[0]}/{uncompacted[1]} commit sequence={c['sequence']} "
        f"generation={c['new_generation']} principal={c['principal']} "
        f"request={c['request_sequence']} objects={c['object_count']}"
        for c in uncompacted[2]
    ]
    return result


def check_one_request_one_generation(cases: list[Case]) -> Result:
    result = Result("identity", "una petición, una generación, incluso repetida")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    # Across every medium the matrix left, because one identity committing twice
    # is a thing to look for wherever a commit record survives -- and the
    # baseline's own commits are compacted away.
    seen: dict[tuple[str, int, int], int] = {}
    counted = 0
    for other in cases:
        for other_leg in other.legs:
            for commit in commits(read_store(other_leg.medium)):
                key = (other.name, commit["principal"], commit["request_sequence"])
                if key in seen and seen[key] != commit["new_generation"]:
                    result.detail = (
                        f"{other.name}: principal {key[1]} sequence {key[2]} committed as two "
                        f"generations, {seen[key]} and {commit['new_generation']}"
                    )
                    return result
                seen[key] = commit["new_generation"]
                counted += 1
    published = note_values(leg.records, "client_published", "k4pub")
    if len(published) < 2 or published[-1] != published[-2]:
        result.detail = (
            "the caller did not see the same generation twice for the request it repeated"
        )
        return result
    result.passed = True
    result.detail = (
        f"{counted} commit records across the matrix, {len(seen)} distinct request identities "
        f"among them and no identity carrying two generations; the repeat was answered with "
        f"generation {published[-1]} again (status only the caller received)"
    )
    return result


def check_refusals(cases: list[Case]) -> Result:
    """The four refusals a publication protocol has to make, each one asked for."""
    result = Result("refusals", "las cuatro negativas que un CAS debe hacer, provocadas a propósito")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    wanted = {
        fmt.STORESTATUS["CONFLICT"]: "conflict",
        fmt.STORESTATUS["SEQUENCE_GAP"]: "sequence gap",
        fmt.STORESTATUS["GENERATION_STALE"]: "stale expectation",
    }
    refused = set(note_values(leg.records, "publish_refused", STORE))
    missing = [name for status, name in wanted.items() if status not in refused]
    if missing:
        result.detail = f"the service never refused: {', '.join(missing)}"
        return result
    expected = set(report_values(leg.records, "refused_as_expected", "k4pub"))
    if not set(wanted).issubset(expected):
        result.detail = f"the caller did not see all three: {sorted(expected)}"
        return result
    validation = note_values(leg.records, "validation_refused", "k4pub")
    if fmt.STORESTATUS["VALIDATION_MISMATCH"] not in validation:
        result.detail = "evidence naming other inputs was not refused"
        return result
    conflict = note_values(leg.records, "conflict_refused", STORE)
    if not conflict:
        result.detail = "no request identity was recorded as reused"
        return result
    result.passed = True
    result.detail = (
        "the service refused a reused identity, a gap, a stale expectation and evidence that "
        "names other inputs; the caller saw each one as the status it asked for"
    )
    result.evidence = [line_of(r) for r in notes(leg.records, NOTE["publish_refused"], STORE)]
    return result


def check_reader_may_not_publish(cases: list[Case]) -> Result:
    result = Result("authority", "un lector no publica, y la negativa es sobre publicar")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    forbidden = note_values(leg.records, "publish_forbidden", "k4reader")
    if fmt.STORESTATUS["FORBIDDEN"] not in forbidden:
        result.detail = "the reader was not refused for publishing"
        return result
    read = note_values(leg.records, "client_read", "k4reader")
    if not read:
        result.detail = "the reader never reached the service at all, so nothing was isolated"
        return result
    store = read_store(leg.medium)
    for commit in commits(store):
        if commit["principal"] == 3:
            result.detail = "the reader's principal committed a version"
            return result
    result.passed = True
    result.detail = (
        "the reader reached the service and read from it, was refused FORBIDDEN when it tried to "
        "publish, and owns no commit on the medium"
    )
    return result


def check_effect_admission(cases: list[Case]) -> Result:
    result = Result("effect", "el kernel admitió el efecto antes de cada publicación")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    admitted = by_event(leg.records, "effect.admitted")
    published = note_values(leg.records, "published", STORE)
    if len(admitted) < len(published):
        result.detail = f"{len(published)} publications and only {len(admitted)} admissions"
        return result
    receipts = [r for r in by_event(leg.records, "ctrl.receipt") if r.number("kind") == 2]
    if len(receipts) < len(published):
        result.detail = f"{len(published)} publications and only {len(receipts)} effect receipts"
        return result
    seen = note_values(leg.records, "audit_effect", SUPERVISOR)
    if not seen or seen[-1] < len(published):
        result.detail = f"the auditor counted {seen[-1] if seen else 0} effects for {len(published)} publications"
        return result
    result.passed = True
    result.detail = (
        f"{len(published)} publications, {len(admitted)} admissions in the kernel's records, "
        f"{len(receipts)} effect receipts, and an auditor that counted {seen[-1]} of them from "
        f"the control log rather than from the service"
    )
    result.evidence = [line_of(r) for r in admitted[:3]]
    return result


def check_receipt_plane(cases: list[Case]) -> Result:
    result = Result("receipts", "el plano de recibos se consumió entero y sin huecos")
    checked = 0
    for case in cases:
        for leg in case.legs:
            drained = note_values(leg.records, "audit_drained", SUPERVISOR)
            lost = note_values(leg.records, "audit_lost", SUPERVISOR)
            high = note_values(leg.records, "audit_high_water", SUPERVISOR)
            if not drained:
                result.detail = f"{leg.case}/{leg.index}: the auditor reported nothing"
                return result
            if lost and lost[-1] != 0:
                result.detail = f"{leg.case}/{leg.index}: the log dropped {lost[-1]} receipts"
                return result
            gaps = (high[-1] >> 32) if high else None
            if gaps is None or gaps != 0:
                result.detail = f"{leg.case}/{leg.index}: {gaps} gaps in the receipt sequence"
                return result
            capacity = None
            established = by_event(leg.records, "ctrl.log_established")
            if established:
                capacity = established[0].number("capacity")
            water = high[-1] & 0xFFFF_FFFF
            # Except in the case that stops reading it on purpose, where
            # reaching capacity is the whole point and `control` decides it.
            if case.name != "control-lost" and capacity is not None and water >= capacity:
                result.detail = f"{leg.case}/{leg.index}: the log reached its capacity"
                return result
            checked += 1
    if checked == 0:
        result.detail = "no legs to check"
        return result
    result.passed = True
    result.detail = (
        f"{checked} legs each drained the control log with no gap in the sequence and nothing "
        f"lost; every leg but the one that stops reading on purpose stayed short of capacity"
    )
    return result


def check_control_plane_lost(cases: list[Case]) -> Result:
    """What happens when the receipts a publication needs cannot be written."""
    result = Result("control", "sin plano de control no hay publicación, y el rechazo lo nombra")
    case = case_named(cases, "control-lost")
    if case is None or not case.legs:
        result.detail = "no control-lost case"
        return result
    leg = case.legs[0]
    exhausted = [
        record
        for record in by_event(leg.records, "k2.admission_exhausted")
        if record.get("resource") == "receipt_cells"
    ]
    if not exhausted:
        result.detail = "the log never filled, so nothing was refused for want of a receipt"
        return result
    high = note_values(leg.records, "audit_high_water", SUPERVISOR)
    established = by_event(leg.records, "ctrl.log_established")
    if not high or not established:
        result.detail = "no auditor report or no log to compare it against"
        return result
    capacity = established[0].number("capacity")
    reserved = established[0].number("reserved_cells")
    ordinary = (capacity or 0) - (reserved or 0)
    if (high[-1] & 0xFFFF_FFFF) < ordinary:
        result.detail = f"the log reached {high[-1] & 0xFFFF_FFFF} of {ordinary} ordinary cells"
        return result
    lost = note_values(leg.records, "audit_lost", SUPERVISOR)
    if lost and lost[-1] != 0:
        result.detail = f"{lost[-1]} receipts were lost rather than the work being refused"
        return result
    refused = note_values(leg.records, "publish_refused", STORE)
    if fmt.STORESTATUS["UNAVAILABLE"] not in refused:
        result.detail = f"the service refused with {refused} rather than naming itself unavailable"
        return result
    unreachable = note_values(leg.records, "disk_unreachable", STORE)
    if not unreachable:
        result.detail = "the service did not record that it could not reach the driver"
        return result
    store = read_store(leg.medium)
    published = note_values(leg.records, "published", STORE)
    if store.chosen is None:
        result.detail = "no valid superblock"
        return result
    for commit in commits(store):
        if commit["new_generation"] not in published:
            result.detail = "a version reached the medium that the service never claimed"
            return result
    outcomes = note_values(leg.records, "client_result", "k4pub")
    if not any((value & 0xFF) == fmt.RESULTOUTCOME["UNKNOWN"] for value in outcomes):
        result.detail = "the caller was given an answer rather than being told nobody can say"
        return result
    result.passed = True
    result.detail = (
        f"the auditor stopped reading, the log filled to all {ordinary} ordinary cells and the "
        f"kernel refused the admissions their receipts would have covered rather than losing "
        f"one; the service could not reach the driver, refused with UNAVAILABLE rather than "
        f"calling an intact store damaged, and told the caller UNKNOWN rather than a result"
    )
    result.evidence = [line_of(exhausted[0]), line_of(established[0])]
    return result


def check_compaction(cases: list[Case]) -> Result:
    result = Result("compaction", "la compactación copió el conjunto alcanzable y cambió de arena")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    copied = note_values(leg.records, "compacted", STORE)
    if not copied:
        result.detail = "no compaction happened"
        return result
    store = read_store(leg.medium)
    if store.chosen is None:
        result.detail = "no valid superblock"
        return result
    switched = note_values(leg.records, "superblock_switched", STORE)
    if len(switched) < 2:
        result.detail = f"only {len(switched)} superblock switches"
        return result
    if store.chosen["active_arena"] == 0:
        result.detail = "the medium still names the arena the store started in"
        return result
    walked = reachable_from(store, store.chosen["published_root"])
    if walked is None:
        result.detail = "after compaction the published root names an object the arena lacks"
        return result
    if len(walked) > copied[-1]:
        result.detail = f"{len(walked)} objects are reachable and only {copied[-1]} were copied"
        return result
    heads = checkpoints(store)
    if not heads:
        result.detail = "the compacted arena opens with no checkpoint"
        return result
    result.passed = True
    result.detail = (
        f"{copied[-1]} objects copied into arena {store.chosen['active_arena']}, which opens with "
        f"a checkpoint at sequence {heads[0]['sequence']}; the published root still reaches "
        f"{len(walked)} objects, all of them in the new arena"
    )
    return result


def check_outbox(cases: list[Case]) -> Result:
    result = Result("outbox", "lo que el broker contestó quedó durable, incluido no saber")
    delivered = case_named(cases, "baseline")
    unknown = case_named(cases, "broker-unknown")
    if delivered is None or not delivered.legs:
        result.detail = "no baseline case"
        return result
    leg = delivered.legs[0]
    # A compacted arena carries the outbox result forward in its checkpoint
    # rather than as a record, so the record itself is read where one survives.
    # `cut-after-checkpoint` is the case whose arena still holds the records of
    # all three publications: it is cut before the compaction that would carry
    # the outbox result forward in a checkpoint and drop the record.
    holder = case_named(cases, "cut-after-checkpoint")
    if holder is None or not holder.legs:
        result.detail = "no cut-after-checkpoint case to read an outbox record from"
        return result
    where = f"{holder.name}/{holder.legs[0].index}"
    entries = outboxes(read_store(holder.legs[0].medium))
    if not entries:
        result.detail = f"no outbox record on {where}"
        return result
    store = read_store(leg.medium)
    entries = [e for e in entries if e["status"] == fmt.OUTBOXSTATUS["DELIVERED"]]
    answered = note_values(leg.records, "broker_answered", "k4broker")
    if fmt.OUTBOXSTATUS["DELIVERED"] not in answered:
        result.detail = "the broker never answered DELIVERED"
        return result
    detail = (
        f"the broker answered DELIVERED and {where} holds it at sequence "
        f"{entries[0]['sequence']} for principal {entries[0]['principal']} request "
        f"{entries[0]['request_sequence']}"
    )
    if unknown is not None and unknown.legs:
        said = note_values(unknown.legs[0].records, "broker_answered", "k4broker")
        if fmt.OUTBOXSTATUS["UNKNOWN"] not in said:
            result.detail = "the broker that cannot say did not say so"
            return result
        recorded = note_values(unknown.legs[0].records, "outbox_recorded", STORE)
        if fmt.OUTBOXSTATUS["UNKNOWN"] not in recorded:
            result.detail = f"the service recorded {recorded} rather than UNKNOWN"
            return result
        detail += "; the case where it cannot say recorded UNKNOWN rather than a guess"
    result.passed = True
    result.detail = detail
    result.evidence = [
        f"outbox sequence={e['sequence']} status={fmt.OUTBOXSTATUS_NAME[e['status']]} "
        f"attempts={e['attempts']} target=0x{e['target']:x}"
        for e in entries
    ]
    return result


def directive_of(leg: Leg) -> tuple[str, str]:
    return leg.directive.get("fault_point", "NONE"), leg.directive.get("fault_mode", "NONE")


def check_cuts_were_applied(cases: list[Case]) -> Result:
    result = Result("cuts", "cada corte que la directiva nombra ocurrió donde lo nombra")
    applied = []
    for case in cases:
        if not case.legs:
            continue
        leg = case.legs[0]
        point, mode = directive_of(leg)
        if point == "NONE":
            if notes(leg.records, NOTE["fault_applied"], STORE):
                result.detail = f"{case.name}: a fault was applied and none was asked for"
                return result
            continue
        seen = note_values(leg.records, "fault_applied", STORE)
        if not seen:
            result.detail = f"{case.name}: the directive named {point}/{mode} and nothing applied it"
            return result
        value = seen[0]
        if fmt.FAULTPOINT_NAME.get(value & 0xFF) != point:
            result.detail = f"{case.name}: applied at {value & 0xFF}, directive said {point}"
            return result
        if fmt.FAULTMODE_NAME.get((value >> 8) & 0xFF) != mode:
            result.detail = f"{case.name}: applied {(value >> 8) & 0xFF}, directive said {mode}"
            return result
        applied.append(f"{case.name} {point}/{mode}")
    if len(applied) < 8:
        result.detail = f"only {len(applied)} cases cut anything"
        return result
    result.passed = True
    result.detail = f"{len(applied)} cuts, each at the point and in the mode its directive named"
    result.evidence = applied
    return result


def declared_cuts(case: Case) -> set[int]:
    """Legs the case's own spec says it cuts, read from what was asked for."""
    legs = set()
    for spec in case.spec.get("cuts", []):
        head = str(spec).split(":", 1)[0]
        if head.isdigit():
            legs.add(int(head))
    return legs


def check_later_legs_are_not_cut(cases: list[Case]) -> Result:
    result = Result(
        "recovery_uncut", "la recuperación no consulta la directiva ni se corta sin pedirlo"
    )
    checked = 0
    declared = 0
    for case in cases:
        asked = declared_cuts(case)
        for leg in case.legs[1:]:
            point, mode = directive_of(leg)
            if leg.index in asked:
                # A case may cut a later leg on purpose, which is how the run
                # gets a look at what recovery itself wrote before a compaction
                # rewrites the arena over it. What it may not do is get one
                # nobody asked for.
                declared += 1
                continue
            if point != "NONE" or mode != "NONE":
                result.detail = f"{case.name}/{leg.index} was handed a directive naming {point}"
                return result
            if notes(leg.records, NOTE["fault_applied"], STORE):
                result.detail = f"{case.name}/{leg.index} applied a fault nobody asked for"
                return result
            checked += 1
    if checked == 0:
        result.detail = "no case ran an uncut second leg"
        return result
    result.passed = True
    result.detail = (
        f"{checked} recovery legs handed a directive that names no point, none of which applied "
        f"one; the {declared} later legs that were cut are the ones their case asks for by number"
    )
    return result


def check_suppression_matches_the_medium(cases: list[Case]) -> Result:
    """What the driver says it withheld, checked against what the medium lacks."""
    result = Result("suppression", "lo que el driver retuvo es lo que al medio le falta")
    checked = []
    for case in cases:
        if not case.legs:
            continue
        leg = case.legs[0]
        point, mode = directive_of(leg)
        if mode not in ("DROP_WRITE", "TEAR_WRITE", "IO_ERROR", "REORDER"):
            continue
        suppressed = note_values(leg.records, "disk_suppressed", DISK)
        errors = note_values(leg.records, "disk_io_error", DISK)
        reordered = note_values(leg.records, "disk_reordered", DISK)
        if not (suppressed or errors or reordered):
            result.detail = f"{case.name}: the driver withheld nothing"
            return result
        store = read_store(leg.medium)
        if store.broken_at is None:
            result.detail = (
                f"{case.name}: the medium's valid prefix runs to the end of the arena, so "
                f"nothing the driver withheld is missing from it"
            )
            return result
        blocks = [(value >> 8) for value in suppressed] + errors + reordered
        withheld = min(blocks) if blocks else None
        if withheld is not None and store.broken_at > withheld:
            result.detail = (
                f"{case.name}: the driver withheld block {withheld} and the prefix still runs to "
                f"{store.broken_at}"
            )
            return result
        checked.append(
            f"{case.name} {mode} withheld={blocks} prefix_ends_at_block={store.broken_at}"
        )
    if len(checked) < 3:
        result.detail = f"only {len(checked)} cases withheld a write"
        return result
    result.passed = True
    result.detail = (
        f"{len(checked)} cases withheld a write in the driver, and in each the medium's valid "
        f"prefix ends rather than running to the end of the arena"
    )
    result.evidence = checked
    return result


def check_unresolved_prepare_is_aborted(cases: list[Case]) -> Result:
    result = Result("abort", "un prepare sin resolver se recupera como abort, nunca como versión")
    case = case_named(cases, "abort-visible")
    if case is None or len(case.legs) < 2:
        result.detail = "no abort-visible case with a recovery leg"
        return result
    first, second = case.legs[0], case.legs[1]
    before = read_store(first.medium)
    if not prepares(before):
        result.detail = "the cut leg left no prepare"
        return result
    if commits(before):
        result.detail = "the cut leg left a commit, so nothing was unresolved"
        return result
    aborted = note_values(second.records, "recovery_aborted", STORE)
    if not aborted:
        result.detail = "the recovery leg resolved nothing"
        return result
    after = read_store(second.medium)
    written = aborts(after)
    if not written:
        result.detail = "the recovery leg wrote no abort record"
        return result
    prepare = prepares(after)[0]
    if written[0]["prepare_sequence"] != prepare["sequence"]:
        result.detail = "the abort names a different prepare than the one on the medium"
        return result
    if written[0]["reason"] != fmt.ABORTREASON["RECOVERED_UNRESOLVED"]:
        result.detail = f"the abort reason is {written[0]['reason']}"
        return result
    if commits(after):
        result.detail = "the recovery leg turned the prepare into a version"
        return result
    recovered = note_values(second.records, "store_recovered", STORE)
    if not recovered or recovered[-1] != 0:
        result.detail = f"the service adopted generation {recovered} from a store with no commit"
        return result
    result.passed = True
    result.detail = (
        f"the cut left one prepare and no commit; recovery wrote an abort at sequence "
        f"{written[0]['sequence']} naming prepare {prepare['sequence']} with reason "
        f"RECOVERED_UNRESOLVED, and adopted generation 0"
    )
    result.evidence = [
        f"cut prefix: {len(before.records)} records, ends at block {before.broken_at}",
        f"recovered prefix: {len(after.records)} records, ends at block {after.broken_at}",
    ]
    return result


def check_durable_commit_is_adopted(cases: list[Case]) -> Result:
    result = Result("adopt", "un commit durable se adopta aunque el checkpoint no llegara")
    case = case_named(cases, "cut-after-commit")
    if case is None or len(case.legs) < 2:
        result.detail = "no cut-after-commit case with a recovery leg"
        return result
    first, second = case.legs[0], case.legs[1]
    before = read_store(first.medium)
    made = commits(before)
    if not made:
        result.detail = "the cut leg left no commit"
        return result
    if before.chosen is None or before.chosen["published_generation"] >= made[-1]["new_generation"]:
        result.detail = "the superblock already named the version, so nothing had to be recovered"
        return result
    recovered = note_values(second.records, "store_recovered", STORE)
    if not recovered or recovered[-1] != made[-1]["new_generation"]:
        result.detail = (
            f"the medium holds a commit for generation {made[-1]['new_generation']} and the "
            f"service recovered {recovered}"
        )
        return result
    result.passed = True
    result.detail = (
        f"the cut left a commit for generation {made[-1]['new_generation']} under a superblock "
        f"still publishing {before.chosen['published_generation']}; the next boot read the record "
        f"and adopted {recovered[-1]}"
    )
    result.evidence = [
        f"commit sequence={made[-1]['sequence']} generation={made[-1]['new_generation']}",
        f"superblock generation={before.chosen['superblock_generation']} "
        f"published={before.chosen['published_generation']}",
    ]
    return result


def check_torn_record_is_refused(cases: list[Case]) -> Result:
    result = Result("torn", "un registro desgarrado corta el prefijo y no se lee a medias")
    case = case_named(cases, "tear-object")
    if case is None or len(case.legs) < 2:
        result.detail = "no tear-object case with a recovery leg"
        return result
    first, second = case.legs[0], case.legs[1]
    before = read_store(first.medium)
    if before.broken_at is None:
        result.detail = "the torn write left a prefix that runs to the end of the arena"
        return result
    torn = fmt.read_record(first.medium, before.broken_at)
    if torn is not None:
        result.detail = f"block {before.broken_at} still decodes as a record"
        return result
    head = first.medium[
        before.broken_at * fmt.BLOCK_SIZE : before.broken_at * fmt.BLOCK_SIZE + 8
    ]
    if head != fmt.MAGIC["record"]:
        result.detail = "the torn block does not even carry a record magic, so nothing was torn"
        return result
    scanned = note_values(second.records, "recovery_scanned", STORE)
    if not scanned:
        result.detail = "the recovery leg scanned nothing"
        return result
    if scanned[-1] != len(before.records):
        result.detail = (
            f"the medium's valid prefix is {len(before.records)} records and the service scanned "
            f"{scanned[-1]}"
        )
        return result
    result.passed = True
    result.detail = (
        f"block {before.broken_at} carries a record magic and does not verify; the host walks "
        f"{len(before.records)} records before it and the guest scanned exactly {scanned[-1]}"
    )
    return result


def check_result_survives_a_lost_answer(cases: list[Case]) -> Result:
    result = Result("retry", "una respuesta perdida se contesta desde el resultado durable")
    case = case_named(cases, "cut-after-superblock")
    if case is None or len(case.legs) < 2:
        result.detail = "no cut-after-superblock case with a recovery leg"
        return result
    first, second = case.legs[0], case.legs[1]
    if not note_values(first.records, "service_stopped", STORE):
        result.detail = "the service was not stopped without answering"
        return result
    resumed = note_values(second.records, "client_resumed", "k4pub")
    if not resumed:
        result.detail = "the client did not ask what became of its own requests"
        return result
    spent = resumed[-1] & 0xFF
    outcome = (resumed[-1] >> 8) & 0xFF
    if spent == 0:
        result.detail = "the client found none of its requests spent, so nothing was retried"
        return result
    if outcome not in (
        fmt.RESULTOUTCOME["COMMITTED"],
        fmt.RESULTOUTCOME["ABORTED"],
        fmt.RESULTOUTCOME["UNKNOWN"],
    ):
        result.detail = f"the client resumed on outcome {outcome}"
        return result
    after = read_store(second.medium)
    seen: set[tuple[int, int]] = set()
    for commit in commits(after):
        key = (commit["principal"], commit["request_sequence"])
        if key in seen:
            result.detail = f"identity {key} committed twice across the two legs"
            return result
        seen.add(key)
    result.passed = True
    result.detail = (
        f"the run was cut without answering; on the next boot the client asked and found "
        f"{spent} of its request identities already spent, the last as "
        f"{fmt.RESULTOUTCOME_NAME[outcome]}, and started again after it"
    )
    return result


def check_service_replaced(cases: list[Case]) -> Result:
    result = Result("replace", "el supervisor reemplazó el servicio dentro de una ejecución")
    case = case_named(cases, "kill-service")
    if case is None or not case.legs:
        result.detail = "no kill-service case"
        return result
    leg = case.legs[0]
    restarted = note_values(leg.records, "super_restarted", SUPERVISOR)
    if not restarted:
        result.detail = "the supervisor started no replacement"
        return result
    instances = note_values(leg.records, "super_built", STORE)
    if len(instances) < 2:
        result.detail = f"only {len(instances)} service instances announced themselves"
        return result
    terminated = [
        r for r in by_event(leg.records, "domain.terminated") if r.get("name") == STORE
    ]
    if not terminated:
        result.detail = "the first service was never terminated"
        return result
    ready = note_values(leg.records, "store_ready", STORE)
    if len(ready) < 2:
        result.detail = "the replacement never admitted requests"
        return result
    result.passed = True
    result.detail = (
        f"the service asked to be replaced, the supervisor terminated it and built instance "
        f"{restarted[-1] + 1}, and the replacement recovered and admitted requests"
    )
    result.evidence = [line_of(terminated[0])]
    return result


def check_rival(cases: list[Case]) -> Result:
    result = Result("rival", "dos publicadores pidieron la misma transición y solo uno la obtuvo")
    case = case_named(cases, "rival")
    if case is None or not case.legs:
        result.detail = "no rival case"
        return result
    leg = case.legs[0]
    store = read_store(leg.medium)
    # The contested transition is the first one: both clients fork over the
    # published generation they found and ask to become the next.
    contested = 1
    claimed = note_values(leg.records, "published", STORE)
    if claimed.count(contested) != 1:
        result.detail = f"the service claims generation {contested} {claimed.count(contested)} times"
        return result
    # Whichever of the two won, exactly one of them may have been told it did.
    winners = [
        name
        for name in ("k4pub", "k4rival")
        if contested in note_values(leg.records, "client_published", name)
    ]
    if len(winners) != 1:
        result.detail = f"{len(winners)} clients were told they had published generation {contested}"
        return result
    loser = "k4rival" if winners[0] == "k4pub" else "k4pub"
    refused = note_values(leg.records, "client_refused", loser)
    if not refused:
        result.detail = f"{loser} lost the race and was not refused"
        return result
    # A commit for the contested generation may or may not survive compaction,
    # but wherever one does, only one may name it.
    naming = 0
    for other_leg in case.legs:
        for commit in commits(read_store(other_leg.medium)):
            if commit["new_generation"] == contested:
                naming += 1
    if naming > 1:
        result.detail = f"{naming} commit records name generation {contested}"
        return result
    durable = any(c["new_generation"] == contested for c in commits(store)) or (
        store.chosen is not None and store.chosen["published_generation"] >= contested
    )
    if not durable:
        result.detail = "the medium holds neither a commit for the contested generation nor a superblock naming it"
        return result
    result.passed = True
    result.detail = (
        f"two principals asked to become generation {contested}; the service claims it once, "
        f"{winners[0]} was told it had it, {loser} was refused with status {refused[0]}, and no "
        f"medium in the case holds two commits naming it"
    )
    result.evidence = [
        f"prepare sequence={p['sequence']} principal={p['principal']} "
        f"expected={p['expected_generation']}"
        for p in prepares(store)
    ]
    return result


def check_coverage(cases: list[Case]) -> Result:
    result = Result("coverage", "las operaciones sobre las que descansan las afirmaciones K4 se recorrieron")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    coverage = by_event(leg.records, "k2.coverage")
    if len(coverage) != 1:
        result.detail = "the kernel reported no single coverage record"
        return result
    assigned = coverage[0].number("operations_assigned")
    reached = coverage[0].number("operations_reached")
    untouched = {r.get("name") for r in by_event(leg.records, "k2.operation_untouched")}
    if assigned is None or reached is None:
        result.detail = "the coverage record is missing a count"
        return result
    if reached != assigned - len(untouched):
        result.detail = f"{reached} reached of {assigned} with {len(untouched)} named untouched"
        return result
    missing = sorted(K4_REQUIRED_OPERATIONS & untouched)
    if missing:
        result.detail = f"K4 rests on operations it never reached: {', '.join(missing)}"
        return result
    # Resolving an invocation without answering it only happens where a
    # publication does not commit, so the baseline cannot reach it and a case
    # that is cut has to. Requiring it of the baseline would either be false or
    # would make the baseline fail a publication on purpose.
    elsewhere = set()
    for other in cases:
        for other_leg in other.legs:
            skipped = {r.get("name") for r in by_event(other_leg.records, "k2.operation_untouched")}
            if by_event(other_leg.records, "k2.coverage") and "INVOCATION_RESOLVE" not in skipped:
                elsewhere.add(f"{other.name}/{other_leg.index}")
    if not elsewhere:
        result.detail = "no leg in the matrix ever resolved an invocation without answering it"
        return result
    result.passed = True
    result.detail = (
        f"{reached} of {assigned} operations reached; all {len(K4_REQUIRED_OPERATIONS)} the K4 "
        f"claims rest on are among them, the {len(untouched)} untouched are named one by one, and "
        f"{len(elsewhere)} cut legs also resolved an invocation without answering it"
    )
    result.evidence = [f"untouched: {', '.join(sorted(n for n in untouched if n))}"]
    return result


def check_durability_profile(cases: list[Case]) -> Result:
    """What this run may and may not claim about durability, from the run record."""
    result = Result("profile", "el perfil de durabilidad declara sus dependencias en vez de suponerlas")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    run = case.spec.get("run", {})
    medium = run.get("medium", {})
    if medium.get("cache") != "writeback":
        result.detail = f"the medium's cache mode is {medium.get('cache')!r}, not writeback"
        return result
    if "guest driver" not in medium.get("suppression", ""):
        result.detail = "the run record does not say who suppressed the writes"
        return result
    leg = case.legs[0]
    flushes = note_values(leg.records, "disk_flush", DISK)
    if not flushes:
        result.detail = "the driver acknowledged no flush, so ordering rests on nothing"
        return result
    device = by_event(leg.records, "device.summary")
    if len(device) != 1:
        result.detail = "no device summary"
        return result
    if device[0].number("remapping_programmed") != 0:
        result.detail = "this platform claims a remapping unit it does not have"
        return result
    result.passed = True
    result.detail = (
        f"raw medium behind virtio-blk with cache=writeback, {flushes[-1]} flushes acknowledged "
        f"by the device, suppression done in the guest's own driver so QEMU saw only the writes "
        f"that were issued, and no remapping unit programmed"
    )
    result.evidence = [
        f"medium: {medium.get('format')} {medium.get('mib')} MiB cache={medium.get('cache')}",
        f"suppression: {medium.get('suppression')}",
        line_of(device[0]),
    ]
    return result


def check_staging_is_bounded(cases: list[Case]) -> Result:
    result = Result("staging", "el área de preparación se recupera en vez de agotarse")
    case = case_named(cases, "baseline")
    if case is None or not case.legs:
        result.detail = "no baseline case"
        return result
    leg = case.legs[0]
    # The sweep runs where staging is under pressure, which is a leg that keeps
    # publishing after a recovery -- not the baseline, whose script fits. What
    # matters is that it runs somewhere and that it never has to give up.
    reclaimed: list[int] = []
    swept = []
    for other in cases:
        for other_leg in other.legs:
            found = note_values(other_leg.records, "staging_reclaimed", STORE)
            if found:
                reclaimed += found
                swept.append(f"{other.name}/{other_leg.index}")
    if not reclaimed:
        result.detail = "no leg in the matrix ever swept staging, so the sweep is not exercised"
        return result
    exhausted = note_values(leg.records, "exhausted", STORE)
    # Across the matrix, because the case that presses hardest on staging is
    # the one with three clients preparing candidates at once, not the baseline.
    for other in cases:
        for other_leg in other.legs:
            starved = note_values(other_leg.records, "staging_exhausted", STORE)
            if starved:
                result.detail = (
                    f"{other.name}/{other_leg.index}: a sweep freed nothing with {starved[0]} "
                    f"digests kept, so staging refused work"
                )
                return result
            workspaces = note_values(other_leg.records, "workspaces_exhausted", STORE)
            if workspaces:
                result.detail = (
                    f"{other.name}/{other_leg.index}: a fork was refused for want of a workspace"
                )
                return result
    store = read_store(leg.medium)
    walked = reachable_from(store, store.chosen["published_root"]) if store.chosen else None
    if walked is None:
        result.detail = "the sweep dropped something the published version still names"
        return result
    result.passed = True
    result.detail = (
        f"{len(reclaimed)} sweeps across {len(swept)} legs freed {sum(reclaimed)} staged objects "
        f"between them; in no leg of any case did a sweep free nothing or a fork run out of "
        f"workspaces, and the baseline's published root still reaches all {len(walked)} objects "
        f"it names"
    )
    result.evidence = [f"swept in {', '.join(swept)}"]
    if exhausted:
        result.detail += f"; the reserve refused admission {len(exhausted)} times"
    return result


def check_regression(gate: dict | None, name: str, criteria: int) -> Result:
    result = Result(name.lower(), f"la regresión {name} sigue verde sobre este kernel")
    if gate is None:
        result.detail = f"no {name} verdict; run tools/check_{name.lower()}.py --json first"
        return result
    if not gate.get("passed"):
        failed = [c["title"] for c in gate.get("criteria", []) if not c.get("passed")]
        result.detail = f"{name} failed: {', '.join(failed[:3])}"
        return result
    met = len(gate.get("criteria", []))
    if met != criteria:
        result.detail = f"{name} decided {met} criteria and this gate expects {criteria}"
        return result
    result.passed = True
    result.detail = f"{name} met {met} of {met} criteria on this tree"
    return result


def check_reproducible(cases: list[Case], manifest: dict | None) -> Result:
    result = Result("reproducible", "la imagen ejecutada es la que el manifiesto describe")
    if manifest is None:
        result.detail = "no image manifest"
        return result
    case = case_named(cases, "baseline")
    if case is None:
        result.detail = "no baseline case"
        return result
    image = Path(case.spec.get("run", {}).get("image", ""))
    if not image.exists():
        result.detail = f"the image the run names is gone: {image}"
        return result
    digest = hashlib.sha256(image.read_bytes()).hexdigest()
    recorded = manifest.get("artifacts", {}).get("image", {}).get("sha256")
    if digest != recorded:
        result.detail = "the image on disk is not the one the manifest describes"
        return result
    names = {module["name"] for module in manifest.get("modules", [])}
    if not {"k4super", "k4store", "k4disk", "k4client"}.issubset(names):
        result.detail = f"the package holds {sorted(names)}"
        return result
    result.passed = True
    result.detail = (
        f"image sha256 {digest[:16]}… matches the manifest, built by {manifest.get('rust')} "
        f"for {manifest.get('targets', {}).get('kernel')}, four modules"
    )
    return result


CRITERIA = [
    check_legs_completed,
    check_no_user_faults,
    check_nothing_unexpected,
    check_format_agreement,
    check_medium_formatted,
    check_chain,
    check_publications_are_durable,
    check_one_request_one_generation,
    check_refusals,
    check_reader_may_not_publish,
    check_effect_admission,
    check_receipt_plane,
    check_control_plane_lost,
    check_compaction,
    check_outbox,
    check_cuts_were_applied,
    check_later_legs_are_not_cut,
    check_suppression_matches_the_medium,
    check_unresolved_prepare_is_aborted,
    check_durable_commit_is_adopted,
    check_torn_record_is_refused,
    check_result_survives_a_lost_answer,
    check_service_replaced,
    check_rival,
    check_coverage,
    check_durability_profile,
    check_staging_is_bounded,
]


# --- loading ----------------------------------------------------------------


def load_cases(root: Path) -> list[Case]:
    index = root / "cases.json"
    if not index.exists():
        return []
    cases = []
    for entry in json.loads(index.read_text()):
        run = entry.get("run")
        if run is None:
            continue
        legs = []
        directives = run.get("directives", [])
        for leg in run.get("legs", []):
            log = ROOT / leg["serial_log"]
            medium = ROOT / leg["medium_after"]
            if not log.exists() or not medium.exists():
                continue
            index_of = leg["leg"] - 1
            legs.append(
                Leg(
                    case=entry["case"],
                    index=leg["leg"],
                    records=parse(log.read_text(errors="replace")),
                    exit_status=leg["exit_status"],
                    timed_out=leg["timed_out"],
                    medium=medium.read_bytes(),
                    directive=directives[index_of] if index_of < len(directives) else {},
                )
            )
        cases.append(Case(entry["case"], entry, legs))
    return cases


# --- self-test --------------------------------------------------------------
#
# A gate nobody has damaged is a gate nobody has tested. Each mutation below
# breaks the evidence one way, and the criterion named beside it has to notice.
# Log mutations are rewrites of the serial text; medium mutations are byte
# edits of one leg's image, because half of what this gate reads is not a log.


def drop_event(event: str):
    def mutate(text: str) -> str:
        return "\n".join(line for line in text.splitlines() if f" {event} " not in line)

    return mutate


def rewrite_re(pattern: str, replacement: str):
    compiled = re.compile(pattern)

    def mutate(text: str) -> str:
        return "\n".join(compiled.sub(replacement, line) for line in text.splitlines())

    return mutate


def drop_note(code: int):
    marker = re.compile(rf"user\.note .*a=0x{code:x}\b")

    def mutate(text: str) -> str:
        return "\n".join(line for line in text.splitlines() if not marker.search(line))

    return mutate


def flip_block(block: int, offset: int = 0):
    """Flips one byte inside a block of the medium."""

    def mutate(image: bytes) -> bytes:
        blob = bytearray(image)
        at = block * fmt.BLOCK_SIZE + offset
        blob[at] ^= 0xFF
        return bytes(blob)

    return mutate


def zero_blocks(*blocks: int):
    def mutate(image: bytes) -> bytes:
        blob = bytearray(image)
        for block in blocks:
            at = block * fmt.BLOCK_SIZE
            blob[at : at + fmt.BLOCK_SIZE] = bytes(fmt.BLOCK_SIZE)
        return bytes(blob)

    return mutate


def flip_record(kind: str, which: int = 0):
    """Damages the `which`-th record of `kind` in whichever arena is live.

    Naming a block number would make a mutation stop damaging anything the
    moment a run lays its records out differently, and a self-test that damages
    nothing reports success for a check it never made. This finds the record
    the criterion is about and breaks its digest.
    """

    def mutate(image: bytes) -> bytes:
        store = read_store(image)
        found = [r for r in store.records if r["kind"] == fmt.RECORDKIND[kind]]
        if len(found) <= which:
            return image
        blob = bytearray(image)
        header = fmt.STRUCTS["RecordHeader"][0]
        at = found[which]["block"] * fmt.BLOCK_SIZE + header
        blob[at] ^= 0xFF
        return bytes(blob)

    return mutate


# (name, case, leg index, what to damage, how, criterion that must fail)
LOG_MUTATIONS = [
    ("no terminal record", "baseline", 1, drop_event("k1.terminal"), "legs"),
    ("a domain that faulted", "baseline", 1, rewrite_re(r"user_faults=\d+", "user_faults=1"), "faults"),
    (
        "a program that recorded an unexpected result",
        "baseline",
        1,
        rewrite_re(r"(user\.note domain=2 [^\n]*?)a=0x4016", r"\1a=0x200c"),
        "unexpected",
    ),
    ("golden vectors that failed", "baseline", 1, rewrite_re(r"a=0x401d b=0x16", "a=0x401d b=0x100000016"), "format"),
    ("a service that never formatted a store", "baseline", 1, drop_note(NOTE["store_formatted"]), "formatted"),
    ("publications the service never claimed", "baseline", 1, drop_note(NOTE["published"]), "published"),
    (
        "a publication claiming a generation the medium lacks",
        "baseline",
        1,
        rewrite_re(r"a=0x4016 b=0x3", "a=0x4016 b=0x9"),
        "published",
    ),
    ("a caller that never saw its repeat answered", "baseline", 1, drop_note(NOTE["client_published"]), "identity"),
    ("no refusal for a reused identity", "baseline", 1, drop_note(NOTE["conflict_refused"]), "refusals"),
    ("a reader that was never refused", "baseline", 1, drop_note(NOTE["publish_forbidden"]), "authority"),
    ("no effect admitted", "baseline", 1, drop_event("effect.admitted"), "effect"),
    ("an auditor that counted no effects", "baseline", 1, drop_note(NOTE["audit_effect"]), "effect"),
    ("receipts the log says it lost", "baseline", 1, rewrite_re(r"a=0x4047 b=0x0", "a=0x4047 b=0x4"), "receipts"),
    ("a gap in the receipt sequence", "baseline", 1, rewrite_re(r"a=0x4048 b=0x([0-9a-f]+)", r"a=0x4048 b=0x100000000"), "receipts"),
    ("no compaction", "baseline", 1, drop_note(NOTE["compacted"]), "compaction"),
    ("a broker that never answered", "baseline", 1, drop_note(NOTE["broker_answered"]), "outbox"),
    ("a cut nothing applied", "cut-after-prepare", 1, drop_note(NOTE["fault_applied"]), "cuts"),
    ("a recovery leg that applied a fault", "cut-after-prepare", 2, rewrite_re(r"(a=0x4012) b=", r"a=0x4022 b="), "recovery_uncut"),
    ("a driver that withheld nothing", "tear-object", 1, drop_note(NOTE["disk_suppressed"]), "suppression"),
    ("recovery that resolved nothing", "abort-visible", 2, drop_note(NOTE["recovery_aborted"]), "abort"),
    (
        "recovery that adopted a version from a store with none",
        "abort-visible",
        2,
        rewrite_re(r"a=0x4011 b=0x0", "a=0x4011 b=0x1"),
        "abort",
    ),
    ("a service that adopted nothing from a durable commit", "cut-after-commit", 2, drop_note(NOTE["store_recovered"]), "adopt"),
    ("a scan that disagrees with the medium's prefix", "tear-object", 2, rewrite_re(r"a=0x4012 b=0x[0-9a-f]+", "a=0x4012 b=0x63"), "torn"),
    ("a client that never asked what it had already done", "cut-after-superblock", 2, drop_note(NOTE["client_resumed"]), "retry"),
    ("a supervisor that started no replacement", "kill-service", 1, drop_note(NOTE["super_restarted"]), "replace"),
    ("the loser of the race, never refused", "rival", 1, drop_note(NOTE["client_refused"]), "rival"),
    (
        "both racers told they had published",
        "rival",
        1,
        rewrite_re(r"(name=k4pub[^\n]*)a=0x4032 b=0x[0-9a-f]+", r"\1a=0x4031 b=0x1"),
        "rival",
    ),
    (
        "a sweep that freed nothing",
        "cut-after-checkpoint",
        2,
        rewrite_re(r"a=0x402c b=0x([0-9a-f]+)", r"a=0x402d b=0x\1"),
        "staging",
    ),
    ("no coverage record", "baseline", 1, drop_event("k2.coverage"), "coverage"),
    (
        "an operation K4 rests on, never reached",
        "baseline",
        1,
        rewrite_re(r"(k2\.operation_untouched operation=0x3 name=)CAP_COPY", r"\1ENDPOINT_CALL"),
        "coverage",
    ),
    ("a driver that acknowledged no flush", "baseline", 1, drop_note(NOTE["disk_flush"]), "profile"),
    ("a platform claiming a remapping unit", "baseline", 1, rewrite_re(r"remapping_programmed=0", "remapping_programmed=1"), "profile"),

    ("a control log that never filled", "control-lost", 1, drop_event("k2.admission_exhausted"), "control"),
    (
        "a control log that lost receipts instead of refusing work",
        "control-lost",
        1,
        rewrite_re(r"a=0x4047 b=0x0", "a=0x4047 b=0x7"),
        "control",
    ),
    (
        "a service that called an intact store damaged",
        "control-lost",
        1,
        rewrite_re(r"a=0x4017 b=0xb", "a=0x4017 b=0xd"),
        "control",
    ),
    (
        "a fork refused for want of a workspace",
        "cut-after-checkpoint",
        2,
        rewrite_re(r"a=0x402c b=0x[0-9a-f]+", "a=0x402b b=0x11111"),
        "staging",
    ),
]

# (name, mutation, criterion that must fail) applied to every leg of every case,
# for the criteria whose claim is about the matrix rather than about one run.
MATRIX_MUTATIONS = [
    ("staging that was never swept anywhere", drop_note(NOTE["staging_reclaimed"]), "staging"),
    ("no leg that resolved an invocation without answering", drop_event("k2.operation_untouched"), "coverage"),
    ("no commit record anywhere in the matrix", drop_note(NOTE["client_published"]), "identity"),
]

# (name, case, leg index, how to damage the medium, criterion that must fail)
MEDIUM_MUTATIONS = [
    (
        "both superblocks gone",
        "baseline",
        1,
        zero_blocks(
            fmt.STORE_BASE_BLOCK + fmt.SUPERBLOCK_A_BLOCK,
            fmt.STORE_BASE_BLOCK + fmt.SUPERBLOCK_B_BLOCK,
        ),
        "formatted",
    ),
    (
        "a superblock whose digest does not cover it",
        "baseline",
        1,
        flip_block(fmt.STORE_BASE_BLOCK + fmt.SUPERBLOCK_B_BLOCK, 32),
        "published",
    ),
    ("the checkpoint the arena opens with, damaged", "baseline", 1, flip_record("CHECKPOINT"), "published"),
    ("an object the published root names, gone", "baseline", 1, flip_record("OBJECT", 1), "compaction"),
    ("the abort record recovery wrote, gone", "abort-visible", 2, flip_record("ABORT"), "abort"),
    ("the commit the next boot adopted, gone", "cut-after-commit", 1, flip_record("COMMIT"), "adopt"),
    ("the prepare the cut left behind, gone", "abort-visible", 1, flip_record("PREPARE"), "abort"),
    (
        "the torn block, made whole again",
        "tear-object",
        1,
        zero_blocks(fmt.STORE_BASE_BLOCK + fmt.ARENA0_START_BLOCK + 6),
        "torn",
    ),
    ("the outbox record, gone", "cut-after-checkpoint", 1, flip_record("OUTBOX"), "outbox"),
]


def clone(cases: list[Case]) -> list[Case]:
    return [Case(c.name, c.spec, [Leg(**vars(leg)) for leg in c.legs]) for c in cases]


def damage_every_log(cases: list[Case], mutate) -> list[Case]:
    """Applies one mutation to every leg of every case.

    A criterion that reads the whole matrix cannot be broken by damaging one
    leg of it, and pretending otherwise would report success for a check the
    self-test never made. Some claims are about the matrix, so some damage has
    to be too.
    """
    copy = clone(cases)
    for entry in copy:
        raw = json.loads((Path(entry.spec["directory"]) / "run.json").read_text())
        for one in entry.legs:
            for leg in raw["legs"]:
                if leg["leg"] == one.index:
                    path = ROOT / leg["serial_log"]
                    one.records = parse(mutate(path.read_text(errors="replace")))
    return copy


def damage_log(cases: list[Case], case: str, leg: int, mutate) -> list[Case] | None:
    copy = clone(cases)
    for entry in copy:
        if entry.name != case:
            continue
        for one in entry.legs:
            if one.index != leg:
                continue
            path = None
            for raw in json.loads((Path(entry.spec["directory"]) / "run.json").read_text())["legs"]:
                if raw["leg"] == leg:
                    path = ROOT / raw["serial_log"]
            if path is None:
                return None
            one.records = parse(mutate(path.read_text(errors="replace")))
            return copy
    return None


def damage_medium(cases: list[Case], case: str, leg: int, mutate) -> list[Case] | None:
    copy = clone(cases)
    for entry in copy:
        if entry.name != case:
            continue
        for one in entry.legs:
            if one.index == leg:
                one.medium = mutate(one.medium)
                return copy
    return None


def self_test(cases: list[Case], gates: dict, manifest: dict | None, quiet: bool) -> int:
    by_name = {}
    for check in CRITERIA:
        by_name[check(cases).name] = check

    baseline = [check(cases) for check in CRITERIA]
    if any(not result.passed for result in baseline):
        broken = [r.name for r in baseline if not r.passed]
        print(
            f"self-test needs a passing matrix to damage; this one already fails: {', '.join(broken)}",
            file=sys.stderr,
        )
        return 2

    every = [(name, case, leg, mutate, criterion, "log") for name, case, leg, mutate, criterion in LOG_MUTATIONS]
    every += [(name, case, leg, mutate, criterion, "medium") for name, case, leg, mutate, criterion in MEDIUM_MUTATIONS]
    every += [(name, None, 0, mutate, criterion, "matrix") for name, mutate, criterion in MATRIX_MUTATIONS]

    width = max(len(name) for name, *_ in every)
    missed = []
    for name, case, leg, mutate, criterion, kind in every:
        if kind == "matrix":
            damaged = damage_every_log(cases, mutate)
        else:
            damaged = (damage_log if kind == "log" else damage_medium)(cases, case, leg, mutate)
        check = by_name.get(criterion)
        if damaged is None:
            missed.append((name, criterion, f"no {case}/{leg} to damage"))
            continue
        if check is None:
            missed.append((name, criterion, "no such criterion"))
            continue
        result = check(damaged)
        caught = not result.passed
        if not quiet:
            mark = "CAUGHT" if caught else "MISSED"
            print(f"{mark}  {name.ljust(width)}  -> {criterion}: {result.detail}")
        if not caught:
            missed.append((name, criterion, result.detail))

    print()
    if missed:
        print(f"K4 GATE SELF-TEST FAILED: {len(missed)} of {len(every)} damaged runs still passed")
        for name, criterion, detail in missed:
            print(f"  {name} -> {criterion}: {detail}")
        return 1
    print(f"K4 GATE SELF-TEST PASSED: {len(every)} of {len(every)} damaged runs were caught")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", type=Path, default=ROOT / "build/k4-cases")
    parser.add_argument("--manifest", type=Path, default=ROOT / "build/image-manifest-k4.json")
    parser.add_argument("--k1", type=Path, default=ROOT / "build/k1-gate.json")
    parser.add_argument("--k2", type=Path, default=ROOT / "build/k2-gate.json")
    parser.add_argument("--k3", type=Path, default=ROOT / "build/k3-gate.json")
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    parser.add_argument("--quiet", action="store_true", help="print only the verdict line")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="damage the evidence one way at a time and check that a criterion notices each",
    )
    arguments = parser.parse_args()

    cases = load_cases(arguments.cases)
    if not cases:
        print(
            f"no cases under {arguments.cases}; run tools/run_k4_cases.py",
            file=sys.stderr,
        )
        return 2

    def read(path: Path) -> dict | None:
        return json.loads(path.read_text()) if path.exists() else None

    manifest = read(arguments.manifest)
    gates = {"K1": read(arguments.k1), "K2": read(arguments.k2), "K3": read(arguments.k3)}

    if arguments.self_test:
        return self_test(cases, gates, manifest, arguments.quiet)

    results = [check(cases) for check in CRITERIA]
    results.append(check_regression(gates["K1"], "K1", 13))
    results.append(check_regression(gates["K2"], "K2", 21))
    results.append(check_regression(gates["K3"], "K3", 28))
    results.append(check_reproducible(cases, manifest))

    width = max(len(result.title) for result in results)
    for result in results:
        mark = "PASS" if result.passed else "FAIL"
        if not arguments.quiet:
            print(f"{mark}  {result.title.ljust(width)}  {result.detail}")

    failed = [result for result in results if not result.passed]
    if not arguments.quiet and not failed:
        print()
        print("Evidence:")
        for result in results:
            for line in result.evidence:
                if line:
                    print(f"  [{result.name}] {line}")

    verdict = {
        "gate": "K4",
        "passed": not failed,
        "criteria": [
            {"name": r.name, "title": r.title, "passed": r.passed, "detail": r.detail}
            for r in results
        ],
        "cases": [
            {
                "case": case.name,
                "point": case.spec.get("point"),
                "mode": case.spec.get("mode"),
                "scenario": case.spec.get("scenario"),
                "legs": len(case.legs),
            }
            for case in cases
        ],
        "legs": sum(len(case.legs) for case in cases),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")

    print()
    if failed:
        print(f"K4 GATE FAILED: {len(failed)} of {len(results)} criteria not met")
        return 1
    print(f"K4 GATE PASSED: {len(results)} of {len(results)} criteria met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
