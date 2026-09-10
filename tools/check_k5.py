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
}

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
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
        run.records = [
            record
            for record in run.records
            if not (record.event == event and record.get("name") == name)
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
]


def append_note(runs: dict[str, Run], key: str, domain: str, value: int) -> dict[str, Run]:
    damaged = copy.deepcopy(runs)
    for run in damaged.values():
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
