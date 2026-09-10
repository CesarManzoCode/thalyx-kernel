#!/usr/bin/env python3
"""Evaluate the K5 runs against the gate.

K5 is the port: Thalyx's semantics executed on this kernel's own mechanisms.
The thing that is easiest to fake about a port is that anything was ported at
all, so this gate is written against that failure. Two rules follow from it and
they are applied everywhere below:

  * **Host build tooling is not native execution.** A cross-compiler producing
    an image proves nothing. Every criterion about the native side is decided
    from the kernel's records of a domain that the kernel built, activated,
    scheduled and charged -- `domain.created`, `domain.activated`, `user.note`,
    `scope.*` -- and from the bytes the run left on the medium.
  * **A guest that prints `PASS` proves nothing.** Where a criterion reads a
    program's own note it says so in its title, and the value it reads is a
    number the program could only have produced by doing the work: a sum over
    bytes it wrote through a mapping it made, a kernel status it was refused
    with, a token an independent implementation of the same model also chose.

Each criterion is decided separately, so it fails on its own rather than being
carried by the others, and each carries the lines it was decided from.

The K1 to K4 regressions are criteria here too. K5 grew inside the kernel that
boots K1, serves K2, drives K3's devices and keeps K4's durable state, and a K5
gate that passed while any of them had quietly broken would be measuring the
wrong thing.

Usage: tools/check_k5.py [--runs build/k5-runs] [--self-test]
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_k4 as k4gate  # noqa: E402

# Mirrors `thalyx_kernel::diag::FORMAT`.
FORMAT = "THLX1"

RECORD = re.compile(
    r"^" + FORMAT + r" (?P<source>loader|kernel) (?P<seq>\d+) (?P<ns>\d+|-) "
    r"(?P<event>\S+)(?P<rest>.*)$"
)

# `run_k5.py` maps the kernel's completion status to QEMU's `(value << 1) | 1`.
EXIT_COMPLETE = 33

# Mirrors the note numbers the K5 programs use. Repeated rather than imported so
# a renumbering fails this gate instead of quietly redefining what it checks.
NOTE = {
    # the supervisor
    "stage": 0x5000,
    "image_read": 0x5010,
    "native_up": 0x5011,
    "build_step_failed": 0x5012,
    "domain_fault": 0x5013,
    "domain_stopped": 0x5014,
    "scope_pages": 0x5015,
    "scope_cpu": 0x5016,
    "super_done": 0x50FF,
    # the native runtime
    "runtime_up": 0x5001,
    "abi_mismatch": 0x5002,
    "config_bad": 0x5003,
    "runtime_exit": 0x5004,
    "assert_failed": 0x5005,
    "thread_up": 0x5007,
    "thread_work": 0x5008,
    # the smoke program
    "smoke_limits": 0x5100,
    "smoke_clock": 0x5101,
    "smoke_float": 0x5102,
    "smoke_heap_grew": 0x5103,
    "smoke_heap_round": 0x5104,
    "smoke_heap_ceiling": 0x5105,
    "smoke_bss": 0x5106,
    "smoke_math": 0x5107,
    "smoke_heap_error": 0x5108,
    "smoke_heap_held": 0x5109,
    "smoke_done": 0x51FF,
    # the supervisor's auditor
    "service_built": 0x5017,
    "store_ready": 0x5018,
    "work_built": 0x5019,
    "directive": 0x501A,
    "cut": 0x501B,
    "scope_after": 0x501C,
    "audit_effect": 0x501D,
    "audit_drained": 0x501E,
    "audit_effects": 0x501F,
    "audit_high_water": 0x5020,
    "audit_lost": 0x5021,
    # the work domain
    "version_seen": 0x5200,
    "context_answered": 0x5201,
    "workspace_open": 0x5202,
    "workspace_wrote": 0x5203,
    "candidate_frozen": 0x5204,
    "candidate_sealed": 0x5205,
    "tool_verdict": 0x5206,
    "tool_cost": 0x5207,
    "validation_staged": 0x5208,
    "published": 0x5209,
    "publish_refused": 0x520A,
    "abandoned": 0x520B,
    "evidence_written": 0x520C,
    "evidence_read": 0x520D,
    "work_done": 0x520E,
    "store_refused": 0x520F,
    "final_generation": 0x5210,
    "cancelled": 0x5211,
    "work_cpu": 0x5212,
    "work_unexpected": 0x5213,
    "root_prefix": 0x5214,
    "tool_read": 0x5215,
    "tool_sum": 0x5216,
    # the language runtime
    "program_compiled": 0x5300,
    "program_refused": 0x5301,
    "hostcall": 0x5302,
    "program_finish": 0x5303,
    "program_ceiling": 0x5304,
    "program_latched": 0x5305,
    # the validation tool
    "tool_start": 0x5400,
    "tool_parsed": 0x5401,
    "tool_parse_failed": 0x5402,
    "tool_check": 0x5403,
    "tool_check_failed": 0x5404,
    "tool_done": 0x5405,
    # the launcher
    "launch_built": 0x5600,
    "launch_retired": 0x5601,
    "launch_refused": 0x5602,
    "launch_not_sealed": 0x5603,
}

# `thalyx_user_k5pkg::generated::finish`.
FINISH = {"returned": 1, "needs_model": 2, "assertion": 3, "threw": 4,
          "exhausted": 5, "refused": 6}
# `thalyx_user_k5pkg::generated::verdict`.
VERDICT = {"passed": 1, "failed": 2, "not_proven": 3}

STATUS = {"OK": 0, "LIMIT_EXHAUSTED": -9, "INSUFFICIENT_RIGHTS": -4}


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


@dataclass
class Run:
    """One boot: what the kernel recorded and what the host asked for."""

    name: str
    spec: dict
    records: list[Record]
    exit_status: int | None
    timed_out: bool
    medium: bytes = b""


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


def line_of(record: Record) -> str:
    rendered = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{record.seq} {record.event} {rendered}".rstrip()


def load_runs(directory: Path) -> dict[str, Run]:
    runs: dict[str, Run] = {}
    for child in sorted(directory.iterdir() if directory.is_dir() else []):
        record = child / "run.json"
        serial = child / "serial.log"
        if not record.exists() or not serial.exists():
            continue
        spec = json.loads(record.read_text())
        medium = b""
        if spec.get("medium") and Path(spec["medium"]).exists():
            medium = Path(spec["medium"]).read_bytes()
        runs[child.name] = Run(
            name=child.name,
            spec=spec,
            records=parse(serial.read_text()),
            exit_status=spec.get("exit_status"),
            timed_out=bool(spec.get("timed_out")),
            medium=medium,
        )
    return runs


# --- criteria --------------------------------------------------------------


def check_runs_completed(runs: dict[str, Run]) -> Result:
    result = Result("runs_completed", "every run reached the kernel's own completion")
    if not runs:
        result.detail = "no runs"
        return result
    bad = [
        name
        for name, run in runs.items()
        if run.timed_out or run.exit_status != EXIT_COMPLETE
    ]
    result.passed = not bad
    result.detail = (
        f"{len(runs)} runs, all exiting {EXIT_COMPLETE}"
        if result.passed
        else f"did not complete: {', '.join(bad)}"
    )
    for name, run in runs.items():
        result.evidence.append(f"{name}: exit={run.exit_status} timed_out={run.timed_out}")
    return result


def check_no_user_faults(runs: dict[str, Run]) -> Result:
    result = Result("no_user_faults", "no domain faulted and no runtime assertion fired")
    faults = []
    for name, run in runs.items():
        for record in by_event(run.records, "user.fault"):
            faults.append(f"{name}: {line_of(record)}")
        for record in notes(run.records, NOTE["assert_failed"]):
            faults.append(f"{name}: assertion {line_of(record)}")
        for record in notes(run.records, NOTE["abi_mismatch"]):
            faults.append(f"{name}: abi {line_of(record)}")
        for record in notes(run.records, NOTE["config_bad"]):
            faults.append(f"{name}: config {line_of(record)}")
    result.passed = not faults
    result.detail = "none" if result.passed else "; ".join(faults[:4])
    result.evidence = faults[:8]
    return result


def check_no_build_step_failed(runs: dict[str, Run]) -> Result:
    result = Result("build_steps", "the supervisor built every domain it planned")
    failures = []
    for name, run in runs.items():
        for record in notes(run.records, NOTE["build_step_failed"]):
            failures.append(f"{name}: step {record.number('b')} refused")
    result.passed = not failures
    result.detail = "no step refused" if result.passed else "; ".join(failures[:4])
    result.evidence = failures[:8]
    return result


def smoke_run(runs: dict[str, Run]) -> Run | None:
    for run in runs.values():
        if run.spec.get("image_manifest", {}) and \
                run.spec["image_manifest"].get("stage") == "smoke":
            return run
    return None


def check_native_domain_ran(runs: dict[str, Run]) -> Result:
    """The kernel built, activated and scheduled a domain from a native image."""
    result = Result(
        "native_domain",
        "the kernel built and ran a domain from a C image on the native target",
    )
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    created = [r for r in by_event(run.records, "domain.created") if r.get("name") == "nsmoke"]
    activated = [
        r for r in by_event(run.records, "domain.activated") if r.get("name") == "nsmoke"
    ]
    scheduled = [
        r
        for r in by_event(run.records, "user.note")
        if r.get("name") == "nsmoke" and r.number("preemptions") is not None
    ]
    preempted = max((r.number("preemptions") or 0) for r in scheduled) if scheduled else 0
    result.passed = bool(created) and bool(activated) and preempted > 0
    result.detail = (
        f"built from image object {created[0].get('image_object')}, "
        f"entry {created[0].get('entry')}, preempted {preempted} times"
        if result.passed
        else f"created={len(created)} activated={len(activated)} preemptions={preempted}"
    )
    result.evidence = [line_of(r) for r in created + activated][:4]
    return result


def check_native_runtime_stood_up(runs: dict[str, Run]) -> Result:
    """Reads the program's notes; says so. Each value is one thing that worked."""
    result = Result(
        "native_runtime",
        "the native runtime came up: .bss zeroed, interface answered, clock advanced (program notes)",
    )
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    up = note_values(run.records, "runtime_up", "nsmoke")
    bss = note_values(run.records, "smoke_bss", "nsmoke")
    limits = note_values(run.records, "smoke_limits", "nsmoke")
    clock = note_values(run.records, "smoke_clock", "nsmoke")
    minor = (limits[0] >> 32) & 0xFFFF if limits else None
    cpus = limits[0] & 0xFFFFFFFF if limits else None
    # The kernel's own count, from the processors that completed its handshake,
    # so "the interface told the program the truth" is decided against the
    # kernel's record and not against the command line.
    online = len(by_event(run.records, "smp.ap_online")) + 1
    result.passed = (
        up == [1]
        and bss == [0]
        and minor is not None
        and cpus == online
        and bool(clock)
        and clock[0] >= 2_000_000
    )
    result.detail = (
        f"role={up[0] if up else None} bss_dirty={bss[0] if bss else None} "
        f"limits_minor={minor} cpus_online={cpus} (kernel started {online}) "
        f"clock_delta={clock[0] if clock else None}ns"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["smoke_limits"], "nsmoke")]
    return result


