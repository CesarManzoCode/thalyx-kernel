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
    # the C++ runtime closure, in the native runtime
    "tls_up": 0x5009,
    "tls_bad": 0x500A,
    "constructors": 0x500B,
    "stack_smashed": 0x500C,
    "pthread_refused": 0x500E,
    "fortify_failed": 0x500F,
    "no_entropy": 0x5700,
    # the supervisor, about the engine
    "engine_ready": 0x5022,
    "engine_scope_pages": 0x5023,
    "engine_scope_cpu": 0x5024,
    # the resident engine, and the work's view of it
    "engine_loaded": 0x5500,
    "engine_weights": 0x5501,
    "engine_served": 0x5502,
    "engine_token": 0x5503,
    "engine_margin": 0x5504,
    "engine_digest": 0x5505,
    "engine_bound": 0x5506,
    "engine_elapsed": 0x5507,
    "engine_refused": 0x5508,
    "engine_prompt": 0x5509,
    "engine_load_failed": 0x550A,
    "engine_cancelled": 0x550B,
    "engine_failed": 0x550C,
    "engine_context": 0x550D,
    "engine_exception": 0x550E,
    "engine_argmax": 0x550F,
    "engine_log_lines": 0x5510,
    "engine_model_bytes": 0x5511,
    "engine_model_digest": 0x5512,
}

# What the engine-stage program asks, as the stage runner and the program have
# it; the gate checks the published record against these and against what the
# Linux reference answered for them.
import run_k5_stages  # noqa: E402

ENGINE_PROMPTS = run_k5_stages.ENGINE_PROMPTS


