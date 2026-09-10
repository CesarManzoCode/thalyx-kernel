#!/usr/bin/env python3
"""Evaluate a K3 run against the gate.

K3 is the first phase whose claims are about more than one processor and about
a device that writes memory on its own. Both make the same kind of evidence
worthless: a program's own account. A domain cannot see which processor it ran
on, cannot see whether an invalidation reached the others, and cannot see what
a device did to memory it was granted. So every criterion below is decided from
the kernel's own records, and the three that must read a program's notes say so
in their title -- a status only the caller received, a comparison only the
caller made, a validator only the driver could run.

Each criterion is evaluated separately, so it fails on its own rather than being
carried by the others, and each carries the lines it was decided from.

The K1 and K2 regressions are criteria here too. K3 grew inside the kernel that
boots K1 and serves K2, and a K3 gate that passed while either had quietly
broken would be measuring the wrong thing.

Usage: tools/check_k3.py [--run build/run-k3] [--manifest build/image-manifest-k3.json]
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

# Mirrors `thalyx_kernel::diag::FORMAT`.
FORMAT = "THLX1"

RECORD = re.compile(
    r"^" + FORMAT + r" (?P<source>loader|kernel) (?P<seq>\d+) (?P<ns>\d+|-) (?P<event>\S+)(?P<rest>.*)$"
)

# `run_k3.py` maps the kernel's completion status to QEMU's `(value << 1) | 1`.
EXIT_COMPLETE = 33

# Mirrors `thalyx_abi::generated::status`. Repeated rather than imported so a
# kernel that silently renumbered a status would fail this gate instead of
# redefining what it checks.
INVALID_ARGUMENT = -2
WRONG_TYPE = -5
INSUFFICIENT_RIGHTS = -6
CANCELLED = -13
STATE_CONFLICT = -18
UNSUPPORTED_PROFILE = -21

# Mirrors `thalyx_user_rt::k2::report`, K3 range and the K2 codes this phase
# still uses.
NOTE = {
    "built": 0x2002,
    "build_failed": 0x2003,
    "refused_as_expected": 0x2007,
    "not_refused": 0x2008,
    "done": 0x200B,
    "unexpected": 0x200C,
    "mapped": 0x2017,
    "sealed": 0x2019,
    "replied": 0x201B,
    "cpus_online": 0x3001,
    "work_interval": 0x3002,
    "shared_counter": 0x3003,
    "window_used": 0x3004,
    "admitted_before_fence": 0x3005,
    "refused_after_fence": 0x3006,
    "device_observed": 0x3007,
    "region_mapped": 0x3008,
    "irq_bound": 0x3009,
    "dma_granted": 0x300A,
    "profile_refused": 0x300B,
    "transport_ready": 0x300C,
    "block_completed": 0x300D,
    "ring_rejected": 0x300E,
    "ring_accepted": 0x300F,
    "stale_session_refused": 0x3010,
    "device_reset": 0x3011,
    "page_read": 0x3012,
    "seal_held": 0x3013,
    "validator_self_test": 0x3015,
}

# Mirrors `thalyx_abi::generated::dma_profile`.
PROFILE_WEAK = 1
PROFILE_STRONG = 2

# Domains of the K3 package. The supervisor is the one the kernel builds;
# everything else exists only because the supervisor made it.
SUPERVISOR = "supervisor"
PROBE = "probe"
WRITER = "writer"
DRIVER = "k3driver"

# The address the probe worker reads and the writer writes. Named here because
# two criteria have to agree that the fault they found is at the page the
# withdrawal took away, and not at some other page.
PROBE_VADDR = 0x2020_0000

# Operations this phase's workload must reach. Not the whole interface: the K3
# package is not the K2 package and does not pretend to exercise what it never
# touches. What it must not do is claim a device path it never walked, so the
# eight device operations are here in full, together with the K2 operations the
# K3 claims are built on.
K3_REQUIRED_OPERATIONS = {
    "DEVICE_QUERY",
    "DEVICE_MAP_REGION",
    "DEVICE_UNMAP_REGION",
    "DEVICE_BIND_IRQ",
    "DEVICE_SET_MASTER",
    "DEVICE_DMA_MAP",
    "DEVICE_DMA_UNMAP",
    "DEVICE_RESET",
    "DOMAIN_MAP",
    "DOMAIN_UNMAP",
    "DOMAIN_ADD_THREAD",
    "DOMAIN_ACTIVATE",
    "MEMORY_SEAL",
    "MEMORY_READ",
    "MEMORY_WRITE",
    "SCOPE_FENCE",
    "SCOPE_DRAIN_STATUS",
    "SCOPE_RETIRE",
    "SCOPE_QUERY",
    "ENDPOINT_CALL",
    "ENDPOINT_RECEIVE",
    "INVOCATION_REPLY",
    "SIGNAL_WAIT",
    "SIGNAL_RAISE",
    "TIMER_ARM",
    "CAP_CLOSE",
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
        """Reads a field written as decimal or as `0x`-prefixed hex."""
        raw = self.fields.get(key)
        if raw is None:
            return None
        try:
            return int(raw, 16) if raw.startswith("0x") else int(raw)
        except ValueError:
            return None

    def signed(self, key: str) -> int | None:
        """Reads a field the kernel wrote as an unsigned 64-bit status."""
        value = self.number(key)
        if value is None:
            return None
        return value - (1 << 64) if value >= (1 << 63) else value


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
        # Firmware writes terminal control sequences before the loader starts.
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


def notes(records: list[Record], kind: str, domain: str | None = None) -> list[Record]:
    """Self-check notes of one report kind, optionally from one domain."""
    wanted = NOTE[kind]
    out = []
    for record in by_event(records, "user.note"):
        if record.get("kind") != "self_check" or record.number("a") != wanted:
            continue
        if domain is not None and record.get("name") != domain:
            continue
        out.append(record)
    return out


def note_value(records: list[Record], kind: str, domain: str | None = None) -> int | None:
    """The value of the single note of a kind, or None if there is not one."""
    found = notes(records, kind, domain)
    if len(found) != 1:
        return None
    return found[0].signed("b")


def line_of(record: Record) -> str:
    rendered = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{record.seq} {record.event} {rendered}".rstrip()


def online_cpus(records: list[Record]) -> int | None:
    summary = by_event(records, "smp.summary")
    if len(summary) != 1:
        return None
    return summary[0].number("online")


# --- criteria ---------------------------------------------------------------
#
# Each function receives the parsed records and the run record, fills in a
# Result, and never raises: a missing record is a failed criterion, not a crash.


def check_smp(records: list[Record], run: dict) -> Result:
    result = Result("smp", "los procesadores descritos arrancaron")
    summary = by_event(records, "smp.summary")
    online_records = by_event(records, "smp.online")
    if len(summary) != 1 or len(online_records) != 1:
        result.detail = "the kernel reported no single bring-up summary"
        return result
    described = summary[0].number("described")
    started = summary[0].number("started")
    failed = summary[0].number("failed")
    online = summary[0].number("online")
    if None in (described, started, failed, online):
        result.detail = "the bring-up summary is missing a count"
        return result
    if online < 2:
        result.detail = f"{online} processor(s) came online; K3 is about more than one"
        return result
    if failed != 0:
        result.detail = f"{failed} processor(s) failed to start"
        return result
    if online != described:
        result.detail = (
            f"firmware described {described} processors and {online} came online. "
            "A run that quietly leaves a described processor down is a smaller run"
        )
        return result
    if started != online - 1:
        result.detail = (
            f"{started} processors were started for {online} online; the bootstrap "
            "processor is the only one that should not have been"
        )
        return result

    mask = online_records[0].number("mask")
    if mask is None or bin(mask).count("1") != online:
        result.detail = f"the online mask 0x{mask:x} does not name {online} processors"
        return result

    per_cpu = by_event(records, "smp.cpu")
    if len(per_cpu) != online:
        result.detail = f"{len(per_cpu)} processors were described individually, {online} are online"
        return result
    roles = [record.get("role") for record in per_cpu]
    if roles.count("bootstrap") != 1:
        result.detail = f"{roles.count('bootstrap')} processors claim to be the bootstrap one"
        return result
    apic_ids = {record.number("apic_id") for record in per_cpu}
    if len(apic_ids) != online:
        result.detail = "two processors report the same local interrupt controller identity"
        return result

    result.passed = True
    result.detail = (
        f"{online} processors online of {described} described, {failed} failures, "
        f"mask 0x{mask:x}, {len(apic_ids)} distinct APIC identities, one bootstrap"
    )
    result.evidence = [line_of(summary[0]), line_of(online_records[0])]
    return result


def check_identity(records: list[Record], run: dict) -> Result:
    result = Result("identity", "cada procesador confirmó su propia identidad")
    started = by_event(records, "smp.ap_start")
    online_events = by_event(records, "smp.ap_online")
    if not online_events:
        result.detail = "no application processor reported itself online"
        return result
    if len(started) != len(online_events):
        result.detail = (
            f"{len(started)} processors were started and {len(online_events)} answered"
        )
        return result

    # The processor that answers has to be the one that was asked. Taking the
    # handshake as proof of identity would let a mis-parsed table start one
    # processor twice and count it as two.
    for record in online_events:
        if record.number("identity_match") != 1:
            result.detail = (
                f"cpu {record.get('cpu')} claimed APIC id {record.get('claimed_apic_id')} "
                f"where the firmware said {record.get('apic_id')}"
            )
            return result
        if record.get("backend") not in ("x2apic", "xapic"):
            result.detail = f"cpu {record.get('cpu')} reported no interrupt controller mode"
            return result
        if record.number("smep") != 1 or record.number("smap") != 1:
            result.detail = (
                f"cpu {record.get('cpu')} came online without the supervisor-access "
                "protections the bootstrap processor runs with"
            )
            return result
        if not record.number("lapic_hz"):
            result.detail = f"cpu {record.get('cpu')} calibrated no timer of its own"
            return result

    backends = {record.get("backend") for record in online_events}
    result.passed = True
    result.detail = (
        f"{len(online_events)} processors answered the identity they were started with, "
        f"each with its own calibrated timer and SMEP/SMAP set, backend {'/'.join(sorted(backends))}"
    )
    result.evidence = [line_of(record) for record in online_events[:2]]
    return result


def check_absent(records: list[Record], run: dict) -> Result:
    result = Result("absent", "control negativo: un procesador que no existe")
    probe = by_event(records, "smp.absent_probe")
    outcome = by_event(records, "smp.absent_result")
    if len(probe) != 1 or len(outcome) != 1:
        result.detail = (
            "the run never asked a processor that does not exist to start. Without it "
            "the bring-up code has only ever been run against processors that answer"
        )
        return result
    if outcome[0].number("answered") != 0:
        result.detail = "a processor that does not exist answered the handshake"
        return result
    before = outcome[0].number("online_before")
    after = outcome[0].number("online_after")
    if before != after:
        result.detail = f"the failed start changed the online count from {before} to {after}"
        return result
    if outcome[0].number("slot_added") != 0:
        result.detail = "the failed start left a processor slot behind"
        return result
    if outcome[0].number("stack_allocated") != 0:
        result.detail = (
            "the failed start released the kernel stack it had prepared. A processor "
            "that answers late would then run on memory somebody else owns"
        )
        return result

    result.passed = True
    result.detail = (
        f"an absent processor (apic id {probe[0].get('apic_id')}) was asked to start, did not "
        f"answer, changed the online count from {before} to {after}, and left its stack retained"
    )
    result.evidence = [line_of(probe[0]), line_of(outcome[0])]
    return result


def check_dispatch(records: list[Record], run: dict) -> Result:
    result = Result("dispatch", "todos los procesadores ejecutaron dominios")
    online = online_cpus(records)
    rows = by_event(records, "sched.cpu_summary")
    if online is None or not rows:
        result.detail = "the run reported no per-processor scheduling summary"
        return result
    if len(rows) != online:
        result.detail = f"{len(rows)} processors reported scheduling, {online} are online"
        return result
    idle = [row for row in rows if not row.number("dispatches") or not row.number("user_ns")]
    if idle:
        names = ", ".join(str(row.get("cpu")) for row in idle)
        result.detail = (
            f"processor(s) {names} dispatched no user thread. Four processors that ran "
            "and one processor that ran everything produce the same total"
        )
        return result
    ticks = [row.number("ticks") for row in rows]
    if any(not value for value in ticks):
        result.detail = "a processor took no timer interrupt of its own"
        return result

    total = sum(row.number("user_ns") for row in rows)
    least = min(row.number("user_ns") for row in rows)
    result.passed = True
    result.detail = (
        f"{online} processors each took their own timer interrupts and dispatched user "
        f"threads; {total} ns of user execution in total, least-loaded processor {least} ns"
    )
    result.evidence = [line_of(row) for row in rows]
    return result


def check_simultaneity(records: list[Record], run: dict) -> Result:
    result = Result("simultaneity", "hilos dispatchados a la vez, no en turnos")
    online = online_cpus(records)
    summary = by_event(records, "sched.summary")
    if online is None or len(summary) != 1:
        result.detail = "the run reported no scheduling summary"
        return result
    peak = summary[0].number("peak_simultaneous_threads")
    started = summary[0].number("cpus_started")
    if peak is None or started is None:
        result.detail = "the scheduling summary reports no simultaneity"
        return result
    if started != online:
        result.detail = f"{started} processors were started and {online} are online"
        return result
    if peak < 2:
        result.detail = (
            f"the highest simultaneity any scope reached was {peak}. Threads that never "
            "held a scope at the same instant were time-sliced, not run in parallel"
        )
        return result
    if peak > online:
        result.detail = f"{peak} threads were dispatched at once on {online} processors"
        return result

    # The ceiling is per scope, and the peak is recorded where the count
    # changes. A scope reporting more than its own ceiling would mean the
    # reservation admitted a dispatch it had no slot for.
    over = []
    for record in by_event(records, "scope.accounting"):
        running = record.number("max_running")
        ceiling = record.number("parallelism")
        if running is None or ceiling is None:
            continue
        if running > ceiling:
            over.append(f"{record.get('label')} reached {running} against a ceiling of {ceiling}")
    if over:
        result.detail = "; ".join(over)
        return result

    result.passed = True
    result.detail = (
        f"{peak} threads were dispatched at the same instant on {online} processors, and no "
        "scope ever exceeded its own simultaneity ceiling"
    )
    result.evidence = [line_of(summary[0])]
    return result


def check_clock(records: list[Record], run: dict) -> Result:
    result = Result("clock", "un solo reloj monótono entre procesadores")
    summary = by_event(records, "sched.summary")
    if len(summary) != 1:
        result.detail = "the run reported no scheduling summary"
        return result
    readings = summary[0].number("clock_readings")
    regressions = summary[0].number("clock_regressions")
    worst = summary[0].number("worst_regression_ns")
    if readings is None or regressions is None:
        result.detail = "the summary reports no clock observations"
        return result
    if readings < 1000:
        result.detail = (
            f"{readings} clock readings is too few to say anything about monotonicity "
            "across processors"
        )
        return result
    if regressions != 0:
        result.detail = (
            f"the clock went backwards {regressions} time(s), worst {worst} ns. A time "
            "source that regresses between processors makes every duration in this run a guess"
        )
        return result

    # The timestamps in the log are the same clock, read from every processor
    # that emits a record. If they were per-processor the sequence would not be
    # ordered.
    stamped = [record.ns for record in records if record.ns is not None]
    backwards = [b for a, b in zip(stamped, stamped[1:]) if b < a]
    if backwards:
        result.detail = (
            f"{len(backwards)} record(s) carry a timestamp earlier than the record before them"
        )
        return result

    result.passed = True
    result.detail = (
        f"{readings} readings from every processor with {regressions} regressions, and "
        f"{len(stamped)} record timestamps in non-decreasing order"
    )
    result.evidence = [line_of(summary[0])]
    return result


def check_budget(records: list[Record], run: dict) -> Result:
    result = Result("budget", "el presupuesto agregado limitó la admisión")
    rows = by_event(records, "scope.accounting")
    summary = by_event(records, "sched.summary")
    if not rows or len(summary) != 1:
        result.detail = "the run reported no scope accounting"
        return result

    over = []
    for record in rows:
        committed = record.number("max_committed_ns")
        budget = record.number("budget_ns")
        if committed is None or budget is None:
            result.detail = f"scope {record.get('label')} reports no commitment peak"
            return result
        if committed > budget:
            over.append(
                f"{record.get('label')} had {committed} ns committed against a budget of {budget}"
            )
    if over:
        result.detail = (
            "a scope was promised more than its budget: " + "; ".join(over)
        )
        return result

    # A ceiling nothing was ever refused against is a number. At least one
    # scope has to have run out and had a dispatch turned away.
    refused = [record for record in rows if (record.number("dispatch_refusals") or 0) > 0]
    if not refused:
        result.detail = (
            "no scope ever refused a dispatch for want of budget, so nothing in this run "
            "shows the budget bounding anything"
        )
        return result

    # Admission is bounded exactly; execution is not, because preemption is
    # tick-driven. What that costs has to be attributable rather than assumed,
    # so the excess is compared against the longest interval a single charge
    # actually covered on this platform.
    interval = summary[0].number("max_charge_interval_ns")
    if not interval:
        result.detail = "the summary does not report how long a single charge ran"
        return result
    # No more dispatches can overrun at once than there are processors, whatever
    # a scope's own ceiling says.
    online = online_cpus(records) or 1
    unattributed = []
    for record in rows:
        excess = record.number("max_excess_ns") or 0
        ceiling = min(record.number("parallelism") or 1, online)
        if excess > interval * ceiling:
            unattributed.append(
                f"{record.get('label')} ran {excess} ns beyond what its window could admit, "
                f"more than {ceiling} preemption latencies of {interval} ns"
            )
    if unattributed:
        result.detail = "; ".join(unattributed)
        return result

    worst = max(record.number("max_excess_ns") or 0 for record in rows)
    grants = sum(record.number("dispatch_grants") or 0 for record in rows)
    refusals = sum(record.number("dispatch_refusals") or 0 for record in rows)
    result.passed = True
    result.detail = (
        f"no scope was ever committed beyond its budget across {grants} granted and "
        f"{refusals} refused reservations; the most any window executed beyond what it "
        f"could admit is {worst} ns, within the {interval} ns a single dispatch actually ran"
    )
    result.evidence = [line_of(record) for record in rows]
    return result


def check_debt(records: list[Record], run: dict) -> Result:
    result = Result("debt", "el exceso se arrastró en vez de perdonarse")
    debts = by_event(records, "scope.debt")
    rows = by_event(records, "scope.accounting")
    if not debts:
        result.detail = (
            "no window ever closed over budget, so nothing in this run shows what happens "
            "to an overrun"
        )
        return result
    if any(record.get("carried_into_next") != "1" for record in debts):
        result.detail = "a window closed over budget and the overrun was not carried"
        return result

    # The debt a scope reports has to be at least the overruns it accumulated:
    # a carry that shrank would be an overrun forgiven at a boundary.
    by_scope: dict[str, list[Record]] = {}
    for record in debts:
        by_scope.setdefault(str(record.get("scope")), []).append(record)
    for scope, entries in by_scope.items():
        values = [record.number("debt_ns") or 0 for record in entries]
        if any(b < a for a, b in zip(values, values[1:])):
            result.detail = f"scope {scope} reported a debt that decreased between windows"
            return result

    carried = {}
    for record in rows:
        carried[str(record.get("label"))] = record.number("debt_ns") or 0
    indebted = {label: value for label, value in carried.items() if value}
    if not indebted:
        result.detail = "windows closed over budget but no scope ended the run holding debt"
        return result

    worst = max(indebted.items(), key=lambda item: item[1])
    result.passed = True
    result.detail = (
        f"{len(debts)} windows closed over budget across {len(by_scope)} scopes, every one "
        f"carried into the next; {worst[0]} ended holding {worst[1]} ns of debt"
    )
    result.evidence = [line_of(debts[0]), line_of(debts[-1])]
    return result


def check_migration(records: list[Record], run: dict) -> Result:
    result = Result("migration", "hilos que cambiaron de procesador con su estado FP")
    moves = by_event(records, "sched.migrated")
    if not moves:
        result.detail = (
            "no thread ever changed processor. A scheduler that never migrates has not "
            "shown that a thread's state travels with it"
        )
        return result
    unsaved = [record for record in moves if record.get("fp_state") != "saved_and_restored"]
    if unsaved:
        result.detail = (
            f"{len(unsaved)} migration(s) moved a thread without carrying its floating-point "
            "state; the first is at seq " + str(unsaved[0].seq)
        )
        return result
    pairs = {(record.get("from_cpu"), record.get("to_cpu")) for record in moves}
    if len(pairs) < 2:
        result.detail = f"every migration went the same way ({pairs}); that is one edge, not motion"
        return result
    threads = {record.get("thread") for record in moves}

    # A migrated thread has to keep running afterwards. One that faulted at its
    # first instruction on the new processor would produce these records too.
    faults = by_event(records, "user.fault")
    unexplained = [
        record
        for record in faults
        if record.get("class") != "user_fault" or record.number("cr2") != PROBE_VADDR
    ]
    if unexplained:
        result.detail = (
            f"{len(unexplained)} fault(s) happened at addresses this run did not take away; "
            "the first is " + line_of(unexplained[0])
        )
        return result

    result.passed = True
    result.detail = (
        f"{len(moves)} migrations of {len(threads)} threads over {len(pairs)} distinct "
        "processor pairs, every one carrying the thread's floating-point state"
    )
    result.evidence = [line_of(moves[0]), line_of(moves[-1])]
    return result


def check_shootdown(records: list[Record], run: dict) -> Result:
    result = Result("shootdown", "la invalidación la acusaron todos los procesadores")
    online = online_cpus(records)
    unmapped = [
        record for record in by_event(records, "mem.unmapped") if record.number("acknowledged_cpus")
    ]
    summary = by_event(records, "sched.summary")
    if online is None or not unmapped or len(summary) != 1:
        result.detail = "no mapping was withdrawn from a live address space"
        return result

    for record in unmapped:
        acknowledged = record.number("acknowledged_cpus")
        expected = record.number("expected_cpus")
        if expected != online:
            result.detail = (
                f"a withdrawal expected {expected} processors to acknowledge with {online} online"
            )
            return result
        if acknowledged != expected:
            result.detail = f"{acknowledged} of {expected} processors acknowledged the withdrawal"
            return result
        if record.number("acknowledged") != 1:
            result.detail = "a withdrawal answered without being acknowledged"
            return result
        # The instantaneous count can legitimately be zero. The mask cannot: a
        # translation has to be retired everywhere the space has ever run, and
        # that set is what the acknowledgement is measured against.
        seen = record.number("space_cpus")
        if seen is None or seen != online:
            result.detail = (
                f"the address space had run on {seen} processors of {online}; a withdrawal "
                "from a space that only ever ran on one is not a cross-processor withdrawal"
            )
            return result

    if summary[0].number("ack_timeouts") != 0:
        result.detail = f"{summary[0].get('ack_timeouts')} invalidations were never acknowledged"
        return result
    ipis = summary[0].number("shootdown_ipis")
    published = summary[0].number("invalidations")
    if not ipis or not published:
        result.detail = "no invalidation was published or signalled to another processor"
        return result

    result.passed = True
    result.detail = (
        f"{len(unmapped)} withdrawals from address spaces that had run on all {online} "
        f"processors, each acknowledged by all {online}; {published} invalidations published, "
        f"{ipis} signalled, no timeouts"
    )
    result.evidence = [line_of(record) for record in unmapped]
    return result


def check_probe(records: list[Record], run: dict) -> Result:
    result = Result("probe", "la traducción retirada dejó de funcionar")
    unmapped = [
        record
        for record in by_event(records, "mem.unmapped")
        if record.number("vaddr") == PROBE_VADDR
    ]
    if not unmapped:
        result.detail = f"the run never withdrew the mapping at 0x{PROBE_VADDR:x}"
        return result
    withdrawal = unmapped[0]

    reads = notes(records, "page_read", PROBE)
    before = [record for record in reads if record.seq < withdrawal.seq]
    if not before:
        result.detail = (
            "the probe never read the page before it was taken away, so a fault afterwards "
            "would not show that anything changed"
        )
        return result

    faults = [
        record
        for record in by_event(records, "user.fault")
        if record.get("name") == PROBE and record.number("cr2") == PROBE_VADDR
    ]
    if not faults:
        result.detail = "the probe never faulted at the address the withdrawal took away"
        return result
    fault = faults[0]
    if fault.seq < withdrawal.seq:
        result.detail = "the probe faulted before the withdrawal, not because of it"
        return result
    if fault.number("error") is None or fault.number("error") & 0x2:
        result.detail = (
            "the probe's fault was a write; a read fault is what shows the translation "
            "itself is gone rather than its write permission"
        )
        return result
    if fault.get("kernel") != "survives":
        result.detail = "the kernel did not survive the fault it contained"
        return result
    after = [record for record in reads if record.seq > fault.seq]
    if after:
        result.detail = f"the probe read the page {len(after)} more time(s) after faulting on it"
        return result

    result.passed = True
    result.detail = (
        f"the probe read the page {len(before)} times, the withdrawal was acknowledged by "
        f"{withdrawal.get('acknowledged_cpus')} processors, and the next read faulted "
        f"(error 0x{fault.number('error'):x}) with the kernel surviving"
    )
    result.evidence = [line_of(withdrawal), line_of(fault)]
    return result


def check_seal(records: list[Record], run: dict) -> Result:
    result = Result("seal", "el sello se publicó contra un escritor vivo")
    online = online_cpus(records)
    sealed = [
        record
        for record in by_event(records, "mem.sealed")
        if record.get("perimeter") == "cpu_translations_retired"
    ]
    if online is None or not sealed:
        result.detail = (
            "no object was sealed with a cross-processor perimeter. A seal that only claims "
            "a uniprocessor perimeter is a K2 seal"
        )
        return result
    seal = sealed[0]
    if (seal.number("writers_withdrawn") or 0) < 1:
        result.detail = "the seal withdrew no writer, so it sealed something nobody could write"
        return result
    if seal.number("remaining_maps") != 0:
        result.detail = f"the seal left {seal.get('remaining_maps')} mapping(s) behind"
        return result
    if seal.number("acknowledged_cpus") != online or seal.number("expected_cpus") != online:
        result.detail = (
            f"the seal was acknowledged by {seal.get('acknowledged_cpus')} of "
            f"{seal.get('expected_cpus')} processors with {online} online"
        )
        return result
    if seal.number("writer_cpus_ever") != online:
        result.detail = (
            f"the writer had only ever run on {seal.get('writer_cpus_ever')} processors of "
            f"{online}; a seal against a writer confined to one processor proves less"
        )
        return result
    if seal.get("dma") != "none":
        result.detail = f"the seal claims a perimeter with dma={seal.get('dma')}, which K3 cannot enforce"
        return result

    faults = [
        record
        for record in by_event(records, "user.fault")
        if record.get("name") == WRITER and record.seq > seal.seq
    ]
    if not faults:
        result.detail = "the writer never faulted after the seal, so it may simply have stopped"
        return result
    fault = faults[0]
    if not fault.number("error") or not fault.number("error") & 0x2:
        result.detail = "the writer's fault after the seal was not a write fault"
        return result

    # The one thing only the caller can report: the same bytes read twice with
    # real time in between, during which the writer was still trying.
    held = note_value(records, "seal_held", SUPERVISOR)
    if held != 1:
        result.detail = "the supervisor did not report reading the same bytes twice after the seal"
        return result

    result.passed = True
    result.detail = (
        f"{seal.get('writers_withdrawn')} writer withdrawn from an object the writer had run "
        f"against on all {online} processors, acknowledged by all {online}; the writer's next "
        f"write faulted (error 0x{fault.number('error'):x}) and the bytes read the same twice"
    )
    result.evidence = [line_of(seal), line_of(fault)]
    return result


def check_quarantine(records: list[Record], run: dict) -> Result:
    result = Result("quarantine", "los marcos volvieron sólo tras la invalidación")
    quarantine = by_event(records, "mm.quarantine")
    reclaimed = by_event(records, "mm.reclaimed")
    if not quarantine or not reclaimed:
        result.detail = "the run reclaimed no domain, so nothing was ever deferred"
        return result
    final = quarantine[-1]
    if final.get("condition") != "all_processors_invalidated":
        result.detail = (
            f"frames were released on condition '{final.get('condition')}'. A frame another "
            "processor may still have a translation for is not free"
        )
        return result
    if not final.number("peak"):
        result.detail = "no frame was ever held in quarantine; reclamation was immediate"
        return result
    if final.number("still_held") != 0:
        result.detail = f"{final.get('still_held')} frame(s) were still in quarantine at the end"
        return result
    if final.number("retained_permanently") != 0:
        result.detail = f"{final.get('retained_permanently')} frame(s) were never released"
        return result
    immediate = [record for record in reclaimed if record.get("release") != "deferred"]
    if immediate:
        result.detail = (
            f"{len(immediate)} domain(s) released their frames immediately rather than "
            "through the quarantine"
        )
        return result

    # Every domain's charge has to reach zero, and the quarantine is where it
    # goes in between. A charge that never cleared would mean the deferral
    # became a leak.
    summaries = by_event(records, "k1.domain_summary")
    held = [record for record in summaries if record.number("charged_frames")]
    if not summaries:
        result.detail = "the run reported no per-domain summary to check the charges against"
        return result
    if held:
        names = ", ".join(str(record.get("name")) for record in held)
        result.detail = f"domain(s) {names} still held frames after the quarantine drained"
        return result

    result.passed = True
    result.detail = (
        f"{len(reclaimed)} domains reclaimed with release deferred, peak {final.get('peak')} "
        f"frames quarantined, {final.get('released_total')} released once every processor had "
        f"invalidated, none still held by any of {len(summaries)} domains"
    )
    result.evidence = [line_of(final), line_of(reclaimed[0])]
    return result


def check_barrier(records: list[Record], run: dict) -> Result:
    result = Result("barrier", "la barrera separó lo admitido de lo nuevo")
    fenced = by_event(records, "scope.fenced")
    replied = note_value(records, "replied", SUPERVISOR)
    if not fenced:
        result.detail = "no scope was ever fenced"
        return result
    if not replied:
        result.detail = (
            "the supervisor answered no call before raising the barrier, so there was "
            "nothing admitted for the barrier to be measured against"
        )
        return result

    admitted = notes(records, "admitted_before_fence")
    refused = notes(records, "refused_after_fence")
    if not admitted or not refused:
        result.detail = (
            "no caller reported both calls admitted and calls refused; a barrier that was "
            "raised before anyone called separates nothing"
        )
        return result
    total_admitted = sum(record.number("b") or 0 for record in admitted)
    if total_admitted < 1:
        result.detail = "every call was refused; nothing was ever admitted"
        return result
    wrong = [record for record in refused if record.signed("b") not in (CANCELLED, -8)]
    if wrong:
        result.detail = (
            "a call after the barrier was refused with "
            f"{wrong[0].signed('b')}, which is neither a cancellation nor a closed scope"
        )
        return result

    # The barrier has to have found something to cancel, and the scope has to
    # have reached quiescence afterwards rather than being declared quiet.
    retired = [record for record in by_event(records, "scope.retired")]
    if not retired:
        result.detail = "no fenced scope was ever retired"
        return result
    early = [record for record in retired if record.get("from_state") != "quiescent"]
    if early:
        result.detail = f"scope {early[0].get('id')} was retired from {early[0].get('from_state')}"
        return result
    leftover = [
        record
        for record in retired
        if record.number("retained_pages") or record.number("retained_metadata")
    ]
    if leftover:
        result.detail = (
            f"scope {leftover[0].get('id')} was retired still holding "
            f"{leftover[0].get('retained_pages')} pages and "
            f"{leftover[0].get('retained_metadata')} metadata"
        )
        return result

    result.passed = True
    result.detail = (
        f"{replied} calls answered before the barrier, {total_admitted} reported admitted by "
        f"their callers and the rest refused as cancelled; {len(retired)} scopes retired from "
        "quiescence holding nothing"
    )
    result.evidence = [line_of(fenced[0]), line_of(retired[-1])]
    return result


def check_device(records: list[Record], run: dict) -> Result:
    result = Result("device", "un dispositivo asignado con su perfil declarado")
    assigned = by_event(records, "device.assigned")
    if not assigned:
        result.detail = "no device was assigned; the K3 device path was never walked"
        return result
    if len(assigned) != 1:
        result.detail = f"{len(assigned)} devices were assigned; this package expects one"
        return result
    device = assigned[0]
    if device.number("version_1") != 1:
        result.detail = "the device was not a modern virtio function"
        return result
    if device.number("bus_master") != 0:
        result.detail = (
            "the device was assigned already able to issue transactions. Bus mastering is "
            "the last thing granted, not the first"
        )
        return result
    if device.get("state") != "ready":
        result.detail = f"the device was assigned in state {device.get('state')}"
        return result
    if not device.number("msix_vectors"):
        result.detail = "the device reported no message-signalled interrupt vectors"
        return result
    profile = device.number("dma_profile")
    if profile != PROFILE_WEAK:
        result.detail = (
            f"the device was assigned profile {profile}. This platform has no unit that "
            "translates device addresses, so anything but the weak profile is a claim the "
            "hardware does not support"
        )
        return result
    reason = device.get("profile_reason")
    if reason not in (
        "no_remapping_unit_described",
        "remapping_described_but_not_programmed",
    ):
        result.detail = f"the weak profile was chosen for reason '{reason}'"
        return result

    # The run record and the kernel have to agree about the platform. A run
    # configured with a remapping unit and a kernel that saw none is a gate
    # measuring a machine nobody built.
    summary = by_event(records, "device.summary")
    if not summary:
        result.detail = "the run reported no device summary"
        return result
    described = summary[-1].number("remapping_unit_described")
    configured = "intel-iommu" in str(run.get("iommu", ""))
    if bool(described) != configured:
        result.detail = (
            f"the run record says iommu={run.get('iommu')!r} and the kernel described "
            f"{described} remapping units"
        )
        return result
    if summary[-1].number("remapping_programmed") != 0:
        result.detail = (
            "the kernel reports a remapping unit it programmed. K3 does not program one, "
            "and a run that claims otherwise is claiming isolation it has not built"
        )
        return result

    result.passed = True
    result.detail = (
        f"one modern virtio function ({device.get('vendor')}:{device.get('device_id')}) assigned "
        f"in state ready without bus mastering, {device.get('msix_vectors')} interrupt vectors, "
        f"weak DMA profile because {reason}; remapping unit described={described}, programmed=0"
    )
    result.evidence = [line_of(device), line_of(summary[-1])]
    return result


def check_regions(records: list[Record], run: dict) -> Result:
    result = Result("regions", "ventanas de registros sin la tabla de interrupciones")
    regions = by_event(records, "device.region")
    mapped = by_event(records, "device.region_mapped")
    transport = by_event(records, "device.transport")
    if not regions or not mapped or not transport:
        result.detail = "the run mapped no device register window into a driver"
        return result

    kinds = {record.number("kind") for record in regions}
    if len(kinds) != len(regions):
        result.detail = "two register windows report the same structure kind"
        return result
    if len(regions) < 4:
        result.detail = (
            f"{len(regions)} register windows were found; a modern virtio function has a "
            "common configuration, a notification area, an ISR and a device-specific window"
        )
        return result

    # The interrupt table carries the address and the payload of an interrupt.
    # Page protection cannot separate two structures that share a page, so a
    # window in the same memory region as the table is a window that hands the
    # driver the ability to aim an interrupt anywhere.
    msix_bar = regions[0].number("msix_bar")
    if msix_bar is None:
        result.detail = "the run does not record where the interrupt table lives"
        return result
    if msix_bar != 255:
        shared = [record for record in regions if record.number("bar") == msix_bar]
        if shared:
            result.detail = (
                f"{len(shared)} register window(s) are in the same memory region as the "
                "interrupt table"
            )
            return result

    cacheable = [record for record in mapped if record.number("cacheable") != 0]
    if cacheable:
        result.detail = f"{len(cacheable)} register window(s) were mapped cacheable"
        return result
    executable = [record for record in mapped if record.number("executable") != 0]
    if executable:
        result.detail = f"{len(executable)} register window(s) were mapped executable"
        return result

    # The mapping is installed by the authority that holds the device, at an
    # address the driver is told. A window mapped in a session the device is no
    # longer in would be a stale window.
    sessions = {record.number("session") for record in mapped}
    if len(sessions) != 1:
        result.detail = f"windows were mapped across {len(sessions)} device sessions"
        return result

    withdrawn = by_event(records, "device.region_unmapped")
    if not withdrawn:
        result.detail = (
            "no register window was ever taken back by name. The reset takes them all at "
            "once; a window withdrawn on its own is what shows one can be"
        )
        return result
    if withdrawn[0].number("acknowledged") != 1:
        result.detail = "a register window was withdrawn without every processor acknowledging"
        return result

    result.passed = True
    result.detail = (
        f"{len(regions)} distinct structures found and {len(mapped)} mapped uncached and "
        f"non-executable into the driver, none sharing a memory region with the interrupt "
        f"table (bar {msix_bar}); one withdrawn by name and acknowledged by "
        f"{withdrawn[0].get('acknowledged_cpus')} processors"
    )
    result.evidence = [line_of(regions[0]), line_of(mapped[0]), line_of(withdrawn[0])]
    return result


def check_irq(records: list[Record], run: dict) -> Result:
    result = Result("irq", "interrupciones entregadas a la señal enlazada")
    bound = by_event(records, "device.irq_bound")
    delivered = by_event(records, "device.interrupt")
    summary = by_event(records, "device.summary")
    if not bound or not summary:
        result.detail = "no device interrupt was ever bound"
        return result
    if bound[0].get("table_written_by") != "kernel":
        result.detail = (
            f"the interrupt table entry was written by {bound[0].get('table_written_by')}. "
            "A driver that can write it can aim an interrupt at anything"
        )
        return result
    if not delivered:
        result.detail = (
            "no interrupt was delivered. A bound interrupt nothing ever arrived on shows "
            "the binding, not the path"
        )
        return result

    signals = {record.number("signal") for record in delivered}
    if signals != {bound[0].number("signal")}:
        result.detail = f"interrupts were delivered to signals {signals}, not the bound one"
        return result
    vectors = {record.number("vector") for record in delivered}
    if vectors != {bound[0].number("vector")}:
        result.detail = f"interrupts arrived on vectors {vectors}, not the bound one"
        return result
    counts = [record.number("deliveries") for record in delivered]
    if counts != sorted(counts) or counts[0] != 1:
        result.detail = f"the delivery counts {counts} are not a run of increasing deliveries"
        return result
    total = summary[-1].number("interrupts_delivered")
    if not total or total < len(delivered):
        result.detail = f"the summary reports {total} deliveries and {len(delivered)} were recorded"
        return result
    if summary[-1].number("interrupts_unclaimed") != 0:
        result.detail = (
            f"{summary[-1].get('interrupts_unclaimed')} interrupts arrived for no binding"
        )
        return result

    result.passed = True
    result.detail = (
        f"{total} interrupts delivered on vector {bound[0].get('vector')} to signal "
        f"{bound[0].get('signal')}, table entry written by the kernel, none unclaimed"
    )
    result.evidence = [line_of(bound[0]), line_of(delivered[0])]
    return result


def check_dma(records: list[Record], run: dict) -> Result:
    result = Result("dma", "un permiso de DMA débil, y el fuerte rechazado")
    granted = by_event(records, "device.dma_granted")
    refused = by_event(records, "device.profile_refused")
    if not granted:
        result.detail = "no DMA grant was made; the device never reached memory"
        return result
    grant = granted[0]
    if grant.number("profile") != PROFILE_WEAK:
        result.detail = f"the grant claims profile {grant.get('profile')} on a platform with no remapping"
        return result
    if grant.get("enforced_by") != "nothing_driver_is_trusted":
        result.detail = (
            f"the grant claims to be enforced by '{grant.get('enforced_by')}'. Without a "
            "programmed remapping unit nothing enforces it, and saying otherwise is the "
            "one claim this phase must not make"
        )
        return result

    if not refused:
        result.detail = (
            "nobody asked for the strong profile, so the refusal path was never entered. "
            "A profile that is only ever asked for when it is available is not a boundary"
        )
        return result
    if refused[0].number("required") != PROFILE_STRONG:
        result.detail = f"the refusal was for profile {refused[0].get('required')}"
        return result
    if refused[0].number("available") != PROFILE_WEAK:
        result.detail = f"the refusal claims {refused[0].get('available')} was available"
        return result
    if refused[0].number("remapping_programmed" if "remapping_programmed" in refused[0].fields else "iommu_translating") != 0:
        result.detail = "the strong profile was refused on a platform that could have provided it"
        return result

    # The status the caller received, which only the caller can report.
    status = note_value(records, "profile_refused", DRIVER)
    if status != UNSUPPORTED_PROFILE:
        result.detail = (
            f"the driver was told {status} rather than UNSUPPORTED_PROFILE ({UNSUPPORTED_PROFILE}). "
            "A refusal that arrives as some other error lets a driver retry into a downgrade"
        )
        return result

    # Bus mastering is the supervisor's to give and the driver's to be refused.
    # A grant that names a session the device is no longer in reaches nothing,
    # so the two have to agree.
    mastering = by_event(records, "device.bus_master")
    enabled = [record for record in mastering if record.number("effective") == 1]
    assigned = by_event(records, "device.assigned")
    if not enabled or not assigned:
        result.detail = "the device was never allowed to issue a transaction"
        return result
    if enabled[0].seq < assigned[0].seq:
        result.detail = "the device could master the bus before it was assigned to anyone"
        return result
    if enabled[0].number("session") != grant.number("session"):
        result.detail = (
            f"the grant belongs to session {grant.get('session')} and bus mastering to "
            f"{enabled[0].get('session')}"
        )
        return result
    by_driver = [
        record
        for record in by_event(records, "k2.refused")
        if record.get("name") == DRIVER and record.get("op") == "DEVICE_SET_MASTER"
    ]
    if not by_driver:
        result.detail = (
            "the driver was never refused bus mastering, so nothing shows the device's "
            "reach into memory is not the driver's to enable"
        )
        return result

    result.passed = True
    result.detail = (
        f"one grant of {grant.get('length')} bytes at iova {grant.get('iova')} under the weak "
        f"profile, enforced by nothing and saying so; the strong profile was asked for and "
        f"refused with {status} because {refused[0].get('reason')}"
    )
    result.evidence = [line_of(grant), line_of(refused[0])]
    return result


def check_blockio(records: list[Record], run: dict) -> Result:
    result = Result("blockio", "tráfico de bloques real, reportado por el driver")
    ready = note_value(records, "transport_ready", DRIVER)
    completed = notes(records, "block_completed", DRIVER)
    if ready is None:
        result.detail = "the driver never finished negotiating with the transport"
        return result
    # ACKNOWLEDGE | DRIVER | DRIVER_OK | FEATURES_OK
    if ready != 0x0F:
        result.detail = f"the transport settled at status 0x{ready:x}, not 0x0f"
        return result
    if len(completed) < 3:
        result.detail = (
            f"{len(completed)} block requests completed. A read, a write and a flush are "
            "three different paths through the device"
        )
        return result

    heads = set()
    for record in completed:
        value = record.number("b") or 0
        status = value & 0xFF
        length = (value >> 8) & 0xFFFFFFFF
        head = value >> 40
        if status != 0:
            result.detail = f"a request completed with device status {status}"
            return result
        heads.add(head)
    if len(heads) != len(completed):
        result.detail = f"{len(completed)} completions used {len(heads)} descriptor chains"
        return result

    # A completion the driver believed without an interrupt would be a poll.
    delivered = by_event(records, "device.interrupt")
    if len(delivered) < len(completed):
        result.detail = (
            f"{len(completed)} requests completed against {len(delivered)} interrupts"
        )
        return result
    if not notes(records, "done", DRIVER):
        result.detail = "the driver did not report reaching the end of its own script"
        return result

    result.passed = True
    result.detail = (
        f"the transport settled at 0x{ready:x} and {len(completed)} requests completed with "
        f"device status 0 over {len(heads)} distinct descriptor chains, each behind an interrupt"
    )
    result.evidence = [line_of(record) for record in completed]
    return result


def check_validator(records: list[Record], run: dict) -> Result:
    result = Result("validator", "el validador del anillo, probado contra daño (notas del driver)")
    self_test = note_value(records, "validator_self_test", DRIVER)
    rejected = notes(records, "ring_rejected", DRIVER)
    accepted = notes(records, "ring_accepted", DRIVER)
    if self_test is None:
        result.detail = (
            "the driver never ran its validator against entries it forged itself, so the "
            "rejecting branches are code no run has entered"
        )
        return result
    if self_test != 3:
        result.detail = (
            f"the validator refused {self_test} of the 3 damaged entries it was given"
        )
        return result
    if len(rejected) != 3:
        result.detail = f"{len(rejected)} rejections were recorded for 3 damaged entries"
        return result
    reasons = {record.number("b") for record in rejected}
    if len(reasons) != 3:
        result.detail = (
            f"the three rejections reported {len(reasons)} distinct reasons. A validator that "
            "refuses everything for one reason passes the other cases by accident"
        )
        return result
    if not accepted:
        result.detail = (
            "no used-ring entry was ever accepted; a validator that rejects everything is "
            "not a validator"
        )
        return result

    # The forged entries must live where the device cannot have written them.
    granted = by_event(records, "device.dma_granted")
    mapped = [record for record in by_event(records, "mem.mapped") if record.get("name") == DRIVER]
    if not granted or not mapped:
        result.detail = "the run does not record the ring object and what of it was granted"
        return result
    ring_pages = mapped[0].number("pages")
    granted_bytes = granted[0].number("length")
    if ring_pages is None or granted_bytes is None:
        result.detail = "the ring's size or the granted length is not recorded"
        return result
    if granted_bytes >= ring_pages * 4096:
        result.detail = (
            f"the device was granted all {granted_bytes} bytes of the {ring_pages}-page ring "
            "object, so the validator's own scratch is memory the device can write"
        )
        return result

    result.passed = True
    result.detail = (
        f"{self_test} damaged used-ring entries refused for {len(reasons)} distinct reasons and "
        f"{len(accepted)} plausible ones accepted, forged in the "
        f"{ring_pages * 4096 - granted_bytes} bytes of the ring object the device was not granted"
    )
    result.evidence = [line_of(record) for record in rejected]
    return result


def check_reset(records: list[Record], run: dict) -> Result:
    result = Result("reset", "el dispositivo se detuvo bajo su driver")
    online = online_cpus(records)
    reset = by_event(records, "device.reset")
    if online is None or not reset:
        result.detail = "the device was never reset by recovery authority"
        return result
    event = reset[0]
    if event.number("status_after_reset") != 0:
        result.detail = (
            f"the transport still reported status 0x{event.number('status_after_reset'):x} "
            "after the reset"
        )
        return result
    if event.get("confirmed_by") != "transport_status_zero":
        result.detail = (
            f"the reset was confirmed by '{event.get('confirmed_by')}' rather than by reading "
            "the transport back"
        )
        return result
    if event.number("bus_master") != 0:
        result.detail = "the device could still issue transactions after the reset"
        return result
    if not event.number("irq_unbound") or not event.number("regions_unmapped"):
        result.detail = "the reset left the driver holding interrupts or register windows"
        return result
    if not event.number("dma_pages_revoked"):
        result.detail = "the reset revoked no DMA grant, so the device kept its reach into memory"
        return result
    if event.number("acknowledged_cpus") != online or event.number("expected_cpus") != online:
        result.detail = (
            f"the reset's invalidation was acknowledged by {event.get('acknowledged_cpus')} of "
            f"{event.get('expected_cpus')} processors"
        )
        return result
    if (event.number("new_session") or 0) <= 1:
        result.detail = "the reset did not open a new session"
        return result

    # Bus mastering has to go before the transport is touched: a device that
    # can still master between the decision and the confirmation can still
    # write.
    mastering = [record for record in by_event(records, "device.bus_master") if record.seq < event.seq]
    if not mastering or mastering[-1].number("effective") != 0:
        result.detail = "bus mastering was not withdrawn before the reset"
        return result

    stale = note_value(records, "stale_session_refused", DRIVER)
    if stale != 3:
        result.detail = (
            f"the driver had {stale} of its remembered operations refused after the reset; "
            "everything it names belongs to a session that no longer exists"
        )
        return result

    result.passed = True
    result.detail = (
        f"bus mastering withdrawn, transport read back at 0x0, {event.get('irq_unbound')} "
        f"interrupt(s) unbound, {event.get('regions_unmapped')} window(s) unmapped, "
        f"{event.get('dma_pages_revoked')} DMA page(s) revoked, acknowledged by all {online} "
        f"processors, session {event.get('new_session')}; {stale} stale operations refused"
    )
    result.evidence = [line_of(event)]
    return result


def check_authority(records: list[Record], run: dict) -> Result:
    result = Result("authority", "el driver fue rechazado en lo que no se le dio")
    refusals = [
        record
        for record in by_event(records, "k2.refused")
        if record.get("name") == DRIVER and str(record.get("op")).startswith("DEVICE_")
    ]
    if not refusals:
        result.detail = "the driver was never refused a device operation"
        return result

    seen = {(record.get("op"), record.signed("status")) for record in refusals}
    required = [
        ("DEVICE_SET_MASTER", INSUFFICIENT_RIGHTS),
        ("DEVICE_RESET", INSUFFICIENT_RIGHTS),
        ("DEVICE_MAP_REGION", INVALID_ARGUMENT),
        ("DEVICE_DMA_MAP", UNSUPPORTED_PROFILE),
        ("DEVICE_DMA_UNMAP", STATE_CONFLICT),
    ]
    missing = [f"{op} with {status}" for op, status in required if (op, status) not in seen]
    if missing:
        result.detail = (
            "the driver was never refused: " + ", ".join(missing) + ". A boundary nothing "
            "was ever refused at is a boundary nobody has stood on"
        )
        return result

    # And nothing the driver asked for was wrongly permitted.
    unexpected = notes(records, "unexpected", DRIVER)
    not_refused = notes(records, "not_refused", DRIVER)
    if unexpected or not_refused:
        offender = (unexpected or not_refused)[0]
        result.detail = (
            f"the driver reported an outcome it did not expect: {line_of(offender)}"
        )
        return result

    result.passed = True
    result.detail = (
        f"{len(refusals)} device operations refused to the driver across "
        f"{len(seen)} distinct (operation, status) pairs, including bus mastering and reset "
        "for want of rights and the strong DMA profile for want of hardware"
    )
    result.evidence = [line_of(record) for record in refusals[:4]]
    return result


def check_outstanding(records: list[Record], run: dict) -> Result:
    result = Result("outstanding", "nada quedó al alcance del dispositivo")
    summary = by_event(records, "device.summary")
    if not summary:
        result.detail = "the run reported no device summary"
        return result
    final = summary[-1]
    for field_name, what in [
        ("grants_outstanding", "DMA grants"),
        ("maps_outstanding", "register windows"),
        ("bindings_outstanding", "interrupt bindings"),
        ("stale_maps", "windows from a session that ended"),
        ("stale_bindings", "interrupt bindings from a session that ended"),
    ]:
        value = final.number(field_name)
        if value is None:
            result.detail = f"the summary does not report {what}"
            return result
        if value != 0:
            result.detail = f"{value} {what} outlived the run"
            return result
    named = by_event(records, "device.grant_outstanding")
    if named:
        result.detail = f"{len(named)} DMA grants were still naming memory at the end"
        return result

    result.passed = True
    result.detail = (
        "no DMA grant, register window or interrupt binding outlived the run, and none was "
        "left naming a session that had ended"
    )
    result.evidence = [line_of(final)]
    return result


def check_coverage(records: list[Record], run: dict) -> Result:
    result = Result("coverage", "el camino K3 de la interfaz se recorrió entero")
    coverage = by_event(records, "k2.coverage")
    untouched = by_event(records, "k2.operation_untouched")
    if len(coverage) != 1:
        result.detail = "the run reported no interface coverage"
        return result
    assigned = coverage[0].number("operations_assigned")
    reached = coverage[0].number("operations_reached")
    if assigned is None or reached is None:
        result.detail = "the coverage record is missing a count"
        return result
    # The count and the list come from one bitmap; a count that disagrees with
    # the list means one of them is not being derived from what ran.
    if assigned - reached != len(untouched):
        result.detail = (
            f"the count says {reached} of {assigned} reached, which does not agree with the "
            f"{len(untouched)} operation(s) named as untouched"
        )
        return result

    names = {str(record.get("name")) for record in untouched}
    missed = sorted(K3_REQUIRED_OPERATIONS & names)
    if missed:
        result.detail = (
            f"{len(missed)} operation(s) this phase's claims rest on were never reached: "
            + ", ".join(missed)
        )
        return result

    result.passed = True
    result.detail = (
        f"{reached} of {assigned} operations reached, including all "
        f"{len(K3_REQUIRED_OPERATIONS)} the K3 claims rest on; the {len(untouched)} untouched "
        "are named individually and belong to paths this package does not walk"
    )
    result.evidence = [line_of(coverage[0])]
    return result


def check_surprises(records: list[Record], run: dict) -> Result:
    result = Result("surprises", "ningún programa vio algo que no esperaba")
    if run.get("exit_status") != EXIT_COMPLETE:
        result.detail = (
            f"the run exited with {run.get('exit_status')}, not the completion status "
            f"{EXIT_COMPLETE}"
        )
        return result
    if run.get("timed_out"):
        result.detail = "the run was stopped by its timeout rather than finishing"
        return result

    unexpected = notes(records, "unexpected")
    not_refused = notes(records, "not_refused")
    failed = notes(records, "build_failed")
    if failed:
        result.detail = f"a build step failed: {line_of(failed[0])}"
        return result
    if not_refused:
        result.detail = (
            f"{len(not_refused)} operation(s) that had to be refused were permitted: "
            + line_of(not_refused[0])
        )
        return result
    if unexpected:
        result.detail = (
            f"{len(unexpected)} unexpected status(es) were reported, the first being "
            + line_of(unexpected[0])
        )
        return result

    controls = notes(records, "refused_as_expected")
    if len(controls) < 10:
        result.detail = (
            f"only {len(controls)} negative controls were reported; a run with no refusals "
            "to report has not stood on any boundary"
        )
        return result
    done = notes(records, "done")
    if len(done) < 4:
        result.detail = f"{len(done)} programs reached the end of their own script"
        return result

    # The kernel survived everything it contained.
    panics = [record for record in records if record.event.startswith("panic")]
    if panics:
        result.detail = f"the kernel panicked: {line_of(panics[0])}"
        return result

    result.passed = True
    result.detail = (
        f"the run exited {EXIT_COMPLETE} with {len(controls)} refusals reported as expected, "
        f"{len(done)} programs reaching the end of their script, and no unexpected status "
        "anywhere"
    )
    result.evidence = [f"exit_status={run.get('exit_status')} wall_seconds={run.get('wall_seconds')}"]
    return result


def check_k1_regression(records: list[Record], run: dict, k1: dict | None) -> Result:
    result = Result("k1", "regresión K1 verde con el sustrato K3")
    return _regression(result, k1, "K1")


def check_k2_regression(records: list[Record], run: dict, k2: dict | None) -> Result:
    result = Result("k2", "regresión K2 verde con el sustrato K3")
    return _regression(result, k2, "K2")


def _regression(result: Result, verdict: dict | None, gate: str) -> Result:
    if verdict is None:
        result.detail = (
            f"no {gate} verdict was found; run tools/check_{gate.lower()}.py "
            f"--json build/{gate.lower()}-gate.json"
        )
        return result
    if verdict.get("gate") != gate:
        result.detail = f"the verdict found is for {verdict.get('gate')}, not {gate}"
        return result
    if not verdict.get("passed"):
        failed = [c["name"] for c in verdict.get("criteria", []) if not c.get("passed")]
        result.detail = f"the {gate} gate failed: {', '.join(failed)}"
        return result
    criteria = verdict.get("criteria", [])
    result.passed = True
    result.detail = f"the {gate} gate passed all {len(criteria)} criteria against the same kernel"
    result.evidence = [f"{gate} criteria met: {len(criteria)}"]
    return result


def check_reproducible(records: list[Record], run: dict, manifest: dict | None) -> Result:
    result = Result("reproducible", "evidencia reproducible")
    if manifest is None:
        result.detail = "no image manifest was found next to the run"
        return result
    sequence = [record.seq for record in records]
    if not sequence:
        result.detail = "the log contains no records"
        return result
    if sequence[0] != 0:
        result.detail = f"the record sequence starts at {sequence[0]}, not 0"
        return result
    gaps = [b for a, b in zip(sequence, sequence[1:]) if b != a + 1]
    if gaps:
        result.detail = f"the record sequence has {len(gaps)} gap(s), first before seq {gaps[0]}"
        return result

    if manifest.get("phase") != "k3":
        result.detail = f"the manifest describes phase {manifest.get('phase')}, not k3"
        return result
    image = manifest.get("artifacts", {}).get("image", {})
    if not image.get("sha256"):
        result.detail = "the manifest records no image digest"
        return result
    ran = Path(run.get("image", ""))
    if ran.exists():
        digest = hashlib.sha256(ran.read_bytes()).hexdigest()
        if digest != image["sha256"]:
            result.detail = "the image that was run is not the image in the manifest"
            return result

    # The platform the run actually used, not the one it meant to use. A
    # multiprocessor gate read from a uniprocessor run is the mistake worth
    # making impossible.
    for key in ("processors", "machine", "accelerator", "cpu", "virtio", "iommu"):
        if not run.get(key):
            result.detail = f"the run record does not say what {key} it used"
            return result
    processors = run.get("processors")
    online = online_cpus(records)
    if online != processors:
        result.detail = (
            f"the run was configured with {processors} processors and {online} came online"
        )
        return result

    result.passed = True
    result.detail = (
        f"{len(sequence)} records with no gap; run traced to image {image['sha256'][:16]} on "
        f"{run.get('machine')}/{run.get('accelerator')} with {processors} processors, "
        f"{run.get('virtio')}, iommu {run.get('iommu')}"
    )
    result.evidence = [
        f"image sha256={image['sha256']}",
        f"kernel sha256={manifest['artifacts']['kernel']['sha256']}",
        f"rust={manifest.get('rust')}",
        f"cpu={run.get('cpu')} smp={processors} profile={run.get('profile')}",
    ]
    return result


CRITERIA = [
    check_smp,
    check_identity,
    check_absent,
    check_dispatch,
    check_simultaneity,
    check_clock,
    check_budget,
    check_debt,
    check_migration,
    check_shootdown,
    check_probe,
    check_seal,
    check_quarantine,
    check_barrier,
    check_device,
    check_regions,
    check_irq,
    check_dma,
    check_blockio,
    check_validator,
    check_reset,
    check_authority,
    check_outstanding,
    check_coverage,
    check_surprises,
]


def drop_event(event: str):
    def mutate(lines: list[str]) -> list[str]:
        return [line for line in lines if f" {event} " not in line]

    return mutate


def rewrite(pattern: str, replacement: str):
    def mutate(lines: list[str]) -> list[str]:
        return [line.replace(pattern, replacement) for line in lines]

    return mutate


def rewrite_re(pattern: str, replacement: str):
    """Rewrites by pattern, so a mutation does not depend on a run's numbers.

    A self-test whose damage is spelled out literally stops damaging anything
    the moment a counter changes, and then reports success for a check it never
    made.
    """
    compiled = re.compile(pattern)

    def mutate(lines: list[str]) -> list[str]:
        return [compiled.sub(replacement, line) for line in lines]

    return mutate


# (name, mutation, criterion that must fail)
MUTATIONS = [
    ("no bring-up summary", drop_event("smp.summary"), "smp"),
    (
        "a processor described and never started",
        rewrite_re(r"described=(\d+) started=\d+", r"described=\1 started=0"),
        "smp",
    ),
    (
        "a processor that answered an identity it was not asked for",
        rewrite_re(r"identity_match=\d+", "identity_match=0"),
        "identity",
    ),
    (
        "an application processor that came up without SMEP",
        rewrite_re(r"smep=\d+ smap=\d+", "smep=0 smap=1"),
        "identity",
    ),
    ("no absent-processor control", drop_event("smp.absent_result"), "absent"),
    (
        "an absent processor that answered",
        rewrite_re(r"answered=\d+ online_before", "answered=1 online_before"),
        "absent",
    ),
    (
        "a failed start that freed the stack it had prepared",
        rewrite_re(r"stack_allocated=\d+", "stack_allocated=1"),
        "absent",
    ),
    (
        "a processor that dispatched nothing",
        rewrite_re(r"dispatches=\d+ preemptions", "dispatches=0 preemptions"),
        "dispatch",
    ),
    (
        "threads that never held a scope at the same instant",
        rewrite_re(r"peak_simultaneous_threads=\d+", "peak_simultaneous_threads=1"),
        "simultaneity",
    ),
    (
        "a scope that exceeded its own simultaneity ceiling",
        rewrite_re(r"parallelism=\d+ windows_closed", "parallelism=1 windows_closed"),
        "simultaneity",
    ),
    (
        "a clock that went backwards between processors",
        rewrite_re(r"clock_regressions=\d+", "clock_regressions=3"),
        "clock",
    ),
    (
        "a scope promised more than its budget",
        rewrite_re(
            r"budget_ns=(\d+) parallelism", r"budget_ns=1 parallelism"
        ),
        "budget",
    ),
    (
        "a budget nothing was ever refused against",
        rewrite_re(r"dispatch_refusals=\d+", "dispatch_refusals=0"),
        "budget",
    ),
    ("no window that closed over budget", drop_event("scope.debt"), "debt"),
    (
        "an overrun forgiven at the boundary",
        rewrite_re(r"carried_into_next=\d+", "carried_into_next=0"),
        "debt",
    ),
    ("no thread that changed processor", drop_event("sched.migrated"), "migration"),
    (
        "a migration that left the floating-point state behind",
        rewrite("fp_state=saved_and_restored", "fp_state=not_saved"),
        "migration",
    ),
    ("no withdrawal from a live address space", drop_event("mem.unmapped"), "shootdown"),
    (
        "a withdrawal one processor never acknowledged",
        rewrite_re(
            r"acknowledged_cpus=(\d+) expected_cpus=(\d+)",
            r"acknowledged_cpus=1 expected_cpus=\2",
        ),
        "shootdown",
    ),
    (
        "a space that had only ever run on one processor",
        rewrite_re(r"space_cpus=\d+", "space_cpus=1"),
        "shootdown",
    ),
    ("no fault after the translation was taken away", drop_event("user.fault"), "probe"),
    ("no seal at all", drop_event("mem.sealed"), "seal"),
    (
        "a seal that left a writable alias behind",
        rewrite_re(r"remaining_maps=\d+", "remaining_maps=1"),
        "seal",
    ),
    (
        "a seal whose writer had only ever run on one processor",
        rewrite_re(r"writer_cpus_ever=\d+", "writer_cpus_ever=1"),
        "seal",
    ),
    (
        "the sealed bytes read differently the second time",
        rewrite("a=0x3013 b=0x1", "a=0x3013 b=0x0"),
        "seal",
    ),
    (
        "frames released before every processor had invalidated",
        rewrite("condition=all_processors_invalidated", "condition=immediate"),
        "quarantine",
    ),
    (
        "a domain still holding frames at the end",
        rewrite_re(r"charged_frames=\d+ handles_held", "charged_frames=4 handles_held"),
        "quarantine",
    ),
    ("no barrier raised", drop_event("scope.fenced"), "barrier"),
    (
        "a scope retired before it was quiescent",
        rewrite("from_state=quiescent", "from_state=fenced"),
        "barrier",
    ),
    ("no device assigned", drop_event("device.assigned"), "device"),
    (
        "a device handed over already able to master the bus",
        rewrite_re(r"state=ready bus_master=\d+", "state=ready bus_master=1"),
        "device",
    ),
    (
        "a strong isolation profile claimed on a platform without one",
        rewrite_re(r"dma_profile=\d+", "dma_profile=2"),
        "device",
    ),
    (
        "a remapping unit the kernel claims to have programmed",
        rewrite_re(r"remapping_programmed=\d+", "remapping_programmed=1"),
        "device",
    ),
    (
        "a register window sharing a region with the interrupt table",
        rewrite_re(r"bar=(\d+) phys", r"bar=1 phys"),
        "regions",
    ),
    (
        "a register window mapped cacheable",
        rewrite_re(r"cacheable=\d+", "cacheable=1"),
        "regions",
    ),
    ("no window taken back by name", drop_event("device.region_unmapped"), "regions"),
    ("no interrupt delivered", drop_event("device.interrupt"), "irq"),
    (
        "an interrupt table entry the driver wrote itself",
        rewrite("table_written_by=kernel", "table_written_by=driver"),
        "irq",
    ),
    (
        "interrupts that arrived for no binding",
        rewrite_re(r"interrupts_unclaimed=\d+", "interrupts_unclaimed=2"),
        "irq",
    ),
    (
        "a weak grant described as enforced by hardware",
        rewrite("enforced_by=nothing_driver_is_trusted", "enforced_by=iommu"),
        "dma",
    ),
    ("nobody ever asked for the strong profile", drop_event("device.profile_refused"), "dma"),
    (
        "a strong-profile refusal reported to the driver as some other error",
        rewrite("a=0x300b b=0xffffffffffffffeb", "a=0x300b b=0xfffffffffffffffe"),
        "dma",
    ),
    (
        "a block request that completed with a device error",
        rewrite_re(r"a=0x300d b=0x(\w+)", r"a=0x300d b=0x101"),
        "blockio",
    ),
    (
        "a transport that never finished negotiating",
        rewrite("a=0x300c b=0xf", "a=0x300c b=0x7"),
        "blockio",
    ),
    (
        "a validator that refused nothing",
        rewrite("a=0x3015 b=0x3", "a=0x3015 b=0x0"),
        "validator",
    ),
    (
        "three rejections that all had the same reason",
        rewrite_re(r"a=0x300e b=0x\w+", "a=0x300e b=0x1"),
        "validator",
    ),
    ("no reset by recovery authority", drop_event("device.reset"), "reset"),
    (
        "a reset confirmed by nothing but its own return",
        rewrite("confirmed_by=transport_status_zero", "confirmed_by=assumed"),
        "reset",
    ),
    (
        "a reset that left the device able to master the bus",
        rewrite_re(r"status_after_reset=0x0 bus_master=\d+", "status_after_reset=0x0 bus_master=1"),
        "reset",
    ),
    (
        "a driver that was never refused anything",
        drop_event("k2.refused"),
        "authority",
    ),
    (
        "a DMA grant that outlived the run",
        rewrite_re(r"grants_outstanding=\d+", "grants_outstanding=1"),
        "outstanding",
    ),
    (
        "a register window left naming a session that had ended",
        rewrite_re(r"stale_maps=\d+", "stale_maps=1"),
        "outstanding",
    ),
    ("no coverage record at all", drop_event("k2.coverage"), "coverage"),
    (
        "a coverage count that disagrees with its own list",
        rewrite_re(
            r"operations_assigned=(\d+) operations_reached=\d+",
            r"operations_assigned=\1 operations_reached=0",
        ),
        "coverage",
    ),
    (
        "a device operation the run never reached",
        rewrite("sched.cpu_summary cpu=0", "k2.operation_untouched name=DEVICE_RESET cpu=0"),
        "coverage",
    ),
    (
        "an operation reported as permitted that had to be refused",
        rewrite("a=0x2007 b=", "a=0x2008 b="),
        "surprises",
    ),
    ("a gap in the record sequence", drop_event("sched.preempt"), "reproducible"),
]


def self_test(
    text: str, run: dict, manifest: dict | None, k1: dict | None, k2: dict | None, quiet: bool
) -> int:
    """Damages the log one way at a time and reports which criteria noticed."""
    lines = text.splitlines()
    clean = parse(text)
    by_name = {check(clean, run).name: check for check in CRITERIA}
    by_name["reproducible"] = lambda r, u: check_reproducible(r, u, manifest)
    by_name["k1"] = lambda r, u: check_k1_regression(r, u, k1)
    by_name["k2"] = lambda r, u: check_k2_regression(r, u, k2)

    baseline = [check(clean, run) for check in CRITERIA]
    baseline.append(check_k1_regression(clean, run, k1))
    baseline.append(check_k2_regression(clean, run, k2))
    baseline.append(check_reproducible(clean, run, manifest))
    if any(not result.passed for result in baseline):
        broken = [result.name for result in baseline if not result.passed]
        print(
            f"self-test needs a passing run to damage; this one already fails: {', '.join(broken)}",
            file=sys.stderr,
        )
        return 2

    width = max(len(name) for name, _, _ in MUTATIONS)
    missed = []
    for name, mutate, criterion in MUTATIONS:
        damaged = parse("\n".join(mutate(lines)))
        check = by_name.get(criterion)
        if check is None:
            missed.append((name, criterion, "no such criterion"))
            continue
        result = check(damaged, run)
        caught = not result.passed
        if not quiet:
            mark = "CAUGHT" if caught else "MISSED"
            print(f"{mark}  {name.ljust(width)}  -> {criterion}: {result.detail}")
        if not caught:
            missed.append((name, criterion, result.detail))

    print()
    if missed:
        print(f"K3 GATE SELF-TEST FAILED: {len(missed)} of {len(MUTATIONS)} damaged runs still passed")
        return 1
    print(f"K3 GATE SELF-TEST PASSED: {len(MUTATIONS)} of {len(MUTATIONS)} damaged runs were caught")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=Path, default=ROOT / "build/run-k3")
    parser.add_argument("--manifest", type=Path, default=ROOT / "build/image-manifest-k3.json")
    parser.add_argument("--k1", type=Path, default=ROOT / "build/k1-gate.json")
    parser.add_argument("--k2", type=Path, default=ROOT / "build/k2-gate.json")
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    parser.add_argument("--quiet", action="store_true", help="print only the verdict line")
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="damage the run one way at a time and check that a criterion notices each",
    )
    arguments = parser.parse_args()

    log = arguments.run / "serial.log"
    record_path = arguments.run / "run.json"
    if not log.exists():
        print(f"serial log not found: {log}; run tools/run_k3.py", file=sys.stderr)
        return 2
    if not record_path.exists():
        print(f"run record not found: {record_path}; run tools/run_k3.py", file=sys.stderr)
        return 2

    text = log.read_text(errors="replace")
    records = parse(text)
    run = json.loads(record_path.read_text())
    manifest = json.loads(arguments.manifest.read_text()) if arguments.manifest.exists() else None
    k1 = json.loads(arguments.k1.read_text()) if arguments.k1.exists() else None
    k2 = json.loads(arguments.k2.read_text()) if arguments.k2.exists() else None

    if arguments.self_test:
        return self_test(text, run, manifest, k1, k2, arguments.quiet)

    results = [check(records, run) for check in CRITERIA]
    results.append(check_k1_regression(records, run, k1))
    results.append(check_k2_regression(records, run, k2))
    results.append(check_reproducible(records, run, manifest))

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
        "gate": "K3",
        "passed": not failed,
        "criteria": [
            {"name": r.name, "title": r.title, "passed": r.passed, "detail": r.detail}
            for r in results
        ],
        "records": len(records),
        "serial_log": str(log),
        "profile": run.get("profile"),
        "processors": run.get("processors"),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")

    print()
    if failed:
        print(f"K3 GATE FAILED: {len(failed)} of {len(results)} criteria not met")
        return 1
    print(f"K3 GATE PASSED: {len(results)} of {len(results)} criteria met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