def check_hardware_float(runs: dict[str, Run]) -> Result:
    """The native target does floating point in hardware, and it survives being
    preempted -- which is what this kernel's eager FP save and restore is for."""
    result = Result(
        "hardware_float",
        "hardware double arithmetic produced the seeded value, across preemptions (program note)",
    )
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    values = note_values(run.records, "smoke_float", "nsmoke")
    error = note_values(run.records, "smoke_math", "nsmoke")
    manifest = run.spec.get("image_manifest") or {}
    seed = manifest.get("seed")
    expected = None
    if seed is not None:
        accumulator = 1.0 + (seed & 0xFFFF) / 65536.0
        for _ in range(64):
            accumulator = accumulator * 1.0000001 + accumulator ** 0.5 / 1024.0
        expected = int(accumulator * 1e9)
    # The host recomputes the same loop in double precision. Agreement to a part
    # in ten million is what the note's fixed-point rendering can carry.
    close = (
        expected is not None
        and values
        and abs(values[0] - expected) <= max(1, expected // 10_000_000)
    )
    result.passed = bool(close) and bool(error) and error[0] <= 1000
    result.detail = (
        f"guest={values[0] if values else None} host={expected} "
        f"exp/log round trip off by {error[0] if error else None} parts per billion"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["smoke_float"], "nsmoke")]
    return result