def fnv1a64(data: bytes) -> int:
    """FNV-1a over bytes, as the engine and the work compute it."""
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value

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
    # What Thalyx's own engine answered on Linux for this run's prompts, when
    # the stage has one. Host execution, and used only as the other side of a
    # comparison.
    reference: dict | None = None


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
        reference_path = child / "reference.json"
        runs[child.name] = Run(
            name=child.name,
            spec=spec,
            records=parse(serial.read_text()),
            exit_status=spec.get("exit_status"),
            timed_out=bool(spec.get("timed_out")),
            medium=medium,
            reference=json.loads(reference_path.read_text()) if reference_path.exists() else None,
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


# --- the engine stage -------------------------------------------------------


def published_content(run: Run, wanted: bytes) -> bytes | None:
    """The bytes a name is bound to in the version the medium says was
    published last, found by the digest its tree entry names."""
    if not run.medium:
        return None
    store = k4gate.read_store(run.medium)
    commits = k4gate.commits(store)
    if not commits:
        return None
    objects = k4gate.objects(store)
    manifest = objects.get(commits[-1]["root_digest"])
    if manifest is None:
        return None
    body = k4gate.fmt.decode("Manifest", manifest["content"], 0)
    tree = objects.get(bytes(body["tree_digest"]))
    if tree is None:
        return None
    header = k4gate.fmt.decode("TreeHeader", tree["content"], 0)
    at = k4gate.fmt.STRUCTS["TreeHeader"][0]
    width = k4gate.fmt.STRUCTS["TreeEntry"][0]
    for index in range(header["entry_count"]):
        entry = k4gate.fmt.decode("TreeEntry", tree["content"], at + index * width)
        if bytes(entry["name"])[: entry["name_len"]] == wanted:
            found = objects.get(bytes(entry["digest"]))
            return found["content"] if found else None
    return None


def engine_domain(run: Run) -> Record | None:
    created = [r for r in by_event(run.records, "domain.created") if r.get("name") == "nengine"]
    return created[0] if len(created) == 1 else None


def scope_id(run: Run, label: str) -> str | None:
    found = [r for r in by_event(run.records, "scope.created") if r.get("label") == label]
    return found[-1].get("id") if found else None


def image_module(run: Run, name: str) -> dict | None:
    manifest = run.spec.get("image_manifest") or {}
    for module in manifest.get("modules", []):
        if module.get("name") == name:
            return module
    return None


def reference_model(run: Run) -> bytes | None:
    """The model file the host holds, if it is the one the image carried."""
    import build_reference  # noqa: E402
    import hashlib

    module = image_module(run, "k5model")
    path = ROOT / "build" / "reference" / "tiny.gguf"
    if module is None or not path.exists():
        return None
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != module.get("sha256"):
        return None
    if module.get("sha256") != build_reference.MODEL_SHA256:
        return None
    return data


def check_cxx_runtime(runs: dict[str, Run]) -> Result:
    """The C++ standard library the engine links keeps its exception state and
    its stack guard behind FS. The kernel is what gives a thread an FS base, so
    whether the runtime stood up is the kernel's record first."""
    result = Result(
        "cxx_runtime",
        "the kernel set each engine thread's own thread pointer, its TLS held and its constructors ran",
    )
    run = stage_run(runs, "engine")
    if run is None:
        result.detail = "no engine run"
        return result
    pointers = [r for r in by_event(run.records, "thread.pointer") if r.get("name") == "nengine"]
    threads = {r.get("thread") for r in pointers}
    bases = {r.get("fs_base") for r in pointers}
    tls = note_values(run.records, "tls_up", "nengine")
    indexes = sorted(value >> 32 for value in tls)
    sizes = {value & 0xFFFFFFFF for value in tls}
    manifest = run.spec.get("image_manifest") or {}
    linked = (((manifest.get("native") or {}).get("programs") or {}).get("nengine") or {})
    linked_tls = (linked.get("closure") or {}).get("tls_bytes")
    constructors = note_values(run.records, "constructors", "nengine")
    failures = sum(
        len(note_values(run.records, key, "nengine"))
        for key in ("tls_bad", "stack_smashed", "fortify_failed", "no_entropy", "pthread_refused")
    )
    result.passed = (
        len(threads) >= 2
        and len(bases) == len(threads)
        and indexes == [0, 1]
        and sizes == {linked_tls}
        and linked_tls is not None
        and len(constructors) == 1
        and constructors[0] > 0
        and failures == 0
    )
    result.detail = (
        f"threads given a pointer {sorted(threads)}, distinct bases {len(bases)}, "
        f"TLS installed for thread indexes {indexes} of {sorted(sizes)} bytes "
        f"(the linked image says {linked_tls}), constructors {constructors}, "
        f"closure failures {failures}"
    )
    result.evidence = [line_of(r) for r in pointers][:2]
    return result


def check_engine_resident(runs: dict[str, Run]) -> Result:
    """One engine, one load, two answers, and the weights it loaded are the
    ones the host pinned: the engine names them by a digest the host can
    recompute from its own copy."""
    result = Result(
        "engine_resident",
        "the engine loaded the pinned model once and served both inferences from it (engine notes)",
    )
    run = stage_run(runs, "engine")
    if run is None:
        result.detail = "no engine run"
        return result
    model = reference_model(run)
    created = [r for r in by_event(run.records, "domain.created") if r.get("name") == "nengine"]
    terminated = [r for r in by_event(run.records, "domain.terminated") if r.get("name") == "nengine"]
    loaded = note_values(run.records, "engine_loaded", "nengine")
    served = note_values(run.records, "engine_served", "nengine")
    weights = note_values(run.records, "engine_weights", "nengine")
    model_bytes = note_values(run.records, "engine_model_bytes", "nengine")
    model_digest = note_values(run.records, "engine_model_digest", "nengine")
    ready = note_values(run.records, "engine_ready", "supervisor")
    work_served = note_values(run.records, "engine_served", "k5pub")
    result.passed = (
        model is not None
        and len(created) == 1
        and not terminated
        and len(loaded) == 1
        and loaded[0] > 0
        and served == [1, 2]
        and work_served == [1, 2]
        and model_bytes == [len(model)]
        and model_digest == [fnv1a64(model)]
        and ready == [len(model)]
        and len(weights) == 1
        and 0 < weights[0] <= len(model)
    )
    result.detail = (
        f"engine domains {len(created)} (terminated {len(terminated)}), loads {len(loaded)}, "
        f"served {served} (the work saw {work_served}), model {model_bytes} bytes digest "
        f"{[hex(v) for v in model_digest]} against the host's "
        f"{hex(fnv1a64(model)) if model else None} over {len(model) if model else None} bytes, "
        f"weights resident {weights}"
    )
    result.evidence = [line_of(r) for r in created] + [
        line_of(r) for r in notes(run.records, NOTE["engine_served"], "nengine")
    ]
    return result


def check_inference_charged_to_caller(runs: dict[str, Run]) -> Result:
    """Who pays is the kernel's decision, not the engine's claim. A worker bound
    to an invocation is charged to the scope the invocation came from, and the
    kernel writes down which account it used."""
    result = Result(
        "inference_charged_to_caller",
        "the kernel charged each inference to the scope of the work that asked, not to the engine",
    )
    run = stage_run(runs, "engine")
    if run is None:
        result.detail = "no engine run"
        return result
    engine = engine_domain(run)
    work_scope = scope_id(run, "work")
    engine_scope = scope_id(run, "engine")
    bound = [r for r in by_event(run.records, "sched.bound")
             if engine is not None and r.get("domain") == engine.get("id")]
    accounts = [(r.get("effective_scope"), r.get("origin_scope"), r.get("account"),
                 r.get("recovery")) for r in bound]
    invocations = sorted(r.number("invocation") for r in bound)
    noted = sorted(note_values(run.records, "engine_bound", "nengine"))
    engine_cpu = note_values(run.records, "engine_scope_cpu", "supervisor")
    work_cpu = note_values(run.records, "scope_after", "supervisor")
    result.passed = (
        engine is not None
        and work_scope is not None
        and len(bound) == 2
        and all(a == (work_scope, work_scope, "origin_budget", "0") for a in accounts)
        and work_scope != engine_scope
        and invocations == noted
        and bool(engine_cpu) and engine_cpu[0] > 0
        and bool(work_cpu) and work_cpu[0] > 0
    )
    result.detail = (
        f"bindings {len(bound)} charged as {accounts}, work scope {work_scope}, engine scope "
        f"{engine_scope}, invocations the kernel bound {invocations} and the engine noted {noted}, "
        f"the engine's scope was charged {engine_cpu}ns, the work's {work_cpu}ns"
    )
    result.evidence = [line_of(r) for r in bound]
    return result


def check_engine_read_the_prompt(runs: dict[str, Run]) -> Result:
    """The engine reached the prompt through the capability the work lent and
    through nothing else, and what it read is what the host chose."""
    result = Result(
        "engine_read_the_prompt",
        "the engine read exactly the prompts the host chose, through the buffer the work lent (notes)",
    )
    run = stage_run(runs, "engine")
    if run is None:
        result.detail = "no engine run"
        return result
    expected = [fnv1a64(prompt.encode()) for prompt in ENGINE_PROMPTS]
    read = note_values(run.records, "engine_prompt", "nengine")
    lent = note_values(run.records, "engine_prompt", "k5pub")
    result.passed = read == expected and lent == expected
    result.detail = (
        f"the engine read {[hex(v) for v in read]}, the work lent {[hex(v) for v in lent]}, "
        f"the host's prompts digest to {[hex(v) for v in expected]}"
    )
    result.evidence = [line_of(r) for r in notes(run.records, NOTE["engine_prompt"], "nengine")]
    return result


def check_engine_matches_reference(runs: dict[str, Run]) -> Result:
    """The decisive one for the engine, and the guest does not narrate it.

    The medium carries the version the work published, and in it `model.json`
    with the completion bytes the native engine produced. The Linux reference
    is Thalyx's own engine, unchanged, on the same model: host execution, used
    only as the other side of this comparison. Same model, same prompt, same
    greedy decision, same bytes -- or the port is not the same engine."""
    result = Result(
        "engine_matches_reference",
        "the published record carries the completions Thalyx's own engine gives on Linux",
    )
    run = stage_run(runs, "engine")
    if run is None or run.reference is None:
        result.detail = "no engine run, or no reference answers beside it"
        return result
    content = published_content(run, b"model.json")
    try:
        record = json.loads(content) if content else None
    except ValueError:
        record = None
    answers = (record or {}).get("answers", [])
    reference = run.reference.get("answers", [])
    native = [(a.get("prompt"), a.get("text_hex")) for a in answers]
    linux = [(a.get("prompt"), a.get("completion_hex")) for a in reference
             if a.get("status") == 0]
    tokens = note_values(run.records, "engine_token", "nengine")
    argmax = note_values(run.records, "engine_argmax", "nengine")
    digests = note_values(run.records, "engine_digest", "nengine")
    recorded_digests = [int(a.get("token_digest", "0"), 16) for a in answers]
    margins = note_values(run.records, "engine_margin", "nengine")
    result.passed = (
        len(native) == len(ENGINE_PROMPTS)
        and [p for p, _ in native] == ENGINE_PROMPTS
        and native == linux
        and all(text for _, text in native)
        and tokens == argmax
        and len(tokens) == len(ENGINE_PROMPTS)
        and recorded_digests == digests
        and all(margin > 0 for margin in margins)
        and "not native evidence" in run.reference.get("note", "")
    )
    result.detail = (
        f"native {native}, Linux reference {linux}, first tokens {tokens} "
        f"(raw argmax {argmax}), margins {margins} ppm, "
        f"record digests match the engine's {recorded_digests == digests}"
    )
    result.evidence = [f"published model.json: {content[:160].decode(errors='replace')}"] if content else []
    return result


def check_engine_confinement(runs: dict[str, Run]) -> Result:
    """What the engine can reach is what was installed and mapped into it, and
    the kernel wrote each of those down as it happened."""
    result = Result(
        "engine_confinement",
        "the engine held its endpoint, its signals and its own scope, and the model read-only",
    )
    run = stage_run(runs, "engine")
    if run is None:
        result.detail = "no engine run"
        return result
    installed = sorted(
        (r.number("slot"), r.get("object_type"))
        for r in by_event(run.records, "cap.installed") if r.get("name") == "nengine"
    )
    wanted = [(0, "scope"), (1, "domain"), (3, "endpoint"), (5, "signal"), (6, "signal"),
              (9, "signal")]
    model = [r for r in by_event(run.records, "mem.sealed") if r.get("label") == "k5model"]
    model_id = model[0].get("object") if model else None
    bulk = [r for r in by_event(run.records, "mem.mapped")
            if r.get("name") == "nengine" and r.get("vaddr") == "0x50000000"]
    result.passed = (
        installed == wanted
        and model_id is not None
        and len(bulk) == 1
        and bulk[0].get("object") == model_id
        and bulk[0].get("rights") == "0x100"
        and bulk[0].get("writable_maps") == "0"
    )
    result.detail = (
        f"capabilities installed {installed}, the model object {model_id} mapped "
        f"{[(r.get('object'), r.get('rights'), r.get('writable_maps')) for r in bulk]}"
    )
    result.evidence = [line_of(r) for r in bulk]
    return result


def check_engine_vertical(runs: dict[str, Run]) -> Result:
    """The whole vertical with the engine in it: a program asked the model, a
    real tool decided, and the version published carries the answers."""
    result = Result(
        "engine_vertical",
        "a program asked the engine, a real tool passed the candidate, and the version carries the answers",
    )
    run = stage_run(runs, "engine")
    if run is None or not run.medium:
        result.detail = "no engine run"
        return result
    published = note_values(run.records, "published", "k5pub")
    verdicts = note_values(run.records, "tool_verdict", "k5pub")
    read = note_values(run.records, "evidence_read", "k5pub")
    finish = note_values(run.records, "program_finish", "k5pub")
    ended = (finish[-1] & 0xFF) if finish else None
    names = published_lengths(run)
    done = note_values(run.records, "work_done", "k5pub")
    result.passed = (
        published == [1, 2]
        and verdicts == [0]
        and read == [5 | (1 << 32)]
        and ended == FINISH["returned"]
        and b"model.json" in names
        and done == [1 | (1 << 32)]
    )
    result.detail = (
        f"generations published {published}, tool exit {verdicts}, evidence read {[hex(v) for v in read]}, "
        f"program ended {ended}, published names {sorted(n.decode() for n in names)}, work done {done}"
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
    check_cxx_runtime,
    check_engine_resident,
    check_inference_charged_to_caller,
    check_engine_read_the_prompt,
    check_engine_matches_reference,
    check_engine_confinement,
    check_engine_vertical,
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
    (
        "no engine thread was given a thread pointer",
        "cxx_runtime",
        lambda runs: drop_event(runs, "thread.pointer", "nengine"),
    ),
    (
        "a thread's TLS layout check failed",
        "cxx_runtime",
        lambda runs: append_note(runs, "tls_bad", "nengine", 5, "engine"),
    ),
    (
        "the constructors never ran",
        "cxx_runtime",
        lambda runs: damage_note(runs, "constructors", "nengine", 0),
    ),
    (
        "the engine loaded its weights a second time",
        "engine_resident",
        lambda runs: append_note(runs, "engine_loaded", "nengine", 123, "engine"),
    ),
    (
        "the engine's model digest is not the host's copy",
        "engine_resident",
        lambda runs: damage_note(runs, "engine_model_digest", "nengine", 0x1234),
    ),
    (
        "both answers claim to be the first served",
        "engine_resident",
        lambda runs: damage_note(runs, "engine_served", "nengine", 1),
    ),
    (
        "an inference was charged to the engine instead of the work",
        "inference_charged_to_caller",
        lambda runs: rebind_to_engine(runs),
    ),
    (
        "no worker was ever bound to an invocation",
        "inference_charged_to_caller",
        lambda runs: drop_event_everywhere(runs, "sched.bound"),
    ),
    (
        "the engine read a prompt the work did not lend",
        "engine_read_the_prompt",
        lambda runs: damage_note(runs, "engine_prompt", "nengine", 0xBAD),
    ),
    (
        "the Linux reference answered differently",
        "engine_matches_reference",
        lambda runs: rewrite_reference(runs),
    ),
    (
        "the sampler's first choice was not the highest logit",
        "engine_matches_reference",
        lambda runs: damage_note(runs, "engine_argmax", "nengine", 7),
    ),
    (
        "the engine was given a capability to the state service",
        "engine_confinement",
        lambda runs: add_event_in(runs, "engine", "cap.installed",
                                  {"target": "3", "name": "nengine", "slot": "2",
                                   "object_type": "endpoint"}),
    ),
    (
        "the model was mapped writable",
        "engine_confinement",
        lambda runs: rewrite_event_field(runs, "mem.mapped", "nengine", "vaddr", "0x50000000",
                                         "rights", "0x300"),
    ),
    (
        "the tool refused the engine-stage candidate and the run published anyway",
        "engine_vertical",
        lambda runs: damage_note_in(runs, "engine", "tool_verdict", "k5pub", 1),
    ),
]


def drop_event_everywhere(runs: dict[str, Run], event: str) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        run.records = [record for record in run.records if record.event != event]
    return damaged


def rebind_to_engine(runs: dict[str, Run]) -> dict[str, Run]:
    """Makes the kernel's record say the engine paid for the first inference."""
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        engine_scope = scope_id(run, "engine")
        for record in by_event(run.records, "sched.bound"):
            if engine_scope is not None:
                record.fields["effective_scope"] = engine_scope
                break
    return damaged


def rewrite_reference(runs: dict[str, Run]) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        if run.reference and run.reference.get("answers"):
            answer = run.reference["answers"][0]
            text = answer.get("completion_hex", "")
            answer["completion_hex"] = ("00" + text[2:]) if text[:2] != "00" else ("11" + text[2:])
    return damaged


def add_event_in(runs: dict[str, Run], stage: str, event: str, fields: dict[str, str]) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        if (run.spec.get("image_manifest") or {}).get("stage") == stage:
            run.records.append(Record("kernel", 99996, 1, event, dict(fields)))
    return damaged


def rewrite_event_field(runs: dict[str, Run], event: str, name: str, key: str, value: str,
                        field_name: str, new: str) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        for record in by_event(run.records, event):
            if record.get("name") == name and record.get(key) == value:
                record.fields[field_name] = new
    return damaged


def damage_note_in(runs: dict[str, Run], stage: str, key: str, domain: str,
                   value: int) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        if (run.spec.get("image_manifest") or {}).get("stage") != stage:
            continue
        for record in notes(run.records, NOTE[key], domain):
            record.fields["b"] = hex(value)
    return damaged


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