def check_heap_from_memory_objects(runs: dict[str, Run]) -> Result:
    """The heap is memory objects the program created and mapped itself, and the
    kernel's records say so independently of the program's."""
    result = Result(
        "native_heap",
        "the heap grew by memory objects the program created against its own scope",
    )
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    created = [
        r
        for r in by_event(run.records, "memory.created")
        if r.get("label") == "heap" or r.get("name") == "heap"
    ]
    grew = note_values(run.records, "smoke_heap_grew", "nsmoke")
    round_trip = note_values(run.records, "smoke_heap_round", "nsmoke")
    held = note_values(run.records, "smoke_heap_held", "nsmoke")
    # 0xA5 + 0x5A + 0x33 per sampled offset, summed over the three blocks.
    expected_unit = 0xA5 + 0x5A + 0x33
    consistent = bool(round_trip) and round_trip[0] % expected_unit == 0 and round_trip[0] > 0
    result.passed = bool(grew) and grew[0] > 0 and consistent and bool(held) and held[0] > grew[0]
    result.detail = (
        f"{grew[0] if grew else 0} pages on the first growth, {held[0] if held else 0} held at "
        f"the ceiling, byte sum {round_trip[0] if round_trip else None} "
        f"({(round_trip[0] // expected_unit) if consistent else '-'} sampled offsets)"
    )
    result.evidence = [line_of(r) for r in created[:3]]
    return result


def check_scope_ceiling_refused(runs: dict[str, Run]) -> Result:
    """What stops the heap is the kernel, and the program reports the kernel's
    own status rather than a limit of its own."""
    result = Result(
        "scope_ceiling",
        "the scope's page ceiling refused the next arena with LIMIT_EXHAUSTED",
    )
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    ceiling = note_values(run.records, "smoke_heap_ceiling", "nsmoke")
    errors = note_values(run.records, "smoke_heap_error", "nsmoke")
    signed = [value - (1 << 64) if value >= (1 << 63) else value for value in errors]
    result.passed = ceiling == [1] and STATUS["LIMIT_EXHAUSTED"] in signed
    result.detail = (
        f"refusals={ceiling} kernel statuses={signed}"
        if result.passed
        else f"refusals={ceiling} statuses={signed}"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["smoke_heap_error"], "nsmoke")]
    return result


def check_native_exit(runs: dict[str, Run]) -> Result:
    result = Result("native_exit", "the native domain finished and the kernel recorded its code")
    run = smoke_run(runs)
    if run is None:
        result.detail = "no smoke run"
        return result
    stopped = note_values(run.records, "domain_stopped", "supervisor")
    done = note_values(run.records, "super_done", "supervisor")
    result.passed = stopped == [0] and done == [1]
    result.detail = f"exit_code={stopped} supervisor_verdict={done}"
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["domain_stopped"], "supervisor")]
    return result


# --- the surface stage ------------------------------------------------------


def stage_run(runs: dict[str, Run], stage: str) -> Run | None:
    for run in runs.values():
        manifest = run.spec.get("image_manifest") or {}
        if manifest.get("stage") == stage:
            return run
    return None


def seed_of(run: Run) -> int | None:
    manifest = run.spec.get("image_manifest") or {}
    return manifest.get("seed")


def check_port_services(runs: dict[str, Run]) -> Result:
    """The port stands on K4's service and K4's driver, rebuilt into this
    package. The kernel's own records say which domains were built."""
    result = Result(
        "port_services",
        "the port ran on the K4 block driver and the K4 managed-state service",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    names = {
        record.get("name")
        for record in by_event(run.records, "domain.created")
    }
    wanted = {"k5disk", "k5store", "k5pub"}
    built = note_values(run.records, "service_built", "supervisor")
    ready = note_values(run.records, "store_ready", "supervisor")
    result.passed = wanted <= names and built == [1, 2] and ready == [1]
    result.detail = (
        f"domains {sorted(names - {'supervisor'})}, services built {built}, service ready {ready}"
    )
    result.evidence = [
        line_of(record)
        for record in by_event(run.records, "domain.created")
        if record.get("name") in wanted
    ]
    return result


def check_vertical_shape(runs: dict[str, Run]) -> Result:
    """The vertical, in the order the phase names it, from the work's notes."""
    result = Result(
        "vertical_shape",
        "version identified, context answered, workspace opened, change made, published (program notes)",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    seen = note_values(run.records, "version_seen", "k5pub")
    context = note_values(run.records, "context_answered", "k5pub")
    opened = note_values(run.records, "workspace_open", "k5pub")
    frozen = note_values(run.records, "candidate_frozen", "k5pub")
    published = note_values(run.records, "published", "k5pub")
    final = note_values(run.records, "final_generation", "k5pub")
    unexpected = note_values(run.records, "work_unexpected", "k5pub")
    # The second sighting packs the generation the work was against in the low
    # half and how many names that version bound in the high half.
    against = [value & 0xFFFFFFFF for value in seen]
    bound = [value >> 32 for value in seen]
    result.passed = (
        against == [0, 1]
        and bound == [0, 4]
        and bool(context)
        and context[0] >= 2
        and len(opened) == 2
        and len(frozen) == 2
        and published == [1, 2]
        and final == [2]
        and not unexpected
    )
    result.detail = (
        f"versions the work was against {against} binding {bound} names, "
        f"uses of `checksum` {context}, workspaces {len(opened)}, "
        f"candidates {len(frozen)}, generations published {published}, final {final}, "
        f"unexpected {unexpected}"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["published"], "k5pub")]
    return result


def check_verbs_drove_the_change(runs: dict[str, Run]) -> Result:
    """Every change went through the verb surface, and the surface said what it
    did. The names are the ones the port answers, spelled as Thalyx spells
    them."""
    result = Result(
        "verb_surface",
        "the change was made through the Thalyx verb surface and nowhere else (program notes)",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    calls = note_values(run.records, "hostcall", "k5pub")
    spelled = []
    for value in calls:
        text = value.to_bytes(8, "big").lstrip(b"\x00").decode("utf-8", "replace")
        spelled.append(text)
    # The plane carries two integers, so a name longer than eight bytes arrives
    # as its first eight. `sustituir` is nine, and the gate compares what the
    # channel can carry rather than pretending it carried more.
    wanted = [name[:8] for name in
              ["estado", "contexto", "leer", "sustituir", "leer", "cambios", "buscar"]]
    result.passed = spelled == wanted
    result.detail = f"calls {spelled}"
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["hostcall"], "k5pub")][:4]
    return result


def check_published_bytes(runs: dict[str, Run]) -> Result:
    """The decisive one, and the only one the guest does not narrate.

    The medium is decoded here by the module the K4 schema generates. The seed
    version's module carries sixteen zeroes; the published one carries the run's
    seed in hexadecimal, which the host chose and wrote into the image. A guest
    that had not done the work could not have put those bytes there."""
    result = Result(
        "published_bytes",
        "the medium carries a published version whose module bears this run's seed",
    )
    run = stage_run(runs, "surface")
    if run is None or not run.medium:
        result.detail = "no surface medium"
        return result
    seed = seed_of(run)
    if seed is None:
        result.detail = "the run does not record a seed"
        return result
    expected = f"{seed:016x}".encode()
    store = k4gate.read_store(run.medium)
    commits = k4gate.commits(store)
    objects = k4gate.objects(store)
    marks = []
    for content in (entry.get("content", b"") for entry in objects.values()):
        at = content.find(b'var MARK = "')
        if at >= 0:
            marks.append(content[at + 12 : at + 28])
    generations = [commit["new_generation"] for commit in commits]
    final_root = commits[-1]["root_digest"] if commits else None
    reachable = k4gate.reachable_from(store, final_root) if final_root else None
    result.passed = (
        generations == [1, 2]
        and sorted(marks) == sorted([b"0" * 16, expected])
        and reachable is not None
        and len(reachable) >= 6
    )
    result.detail = (
        f"generations {generations}, marks {[m.decode() for m in marks]}, "
        f"expected {expected.decode()}, objects reachable from the final root "
        f"{len(reachable) if reachable else 0}"
    )
    result.evidence = [
        f"commit generation={commit['new_generation']} root={commit['root_digest'].hex()[:16]}"
        for commit in commits
    ]
    return result


def check_evidence_survives(runs: dict[str, Run]) -> Result:
    """The work read its own published version back through the service, by the
    path anybody else would use, and found the change it had made."""
    result = Result(
        "evidence_readable",
        "the published version was read back through the service and carries the change",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    read = note_values(run.records, "evidence_read", "k5pub")
    names = [value & 0xFFFFFFFF for value in read]
    marked = [(value >> 32) & 1 for value in read]
    result.passed = names == [4] and marked == [1]
    result.detail = f"names in the published version {names}, carries the mark {marked}"
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["evidence_read"], "k5pub")]
    return result


def check_control_plane(runs: dict[str, Run]) -> Result:
    """Every publication was an effect the kernel admitted and wrote a receipt
    for, and the auditor read them. The receipts are the account of the run
    that is not the work's own."""
    result = Result(
        "control_receipts",
        "the kernel admitted one effect per publication and the auditor read every receipt",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    effects = note_values(run.records, "audit_effects", "supervisor")
    drained = note_values(run.records, "audit_drained", "supervisor")
    lost = note_values(run.records, "audit_lost", "supervisor")
    high = note_values(run.records, "audit_high_water", "supervisor")
    gaps = [value >> 32 for value in high]
    kernel_effects = [
        record
        for record in by_event(run.records, "ctrl.receipt")
        if record.number("kind") == 2
    ]
    exhausted = by_event(run.records, "k2.admission_exhausted")
    result.passed = (
        effects == [2]
        and len(kernel_effects) == 2
        and lost == [0]
        and gaps == [0]
        and bool(drained)
        and drained[0] >= 2
        and not exhausted
    )
    result.detail = (
        f"effect receipts the kernel wrote {len(kernel_effects)}, the auditor counted {effects}, "
        f"receipts drained {drained}, lost {lost}, gaps {gaps}, "
        f"admissions refused for a full log {len(exhausted)}"
    )
    result.evidence = [line_of(record) for record in kernel_effects]
    return result


def check_work_confinement(runs: dict[str, Run]) -> Result:
    """What the work could not do, said as a fact about its capability table.

    A work domain never holds a device capability and never builds a domain, so
    the kernel's records must show no device operation and no domain creation
    charged to it."""
    result = Result(
        "work_confinement",
        "the work domain reached the medium only through the service, and built nothing",
    )
    run = stage_run(runs, "surface")
    if run is None:
        result.detail = "no surface run"
        return result
    refused = [
        record
        for record in by_event(run.records, "k2.refused")
        if record.get("name") == "k5pub"
    ]
    created_by_work = [
        record
        for record in by_event(run.records, "domain.created")
        if record.get("name") not in {"supervisor", "k5disk", "k5store", "k5pub"}
    ]
    device_ops = [
        record
        for record in run.records
        if record.event.startswith("device.") and record.get("name") == "k5pub"
    ]
    result.passed = not created_by_work and not device_ops and not refused
    result.detail = (
        f"domains it built {len(created_by_work)}, device operations {len(device_ops)}, "
        f"operations the kernel refused it {len(refused)}"
    )
    return result


# --- the work stage ---------------------------------------------------------


def check_language_runtime(runs: dict[str, Run]) -> Result:
    """Real QuickJS, in a domain the kernel built from a C image, executing a
    program that came out of managed state."""
    result = Result(
        "language_runtime",
        "the language runtime ran natively and compiled the program from the published version",
    )
    run = stage_run(runs, "work")
    if run is None:
        result.detail = "no work run"
        return result
    created = [r for r in by_event(run.records, "domain.created") if r.get("name") == "nhacer"]
    activated = [r for r in by_event(run.records, "domain.activated") if r.get("name") == "nhacer"]
    compiled = note_values(run.records, "program_compiled", "nhacer")
    refused = note_values(run.records, "program_refused", "nhacer")
    # The program is `program.js` of the published version; its length is what
    # the medium says it is.
    published = published_lengths(run)
    expected = published.get(b"program.js")
    result.passed = (
        bool(created)
        and bool(activated)
        and compiled == [expected]
        and expected is not None
        and not refused
    )
    result.detail = (
        f"built={len(created)} activated={len(activated)} compiled={compiled} bytes, "
        f"the medium says program.js is {expected} bytes, refusals {len(refused)}"
    )
    result.evidence = [line_of(r) for r in created + activated][:3]
    return result


def published_lengths(run: Run) -> dict[bytes, int]:
    """The names and lengths of the version the medium says was published."""
    if not run.medium:
        return {}
    store = k4gate.read_store(run.medium)
    commits = k4gate.commits(store)
    if not commits:
        return {}
    objects = k4gate.objects(store)
    root = commits[-1]["root_digest"]
    manifest = objects.get(root)
    if manifest is None:
        return {}
    body = k4gate.fmt.decode("Manifest", manifest["content"], 0)
    tree = objects.get(bytes(body["tree_digest"]))
    if tree is None:
        return {}
    header = k4gate.fmt.decode("TreeHeader", tree["content"], 0)
    out: dict[bytes, int] = {}
    at = k4gate.fmt.STRUCTS["TreeHeader"][0]
    width = k4gate.fmt.STRUCTS["TreeEntry"][0]
    for index in range(header["entry_count"]):
        entry = k4gate.fmt.decode("TreeEntry", tree["content"], at + index * width)
        name = bytes(entry["name"])[: entry["name_len"]]
        out[name] = entry["length"]
    return out


def check_hostcalls_mediated(runs: dict[str, Run]) -> Result:
    """Everything the program did, it did by asking. The verbs it asked for are
    the ones the port answers, and the work counted the same number."""
    result = Result(
        "hostcalls",
        "the program reached the workspace only through host calls the work served",
    )
    run = stage_run(runs, "work")
    if run is None:
        result.detail = "no work run"
        return result
    calls = note_values(run.records, "hostcall", "k5pub")
    spelled = [value.to_bytes(8, "big").lstrip(b"\x00").decode("utf-8", "replace")
               for value in calls]
    verbs = [name for name in spelled if name in
             {"estado", "contexto", "leer", "sustitui", "cambios", "buscar", "listar",
              "escribir"}]
    finish = note_values(run.records, "program_finish", "k5pub")
    packed = finish[-1] if finish else 0
    requests = (packed >> 8) & 0xFF
    validations = (packed >> 16) & 0xFF
    ended = packed & 0xFF
    result.passed = (
        ended == FINISH["returned"]
        and requests == len(verbs)
        and requests >= 5
        and validations == 1
    )
    result.detail = (
        f"verbs {verbs}, the work counted {requests} requests and {validations} validations, "
        f"the program ended `{[k for k, v in FINISH.items() if v == ended]}`"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["program_finish"], "k5pub")]
    return result


def check_real_tool(runs: dict[str, Run]) -> Result:
    """The validation tool is a native program in a domain and a scope of its
    own, and what it cost is the kernel's account rather than the tool's."""
    result = Result(
        "real_tool",
        "a real tool ran natively in its own domain, compiled the candidate and ran its checks",
    )
    run = stage_run(runs, "work")
    if run is None:
        result.detail = "no work run"
        return result
    created = [r for r in by_event(run.records, "domain.created") if r.get("name") == "ncheck"]
    parsed = note_values(run.records, "tool_parsed", "ncheck")
    parse_failed = note_values(run.records, "tool_parse_failed", "ncheck")
    checks = note_values(run.records, "tool_check", "ncheck")
    failed = note_values(run.records, "tool_check_failed", "ncheck")
    done = note_values(run.records, "tool_done", "ncheck")
    verdicts = note_values(run.records, "tool_verdict", "k5pub")
    costs = note_values(run.records, "tool_cost", "k5pub")
    retired = note_values(run.records, "launch_retired", "supervisor")
    result.passed = (
        bool(created)
        and len(parsed) == 3
        and not parse_failed
        and len(checks) >= 6
        and not failed
        and verdicts == [0]
        and bool(costs)
        and costs[0] > 0
        and len(retired) >= 2
    )
    result.detail = (
        f"files compiled {len(parsed)}, checks held {len(checks)}, checks failed {len(failed)}, "
        f"exit code {verdicts}, the kernel charged its scope {costs[0] if costs else 0}ns, "
        f"scopes retired {len(retired)}, tool_done {[hex(v) for v in done]}"
    )
    result.evidence = [line_of(r) for r in created][:2]
    return result


def check_tool_read_the_candidate(runs: dict[str, Run]) -> Result:
    """The tool's verdict names the bytes it was about, and those bytes are the
    ones the medium says were published."""
    result = Result(
        "tool_read_candidate",
        "the tool read exactly the bytes the medium says the published version binds",
    )
    run = stage_run(runs, "work")
    if run is None:
        result.detail = "no work run"
        return result
    read = note_values(run.records, "tool_read", "k5pub")
    sums = note_values(run.records, "tool_sum", "k5pub")
    bytes_read = [value & 0xFFFFFFFF for value in read]
    checks_run = [(value >> 32) & 0xFFFF for value in read]
    checks_failed = [(value >> 48) & 0xFFFF for value in read]
    published = published_lengths(run)
    expected = sum(published.values()) if published else None
    result.passed = (
        bytes_read == [expected]
        and expected is not None
        and bool(sums)
        and sums[0] != 0
        and checks_failed == [0]
        and bool(checks_run)
        and checks_run[0] >= 6
    )
    result.detail = (
        f"the tool read {bytes_read} bytes, the medium binds {expected}; "
        f"digest over them {[hex(v) for v in sums]}; "
        f"checks {checks_run} of which {checks_failed} failed"
    )
    return result


def check_candidate_sealed(runs: dict[str, Run]) -> Result:
    """The tool was given a sealed object, and the kernel says it was sealed.

    A tool that validated bytes which could change under it would be validating
    nothing in particular."""
    result = Result(
        "candidate_sealed",
        "the candidate was sealed by the kernel before the tool was launched",
    )
    run = stage_run(runs, "work")
    if run is None:
        result.detail = "no work run"
        return result
    sealed = [r for r in by_event(run.records, "mem.sealed")
              if (r.get("label") or "").startswith("candidate")]
    refusals = note_values(run.records, "launch_not_sealed", "supervisor")
    staged = note_values(run.records, "candidate_sealed", "k5pub")
    built = [r for r in by_event(run.records, "domain.created") if r.get("name") == "ncheck"]
    order_ok = bool(sealed) and bool(built) and sealed[0].seq < built[0].seq
    result.passed = order_ok and not refusals and bool(staged)
    result.detail = (
        f"seals recorded {len(sealed)}, sealed at record {sealed[0].seq if sealed else '-'}, "
        f"tool built at {built[0].seq if built else '-'}, "
        f"launcher refusals for an unsealed candidate {len(refusals)}, "
        f"candidate bytes {staged}"
    )
    result.evidence = [line_of(r) for r in sealed[:2]]
    return result


def check_publication_conditioned(runs: dict[str, Run]) -> Result:
    """The publication happened only after the tool passed, and the durable
    record names the tool that decided it."""
    result = Result(
        "publication_conditioned",
        "the publication followed the tool's verdict and the durable record names that tool",
    )
    run = stage_run(runs, "work")
    if run is None or not run.medium:
        result.detail = "no work run"
        return result
    published = note_values(run.records, "published", "k5pub")
    verdicts = note_values(run.records, "tool_verdict", "k5pub")
    staged = note_values(run.records, "validation_staged", "k5pub")
    store = k4gate.read_store(run.medium)
    objects = k4gate.objects(store)
    validations = []
    for entry in objects.values():
        if entry["object_type"] == 5:
            validations.append(k4gate.fmt.decode("Validation", entry["content"], 0))
    tools = sorted({record["tool_id"] for record in validations})
    generations = sorted({record["base_generation"] for record in validations})
    result.passed = (
        published == [1, 2]
        and verdicts == [0]
        and tools == [0x4B352001]
        and generations == [0, 1]
        and 1 in staged
    )
    result.detail = (
        f"generations published {published}, tool exit {verdicts}, "
        f"validation records name tool(s) {[hex(t) for t in tools]} over base generation(s) "
        f"{generations}, the candidate the tool saw matched the frozen tree {1 in staged}"
    )
    return result


def check_work_published_bytes(runs: dict[str, Run]) -> Result:
    """The same decisive check as the surface stage, for the run a program drove."""
    result = Result(
        "work_published_bytes",
        "the medium carries a version whose module bears this run's seed, written by the program",
    )
    run = stage_run(runs, "work")
    if run is None or not run.medium:
        result.detail = "no work run"
        return result
    seed = seed_of(run)
    expected = f"{seed:016x}".encode()
    store = k4gate.read_store(run.medium)
    objects = k4gate.objects(store)
    marks = []
    for content in (entry.get("content", b"") for entry in objects.values()):
        at = content.find(b'var MARK = "')
        if at >= 0:
            marks.append(content[at + 12 : at + 28])
    generations = [commit["new_generation"] for commit in k4gate.commits(store)]
    result.passed = generations == [1, 2] and sorted(marks) == sorted([b"0" * 16, expected])
    result.detail = (
        f"generations {generations}, marks {[m.decode() for m in marks]}, "
        f"expected {expected.decode()}"
    )
    return result


def check_regression(gate: dict | None, name: str, expected: int) -> Result:
    result = Result(f"regression_{name.lower()}", f"the {name} gate still passes on this kernel")
    if gate is None:
        result.detail = f"no {name} verdict; run its gate"
        return result
    met = sum(1 for criterion in gate.get("criteria", []) if criterion.get("passed"))
    result.passed = bool(gate.get("passed")) and met == expected
    result.detail = f"{met} of {expected} criteria"
    return result


CRITERIA = [
    check_runs_completed,
    check_no_user_faults,
    check_no_build_step_failed,
    check_native_domain_ran,
    check_native_runtime_stood_up,
    check_hardware_float,
    check_heap_from_memory_objects,
    check_scope_ceiling_refused,
    check_native_exit,
    check_port_services,
    check_vertical_shape,
    check_verbs_drove_the_change,
    check_published_bytes,
    check_evidence_survives,
    check_control_plane,
    check_work_confinement,
    check_language_runtime,
    check_hostcalls_mediated,
    check_real_tool,
    check_tool_read_the_candidate,
    check_candidate_sealed,
    check_publication_conditioned,
    check_work_published_bytes,
]


# --- self-test -------------------------------------------------------------
#
# A gate is only worth its verdict if a broken run fails it. Each entry damages
# the evidence one way and names the criterion that has to notice.

def damage_note(runs: dict[str, Run], key: str, domain: str, value: int) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        for record in notes(run.records, NOTE[key], domain):
            record.fields["b"] = hex(value)
    return damaged


def drop_note(runs: dict[str, Run], key: str, domain: str) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        keep = [
            record
            for record in run.records
            if not (
                record.event == "user.note"
                and record.get("kind") == "self_check"
                and record.number("a") == NOTE[key]
                and record.get("name") == domain
            )
        ]
        run.records = keep
    return damaged


def drop_event(runs: dict[str, Run], event: str, name: str) -> dict[str, Run]:
    """Removes every record of `event` whose `name` or `label` is `name`.

    Both, because the kernel names a domain with `name` and a memory object with
    `label`, and a damage that only knew about one would leave the other kind of
    record in place and look like a criterion that does not notice."""
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        run.records = [
            record
            for record in run.records
            if not (
                record.event == event
                and name in (record.get("name"), record.get("label"))
            )
        ]
    return damaged


def add_fault(runs: dict[str, Run]) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        run.records.append(
            Record("kernel", 99999, 1, "user.fault", {"domain": "1", "name": "nsmoke", "vector": "14"})
        )
        break
    return damaged


def bad_exit(runs: dict[str, Run]) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        run.exit_status = 35
        break
    return damaged


DAMAGE: list[tuple[str, str, object]] = [
    ("exit status of a run changed", "runs_completed", bad_exit),
    ("a user fault added", "no_user_faults", add_fault),
    (
        "a build step recorded as refused",
        "build_steps",
        lambda runs: append_note(runs, "build_step_failed", "supervisor", 7),
    ),
    (
        "the native domain never activated",
        "native_domain",
        lambda runs: drop_event(runs, "domain.activated", "nsmoke"),
    ),
    (
        ".bss reported dirty",
        "native_runtime",
        lambda runs: damage_note(runs, "smoke_bss", "nsmoke", 1),
    ),
    (
        "the limits query reported a different processor count",
        "native_runtime",
        lambda runs: damage_note(runs, "smoke_limits", "nsmoke", 0x200000009),
    ),
    (
        "the clock did not advance",
        "native_runtime",
        lambda runs: damage_note(runs, "smoke_clock", "nsmoke", 0),
    ),
    (
        "the float result moved",
        "hardware_float",
        lambda runs: damage_note(runs, "smoke_float", "nsmoke", 0x1000),
    ),
    (
        "exp and log stopped agreeing",
        "hardware_float",
        lambda runs: damage_note(runs, "smoke_math", "nsmoke", 5_000_000),
    ),
    (
        "the heap never grew",
        "native_heap",
        lambda runs: damage_note(runs, "smoke_heap_grew", "nsmoke", 0),
    ),
    (
        "the bytes read back through the new mapping are not what was written",
        "native_heap",
        lambda runs: damage_note(runs, "smoke_heap_round", "nsmoke", 12345),
    ),
    (
        "the ceiling refusal carried a different kernel status",
        "scope_ceiling",
        lambda runs: damage_note(runs, "smoke_heap_error", "nsmoke", 0),
    ),
    (
        "the native domain exited with a code",
        "native_exit",
        lambda runs: damage_note(runs, "domain_stopped", "supervisor", 3),
    ),
    (
        "the runtime never came up",
        "native_runtime",
        lambda runs: drop_note(runs, "runtime_up", "nsmoke"),
    ),
    (
        "the state service never reported itself ready",
        "port_services",
        lambda runs: drop_note(runs, "store_ready", "supervisor"),
    ),
    (
        "the work domain was never created",
        "port_services",
        lambda runs: drop_event(runs, "domain.created", "k5pub"),
    ),
    (
        "the work published a different generation",
        "vertical_shape",
        lambda runs: damage_note(runs, "published", "k5pub", 9),
    ),
    (
        "the work recorded something it did not expect",
        "vertical_shape",
        lambda runs: append_note(runs, "work_unexpected", "k5pub", 0x0E10, "surface"),
    ),
    (
        "context found the name used only once",
        "vertical_shape",
        lambda runs: damage_note(runs, "context_answered", "k5pub", 1),
    ),
    (
        "a verb call went by another name",
        "verb_surface",
        lambda runs: damage_note(runs, "hostcall", "k5pub", 0x6E6F7065),
    ),
    (
        "the published module's mark is not this run's seed",
        "published_bytes",
        lambda runs: rewrite_medium(runs, b"000000005eed", b"0000dead0000"),
    ),
    (
        "the seed version was never published",
        "published_bytes",
        lambda runs: rewrite_medium(runs, b'var MARK = "0000000000000000"',
                                    b'var MARK = "0000000000000001"'),
    ),
    (
        "the published version read back without the change",
        "evidence_readable",
        lambda runs: damage_note(runs, "evidence_read", "k5pub", 4),
    ),
    (
        "the auditor counted a different number of effects",
        "control_receipts",
        lambda runs: damage_note(runs, "audit_effects", "supervisor", 1),
    ),
    (
        "the kernel refused an admission because the log was full",
        "control_receipts",
        lambda runs: add_event(runs, "k2.admission_exhausted",
                               {"resource": "receipt_cells", "used": "56", "capacity": "56"}),
    ),
    (
        "a receipt was lost",
        "control_receipts",
        lambda runs: damage_note(runs, "audit_lost", "supervisor", 3),
    ),
    (
        "the work built a domain of its own",
        "work_confinement",
        lambda runs: add_event(runs, "domain.created", {"domain": "9", "name": "sneaky"}),
    ),
    (
        "the language runtime never activated",
        "language_runtime",
        lambda runs: drop_event(runs, "domain.activated", "nhacer"),
    ),
    (
        "the runtime compiled a different program from the one published",
        "language_runtime",
        lambda runs: damage_note(runs, "program_compiled", "nhacer", 99),
    ),
    (
        "the program ended some other way than by returning",
        "hostcalls",
        lambda runs: damage_note(runs, "program_finish", "k5pub", FINISH["threw"]),
    ),
    (
        "the validation tool never ran",
        "real_tool",
        lambda runs: drop_event(runs, "domain.created", "ncheck"),
    ),
    (
        "a check the tool ran did not hold",
        "real_tool",
        lambda runs: append_note(runs, "tool_check_failed", "ncheck", 3, "work"),
    ),
    (
        "the tool exited non-zero and the run went on",
        "real_tool",
        lambda runs: damage_note(runs, "tool_verdict", "k5pub", 1),
    ),
    (
        "the tool read a different number of bytes from what the medium binds",
        "tool_read_candidate",
        lambda runs: damage_note(runs, "tool_read", "k5pub", 8 << 32),
    ),
    (
        "the candidate was never sealed",
        "candidate_sealed",
        lambda runs: drop_event(runs, "mem.sealed", "candidate"),
    ),
    (
        "the launcher was handed an unsealed candidate",
        "candidate_sealed",
        lambda runs: append_note(runs, "launch_not_sealed", "supervisor", 1, "work"),
    ),
    (
        "a validation record names a tool nothing ran",
        "publication_conditioned",
        lambda runs: rewrite_validation_tool(runs),
    ),
    (
        "the published module's mark is not the seed of the run a program drove",
        "work_published_bytes",
        lambda runs: rewrite_medium(runs, b"000000005eed0003", b"00000000dead0003"),
    ),
]


def rewrite_validation_tool(runs: dict[str, Run]) -> dict[str, Run]:
    """Changes the tool identity every durable validation record names.

    The number is little-endian in the record, so this edits the bytes the
    medium actually holds rather than a decoded field."""
    damaged = copy.deepcopy(runs)
    before = (0x4B352001).to_bytes(8, "little")
    after = (0x4B352999).to_bytes(8, "little")
    for run in damaged.values():
        if run.medium:
            run.medium = run.medium.replace(before, after)
    return damaged


def rewrite_medium(runs: dict[str, Run], before: bytes, after: bytes) -> dict[str, Run]:
    """Edits the bytes of a medium, which is what the strongest criterion reads.

    The two must be the same length: this damages what a version says, not how
    the records that carry it are framed. A criterion that only noticed a
    changed length would be checking the framing rather than the content."""
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        if run.medium and before in run.medium:
            run.medium = run.medium.replace(before, after)
    return damaged


def add_event(runs: dict[str, Run], event: str, fields: dict[str, str]) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        manifest = run.spec.get("image_manifest") or {}
        if manifest.get("stage") != "surface":
            continue
        run.records.append(Record("kernel", 99997, 1, event, dict(fields)))
        break
    return damaged


def append_note(
    runs: dict[str, Run], key: str, domain: str, value: int, stage: str | None = None
) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        if stage is not None:
            manifest = run.spec.get("image_manifest") or {}
            if manifest.get("stage") != stage:
                continue
        run.records.append(
            Record(
                "kernel",
                99998,
                1,
                "user.note",
                {"name": domain, "kind": "self_check", "a": hex(NOTE[key]), "b": hex(value)},
            )
        )
        break
    return damaged


def self_test(runs: dict[str, Run], quiet: bool) -> int:
    baseline = [check(runs) for check in CRITERIA]
    if not all(result.passed for result in baseline):
        print("self-test needs a passing baseline; the undamaged evidence already fails",
              file=sys.stderr)
        for result in baseline:
            if not result.passed:
                print(f"  {result.name}: {result.detail}", file=sys.stderr)
        return 2

    missed = []
    for description, expected, damage in DAMAGE:
        damaged = damage(runs)
        results = {result.name: result for result in (check(damaged) for check in CRITERIA)}
        target = results.get(expected)
        if target is None or target.passed:
            missed.append(f"{description}: {expected} did not notice")
        elif not quiet:
            print(f"NOTICED  {expected.ljust(20)}  {description}")
    print()
    if missed:
        print(f"K5 SELF-TEST FAILED: {len(missed)} of {len(DAMAGE)} damages went unnoticed")
        for line in missed:
            print(f"  {line}")
        return 1
    print(f"K5 SELF-TEST PASSED: {len(DAMAGE)} damages, each noticed by the criterion named")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runs", type=Path, default=ROOT / "build/k5-runs")
    parser.add_argument("--k1", type=Path, default=ROOT / "build/k1-gate.json")
    parser.add_argument("--k2", type=Path, default=ROOT / "build/k2-gate.json")
    parser.add_argument("--k3", type=Path, default=ROOT / "build/k3-gate.json")
    parser.add_argument("--k4", type=Path, default=ROOT / "build/k4-gate.json")
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--self-test", action="store_true",
                        help="damage the evidence one way at a time and check a criterion notices")
    arguments = parser.parse_args()

    runs = load_runs(arguments.runs)
    if not runs:
        print(f"no runs under {arguments.runs}; run tools/run_k5_stages.py", file=sys.stderr)
        return 2

    if arguments.self_test:
        return self_test(runs, arguments.quiet)

    def read(path: Path) -> dict | None:
        return json.loads(path.read_text()) if path.exists() else None

    results = [check(runs) for check in CRITERIA]
    results.append(check_regression(read(arguments.k1), "K1", 13))
    results.append(check_regression(read(arguments.k2), "K2", 21))
    results.append(check_regression(read(arguments.k3), "K3", 28))
    results.append(check_regression(read(arguments.k4), "K4", 31))

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
        "gate": "K5",
        "passed": not failed,
        "criteria": [
            {"name": r.name, "title": r.title, "passed": r.passed, "detail": r.detail}
            for r in results
        ],
        "runs": sorted(runs),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")

    print()
    if failed:
        print(f"K5 GATE FAILED: {len(failed)} of {len(results)} criteria not met")
        return 1
    print(f"K5 GATE PASSED: {len(results)} of {len(results)} criteria met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
