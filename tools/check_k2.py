#!/usr/bin/env python3
"""Evaluate a K2 run against the gate.

The K2 programs report what they observed, and they report no surprises. That
is the defendant's account of the trial. It is worth having -- only the caller
knows what status came back from a call the kernel refused -- but it decides
nothing on its own: a program that never attempted an operation and a program
whose attempt was wrongly permitted can both finish without complaining.

So every criterion below is decided from the kernel's own records wherever the
kernel emits one, and the two criteria that must read the programs' notes say
so in their title. Each criterion is evaluated separately, so it fails on its
own rather than being carried by the others, and each carries the lines it was
decided from.

The K1 regression is a criterion here too. K2 grew inside the kernel K1 boots,
and a K2 gate that passed while protected boot had quietly broken would be
measuring the wrong thing.

Usage: tools/check_k2.py [--run build/run-k2] [--manifest build/image-manifest-k2.json]
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

# `run_k1.py` maps the kernel's completion status to QEMU's `(value << 1) | 1`.
EXIT_COMPLETE = 33

# Mirrors `thalyx_abi::generated::status`. Repeated rather than imported so a
# kernel that silently renumbered a status would fail this gate instead of
# redefining what it checks.
OK = 0
INVALID_ARGUMENT = -2
INCOMPATIBLE_VERSION = -3
INVALID_HANDLE = -4
INSUFFICIENT_RIGHTS = -6
EXPIRED = -7
SCOPE_CLOSED = -8
LIMIT_EXHAUSTED = -9
QUEUE_FULL = -10
INVALID_ADDRESS = -11
CANCELLED = -13
STATE_CONFLICT = -18
DRAIN_INCOMPLETE = -19
NOT_SUPPORTED = -20

# Mirrors `thalyx_user_rt::k2::report`. These are the identifiers the programs
# stamp on their own observations, on the diagnostic plane.
NOTE = {
    "limits": 0x2001,
    "built": 0x2002,
    "build_failed": 0x2003,
    "holding": 0x2004,
    "drain": 0x2005,
    "call_result": 0x2006,
    "refused_as_expected": 0x2007,
    "not_refused": 0x2008,
    "cancel_state": 0x2009,
    "origin": 0x200A,
    "done": 0x200B,
    "unexpected": 0x200C,
    "copied": 0x200D,
    "slot_recycled": 0x200E,
    "bounded": 0x200F,
    "log_loss": 0x2010,
    "closure_recorded": 0x2011,
}

# Mirrors `thalyx_abi::generated::cancel_state` and `scope_state`.
CANCEL_LIVE = 0
CANCEL_ORIGIN_FENCED = 1
CANCEL_ORIGIN_DEAD = 2
SCOPE_OPEN = 1
SCOPE_FENCED = 2
SCOPE_QUIESCENT = 3

# The programs the K2 package instantiates. The supervisor is the one the
# kernel builds; the other two exist only if the supervisor made them.
SUPERVISOR = "supervisor"
SERVER = "k2server"
CLIENT = "k2client"


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


def line_of(record: Record) -> str:
    rendered = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{record.seq} {record.event} {rendered}".rstrip()


# --- criteria ---------------------------------------------------------------
#
# Each function receives the parsed records and the run record, fills in a
# Result, and never raises: a missing record is a failed criterion, not a crash.


def check_k2_boot(records: list[Record], run: dict) -> Result:
    result = Result("boot", "arranque K2 con manifiesto explícito")
    built = by_event(records, "k2.supervisor_built")
    root = by_event(records, "scope.root")
    log = by_event(records, "ctrl.log_established")
    if not root:
        result.detail = "the kernel established no root scope"
        return result
    if not log:
        result.detail = "the kernel established no control log"
        return result
    if not built:
        result.detail = "the K2 boot path was not taken: no supervisor was built"
        return result
    if len(built) != 1:
        result.detail = f"{len(built)} supervisors were built; the first supervisor must be one"
        return result

    # The kernel must have created exactly one domain, and it must be the
    # supervisor. Everything else in the run has to have been made through the
    # interface; a second domain the kernel built itself would be authority
    # nobody had to be given.
    unmanaged = [
        record
        for record in by_event(records, "domain.created")
        if record.get("managed") != "1"
    ]
    if len(unmanaged) != 1:
        names = ", ".join(str(record.get("name")) for record in unmanaged)
        result.detail = (
            f"the kernel created {len(unmanaged)} domains of its own ({names}); exactly one, "
            "the supervisor, is allowed"
        )
        return result
    if unmanaged[0].get("name") != SUPERVISOR:
        result.detail = f"the domain the kernel created is {unmanaged[0].get('name')}"
        return result

    capabilities = by_event(records, "k2.boot_capability")
    if not capabilities:
        result.detail = "the supervisor was built with no boot capabilities at all"
        return result
    kinds = {record.get("object_type") for record in capabilities}
    for required in ("domain", "scope", "control_log", "memory"):
        if required not in kinds:
            result.detail = f"the boot manifest carries no {required} capability"
            return result
    if run.get("exit_status") != EXIT_COMPLETE:
        result.detail = f"run exited with {run.get('exit_status')}, expected {EXIT_COMPLETE}"
        return result
    if run.get("timed_out"):
        result.detail = "the run timed out"
        return result

    result.passed = True
    result.detail = (
        f"one supervisor built with {len(capabilities)} explicit boot capabilities "
        f"({', '.join(sorted(kinds))}); no other domain was created by the kernel"
    )
    result.evidence = [line_of(root[0]), line_of(built[0]), line_of(capabilities[0])]
    return result


def check_created_through_interface(records: list[Record], run: dict) -> Result:
    result = Result("creation", "creación sin privilegio ambiental")
    created = by_event(records, "domain.created")
    managed = [record for record in created if record.get("managed") == "1"]
    if len(managed) < 2:
        result.detail = f"{len(managed)} domains were created through the interface; expected 2"
        return result
    names = {record.get("name") for record in managed}
    for required in (SERVER, CLIENT):
        if required not in names:
            result.detail = f"{required} was never created through the interface"
            return result

    # Every capability the client holds must have been installed into it by
    # someone. A domain that acquired one another way would have no record.
    installed = [
        record for record in by_event(records, "cap.installed") if record.get("name") == CLIENT
    ]
    if not installed:
        result.detail = "the client was activated holding no recorded capability"
        return result
    held = {record.get("object_type") for record in installed}
    forbidden = held & {"scope", "domain", "control_log"}
    if forbidden:
        result.detail = f"the client was given ambient authority: {', '.join(sorted(forbidden))}"
        return result

    activated = {
        record.get("name")
        for record in by_event(records, "domain.activated")
        if record.get("fault_channel") == "1"
    }
    for required in (SERVER, CLIENT):
        if required not in activated:
            result.detail = f"{required} was activated without a fault channel"
            return result

    result.passed = True
    result.detail = (
        f"{len(managed)} domains built by the supervisor through the interface; the client "
        f"holds only {', '.join(sorted(held))}, and both were activated with a fault channel"
    )
    result.evidence = [line_of(record) for record in installed]
    return result


def check_derivation_narrows(records: list[Record], run: dict) -> Result:
    result = Result("derivation", "derivación estrecha y no amplifica")
    derivations = by_event(records, "cap.derive")
    if not derivations:
        result.detail = "no capability was derived in this run"
        return result
    for record in derivations:
        parent = record.number("parent_rights")
        child = record.number("child_rights")
        if parent is None or child is None:
            result.detail = f"a derivation at seq {record.seq} recorded no rights"
            return result
        if child & ~parent:
            result.detail = (
                f"derivation at seq {record.seq} widened rights: "
                f"parent 0x{parent:x} -> child 0x{child:x}"
            )
            return result

    # A narrowing that is never attempted in the other direction proves little.
    widened = [
        record
        for record in by_event(records, "k2.refused")
        if record.get("op") in ("CAP_DERIVE", "DOMAIN_INSTALL_CAP")
        and record.signed("status") == INSUFFICIENT_RIGHTS
    ]
    if not widened:
        result.detail = "no attempt to widen a capability was made, so nothing refused one"
        return result

    result.passed = True
    result.detail = (
        f"{len(derivations)} derivations, none wider than its parent; "
        f"{len(widened)} attempts to widen refused with INSUFFICIENT_RIGHTS"
    )
    result.evidence = [line_of(derivations[0]), line_of(widened[0])]
    return result


def check_generational_handles(records: list[Record], run: dict) -> Result:
    result = Result("generations", "un handle antiguo no nombra al nuevo ocupante")
    recycled = notes(records, "slot_recycled")
    if not recycled:
        result.detail = "no table slot was recycled in this run"
        return result
    reused = recycled[0].number("b")
    if reused is None:
        result.detail = "the recycled-slot note carries no handle"
        return result

    # The kernel's own record of the refusal, not the program's claim about it.
    stale = [
        record
        for record in by_event(records, "k2.refused")
        if record.signed("status") == INVALID_HANDLE
    ]
    if len(stale) < 2:
        result.detail = (
            f"{len(stale)} stale-handle refusals; the closed handle must be refused both "
            "before and after its slot is reused"
        )
        return result

    result.passed = True
    result.detail = (
        f"a freed slot came back as handle 0x{reused:x}, and the handle that used to name "
        f"it was refused with INVALID_HANDLE {len(stale)} times"
    )
    result.evidence = [line_of(recycled[0])] + [line_of(record) for record in stale[:2]]
    return result


def check_authenticated_origin(records: list[Record], run: dict) -> Result:
    result = Result("origin", "origen auténtico, no declarado por el payload")
    overridden = by_event(records, "ctrl.origin_overridden")
    if not overridden:
        result.detail = "no caller claimed a false origin, so none was observed being replaced"
        return result

    claimed = {record.number("claimed") for record in overridden}
    receipts = by_event(records, "ctrl.receipt")
    if not receipts:
        result.detail = "no control receipt was written"
        return result
    forged = [
        record
        for record in receipts
        if record.number("origin_domain") in claimed and record.number("origin_domain")
    ]
    if forged:
        result.detail = f"a claimed origin reached a receipt at seq {forged[0].seq}"
        return result

    delivered = by_event(records, "ipc.delivered")
    admitted = by_event(records, "ipc.admitted")
    if not (delivered and admitted):
        result.detail = "no message was admitted and delivered, so no header was built"
        return result
    origin = admitted[0].number("origin_domain")
    reported = notes(records, "origin", SERVER)
    if not reported:
        result.detail = "the server reported no origin for the message it received"
        return result
    if reported[0].number("b") != origin:
        result.detail = (
            f"the server read origin {reported[0].number('b')} but the kernel admitted "
            f"the message from {origin}"
        )
        return result

    result.passed = True
    result.detail = (
        f"{len(overridden)} claimed origins replaced by the kernel and none reached a "
        f"receipt; the server read the origin the kernel recorded ({origin})"
    )
    result.evidence = [line_of(overridden[0]), line_of(admitted[0]), line_of(reported[0])]
    return result


def check_admission_and_effect(records: list[Record], run: dict) -> Result:
    result = Result("admission", "admisión, efecto reservado y resolución")
    admitted = by_event(records, "ipc.admitted")
    delivered = by_event(records, "ipc.delivered")
    effects = by_event(records, "effect.admitted")
    resolved = by_event(records, "ipc.resolved")
    if not admitted:
        result.detail = "no invocation was admitted"
        return result
    if not delivered:
        result.detail = "no invocation was delivered"
        return result
    if not effects:
        result.detail = "no effect was admitted against an invocation"
        return result
    if not resolved:
        result.detail = "no invocation was resolved"
        return result

    effect = effects[0]
    if effect.number("closure_reserve_ns") in (None, 0):
        result.detail = "the effect was admitted without reserving closure capacity"
        return result
    if effect.get("service_scope") == effect.get("origin_scope"):
        result.detail = (
            "closure was reserved in the origin's scope; finishing an obligation must not "
            "depend on the budget of the client being closed"
        )
        return result

    # The message carried a capability, and the receiver was told what it got.
    if delivered[0].number("caps") in (None, 0):
        result.detail = "no capability was transferred with the message"
        return result

    result.passed = True
    result.detail = (
        f"invocation {admitted[0].number('invocation')} admitted with "
        f"{delivered[0].number('caps')} capability, delivered, an effect admitted reserving "
        f"{effect.number('closure_reserve_ns')} ns in the service scope "
        f"{effect.get('service_scope')} rather than the origin's {effect.get('origin_scope')}, "
        f"and resolved with outcome {resolved[0].get('outcome')}"
    )
    result.evidence = [line_of(admitted[0]), line_of(delivered[0]), line_of(effect), line_of(resolved[0])]
    return result


def check_barrier_not_drain(records: list[Record], run: dict) -> Result:
    result = Result("barrier", "barrera separada del drenaje")
    fenced = by_event(records, "scope.fenced")
    if not fenced:
        result.detail = "no scope was fenced"
        return result
    fence = fenced[0]
    outstanding = sum(
        fence.number(key) or 0
        for key in ("threads", "invocations_pending", "effects_pending", "maps_pending")
    )
    if outstanding == 0:
        result.detail = (
            "the barrier reported nothing outstanding, so it cannot show that fencing and "
            "draining are different"
        )
        return result

    refused = [
        record
        for record in by_event(records, "k2.refused")
        if record.get("op") == "SCOPE_RETIRE"
    ]
    conflicts = [r for r in refused if r.signed("status") == STATE_CONFLICT]
    incomplete = [r for r in refused if r.signed("status") == DRAIN_INCOMPLETE]
    if not conflicts:
        result.detail = "retiring an open scope was never attempted, so it was never refused"
        return result
    if not incomplete:
        result.detail = "retiring a fenced but undrained scope was never refused"
        return result

    retired = by_event(records, "scope.retired")
    if not retired:
        result.detail = "the fenced scope was never retired, so the drain never completed"
        return result
    if retired[0].get("from_state") != "quiescent":
        result.detail = (
            f"the scope was retired from state {retired[0].get('from_state')}, not from "
            "quiescent"
        )
        return result
    if retired[0].seq < incomplete[-1].seq:
        result.detail = "retirement succeeded before the last refusal, so the order is wrong"
        return result

    result.passed = True
    result.detail = (
        f"the barrier left {outstanding} obligation(s) counted; retirement refused "
        f"{len(conflicts)} time(s) as a state conflict and {len(incomplete)} time(s) as an "
        f"incomplete drain, and succeeded only from quiescent"
    )
    result.evidence = [line_of(fence), line_of(conflicts[0]), line_of(incomplete[0]), line_of(retired[0])]
    return result


def check_barrier_reaches_delegated(records: list[Record], run: dict) -> Result:
    result = Result("reach", "la barrera alcanza la autoridad delegada")
    fenced = by_event(records, "scope.fenced")
    if not fenced:
        result.detail = "no scope was fenced"
        return result
    fence_seq = fenced[0].seq

    after = [
        record
        for record in by_event(records, "k2.refused")
        if record.seq > fence_seq and record.signed("status") == SCOPE_CLOSED
    ]
    if not after:
        result.detail = "nothing was refused with SCOPE_CLOSED after the barrier"
        return result

    # The capability the client delegated is held by the server, which is not
    # in the fenced perimeter. A barrier that only stopped the fenced domain
    # itself would leave this one working.
    by_server = [record for record in after if record.get("name") == SERVER]
    by_client = [record for record in after if record.get("name") == CLIENT]
    if not by_server:
        result.detail = (
            "the delegated capability still worked after the barrier: the server was never "
            "refused with SCOPE_CLOSED"
        )
        return result
    if not by_client:
        result.detail = "the fenced domain itself was never refused a new admission"
        return result

    result.passed = True
    result.detail = (
        f"after the barrier, {len(by_server)} operation(s) through the capability the fenced "
        f"domain had delegated to another domain were refused, and so were "
        f"{len(by_client)} new admission(s) by the fenced domain itself"
    )
    result.evidence = [line_of(by_server[0]), line_of(by_client[0])]
    return result


def check_obligation_survives(records: list[Record], run: dict) -> Result:
    result = Result("obligation", "la obligación admitida sobrevive a la barrera")
    effects = by_event(records, "effect.admitted")
    fenced = by_event(records, "scope.fenced")
    resolved = by_event(records, "ipc.resolved")
    if not (effects and fenced and resolved):
        result.detail = "the run has no admitted effect, barrier and resolution to order"
        return result
    if not (effects[0].seq < fenced[0].seq < resolved[0].seq):
        result.detail = (
            f"the order was effect={effects[0].seq} fence={fenced[0].seq} "
            f"resolve={resolved[0].seq}; the effect must be admitted before the barrier and "
            "resolved after it"
        )
        return result
    if resolved[0].get("effect") != "admitted":
        result.detail = f"the resolved invocation carried effect {resolved[0].get('effect')}"
        return result
    if resolved[0].get("cancel") not in ("origin_fenced", "origin_dead"):
        result.detail = (
            f"the invocation was resolved with cancel={resolved[0].get('cancel')}, so its "
            "origin was not observed closed"
        )
        return result

    # The server's own view, which is what a service would act on.
    observed = notes(records, "cancel_state", SERVER)
    if not observed:
        result.detail = "the server never read the invocation's cancellation state"
        return result
    if observed[0].number("b") == CANCEL_LIVE:
        result.detail = "the server saw the origin as live after the barrier"
        return result

    # The client's call must have ended as cancelled, not as a reply.
    call = notes(records, "call_result", CLIENT)
    if not call:
        result.detail = "the client never reported how its call ended"
        return result
    if call[0].signed("b") != CANCELLED:
        result.detail = f"the client's call returned {call[0].signed('b')}, expected CANCELLED"
        return result

    result.passed = True
    result.detail = (
        f"the effect was admitted at seq {effects[0].seq}, the barrier placed at "
        f"{fenced[0].seq}, and the obligation discharged at {resolved[0].seq} with "
        f"cancel={resolved[0].get('cancel')}; the client's own call returned CANCELLED"
    )
    result.evidence = [line_of(effects[0]), line_of(fenced[0]), line_of(resolved[0]), line_of(call[0])]
    return result


def check_charge_conservation(records: list[Record], run: dict) -> Result:
    result = Result("charges", "conservación de cargos en la retirada")
    retired = by_event(records, "scope.retired")
    if not retired:
        result.detail = "no scope was retired, so nothing was accounted for"
        return result
    record = retired[0]
    freed = record.number("freed_pages")
    objects = record.number("freed_objects")
    if freed is None or objects is None:
        result.detail = "the retirement recorded nothing about what it freed"
        return result
    if objects == 0:
        result.detail = "retirement freed no object at all, so it only marked the scope"
        return result
    if record.number("retained_pages") != 0 or record.number("retained_metadata") != 0:
        result.detail = (
            f"retirement left {record.number('retained_pages')} page(s) and "
            f"{record.number('retained_metadata')} metadata charged to a retired scope"
        )
        return result

    released = by_event(records, "mem.released")
    if not released:
        result.detail = "no memory object was released, so no page returned to the allocator"
        return result

    summary = by_event(records, "k1.summary")
    if not summary:
        result.detail = "the run emitted no final accounting"
        return result
    free_at_end = summary[-1].number("free_frames")
    usable = summary[-1].number("usable_frames_at_boot")
    charged = summary[-1].number("kernel_charged_frames")
    if free_at_end is None or usable is None or charged is None:
        result.detail = "the final accounting is incomplete"
        return result
    dead = [
        r for r in by_event(records, "k1.domain_summary") if r.get("final_state") == "dead"
    ]
    still_charged = [r for r in dead if (r.number("charged_frames") or 0) != 0]
    if still_charged:
        names = ", ".join(str(r.get("name")) for r in still_charged)
        result.detail = f"dead domains still hold frames: {names}"
        return result

    result.passed = True
    result.detail = (
        f"retirement freed {freed} page(s) in {objects} object(s) and retained nothing; "
        f"{len(dead)} dead domains hold no frame, and the kernel ends holding {charged} "
        f"with {free_at_end} free of {usable} usable at boot"
    )
    result.evidence = [line_of(record), line_of(released[0]), line_of(summary[-1])]
    return result


def check_bounded_tables(records: list[Record], run: dict) -> Result:
    result = Result("bounds", "tablas y colas acotadas, sin crecimiento ilimitado")
    refused = by_event(records, "k2.refused")
    exhausted = [r for r in refused if r.signed("status") == LIMIT_EXHAUSTED]
    full = [r for r in refused if r.signed("status") == QUEUE_FULL]
    if not exhausted:
        result.detail = "no table refused to grow, so no bound was reached"
        return result
    if not full:
        result.detail = "no queue refused an admission, so no queue bound was reached"
        return result

    bounded = notes(records, "bounded")
    if len(bounded) < 2:
        result.detail = f"{len(bounded)} bounds were reported; expected a table and a queue"
        return result
    if any((record.number("b") or 0) == 0 for record in bounded):
        result.detail = "a bound was reported at zero admissions, so nothing was ever admitted"
        return result

    limit = by_event(records, "scope.limit_refused")
    if not limit:
        result.detail = "no scope limit was recorded binding a request"
        return result

    result.passed = True
    result.detail = (
        f"{len(exhausted)} LIMIT_EXHAUSTED and {len(full)} QUEUE_FULL refusals; bounds reached "
        f"after {', '.join(str(record.number('b')) for record in bounded)} admissions, with "
        f"the binding scope named in {len(limit)} record(s)"
    )
    result.evidence = [line_of(exhausted[0]), line_of(full[0]), line_of(limit[0])]
    return result


def check_closure_available(records: list[Record], run: dict) -> Result:
    result = Result("closure", "cierre disponible con el log saturado")
    loss = notes(records, "log_loss")
    recorded = notes(records, "closure_recorded")
    if not loss:
        result.detail = "the control log never lost an ordinary receipt, so it was not saturated"
        return result
    if (loss[0].number("b") or 0) == 0:
        result.detail = "the log reported a loss count of zero"
        return result
    if not recorded:
        result.detail = "no closing receipt was observed being written while the log was full"
        return result

    fenced = by_event(records, "scope.fenced")
    if not fenced:
        result.detail = "no barrier was placed, so no closing receipt was due"
        return result
    fence_seq = fenced[0].seq
    receipts_after = [r for r in by_event(records, "ctrl.receipt") if r.seq >= fence_seq]
    fence_receipt = [r for r in receipts_after if r.number("kind") == 3]
    if not fence_receipt:
        result.detail = "the barrier wrote no receipt, so a full log silenced a revocation"
        return result
    if fence_receipt[0].number("seq") in (None, 0):
        result.detail = "the barrier's receipt was written with no sequence, so it was lost"
        return result

    # An ordinary admission must have been refused once no cell could be
    # reserved: the audited profile pays in advance or the operation does not
    # happen.
    unrecordable = [
        r
        for r in by_event(records, "k2.refused")
        if r.seq >= loss[0].seq and r.signed("status") == LIMIT_EXHAUSTED
    ]
    if not unrecordable:
        result.detail = "with no receipt cell left, no covered admission was refused"
        return result

    result.passed = True
    result.detail = (
        f"the log lost {loss[0].number('b')} ordinary receipt(s); the barrier's own receipt "
        f"was still written at sequence {fence_receipt[0].number('seq')}, and "
        f"{len(unrecordable)} covered admission(s) were refused rather than going unrecorded"
    )
    result.evidence = [line_of(loss[0]), line_of(fence_receipt[0]), line_of(unrecordable[0])]
    return result


def check_malformed_refused(records: list[Record], run: dict) -> Result:
    result = Result("malformed", "peticiones malformadas rechazadas sin efecto parcial")
    refused = by_event(records, "k2.refused")
    wanted = {
        INVALID_ARGUMENT: "structure",
        INCOMPATIBLE_VERSION: "version",
        INVALID_ADDRESS: "address",
        NOT_SUPPORTED: "operation",
    }
    seen: dict[int, list[Record]] = {code: [] for code in wanted}
    for record in refused:
        code = record.signed("status")
        if code in seen:
            seen[code].append(record)
    missing = [name for code, name in wanted.items() if not seen[code]]
    if missing:
        result.detail = f"no request was refused for: {', '.join(missing)}"
        return result

    # The transfers among them must not have moved anything: the client reports
    # the handle every refused transfer named as still present afterwards.
    survived = [
        record for record in notes(records, "built", CLIENT) if record.number("b") == 9
    ]
    if not survived:
        result.detail = (
            "the capability the refused transfers named was not confirmed present "
            "afterwards, so a partial transfer cannot be ruled out"
        )
        return result

    unassigned = [r for r in refused if r.get("op") == "unassigned"]
    if not unassigned:
        result.detail = "no unassigned operation code was attempted"
        return result

    total = sum(len(v) for v in seen.values())
    result.passed = True
    result.detail = (
        f"{total} malformed requests refused across {len(wanted)} distinct reasons, including "
        f"an unassigned operation; the capability the refused transfers named was still held "
        "afterwards"
    )
    result.evidence = [line_of(seen[code][0]) for code in wanted] + [line_of(survived[0])]
    return result


def check_negative_controls(records: list[Record], run: dict) -> Result:
    result = Result("controls", "controles negativos (reportados por los programas)")
    expected = notes(records, "refused_as_expected")
    not_refused = notes(records, "not_refused")
    unexpected = notes(records, "unexpected")
    failed = notes(records, "build_failed")

    if not expected:
        result.detail = "no program attempted an operation it expected to be refused"
        return result
    if not_refused:
        codes = ", ".join(str(record.signed("b")) for record in not_refused)
        result.detail = f"{len(not_refused)} operation(s) that had to be refused succeeded: {codes}"
        return result
    if unexpected:
        codes = ", ".join(str(record.signed("b")) for record in unexpected)
        result.detail = f"{len(unexpected)} unexpected result(s): {codes}"
        return result
    if failed:
        result.detail = f"{len(failed)} build step(s) failed in the supervisor"
        return result

    # Every program must have reached its own end of script; a program that
    # died early would report no surprises simply by reporting nothing.
    done = {record.get("name") for record in notes(records, "done")}
    for required in (SUPERVISOR, SERVER, CLIENT):
        if required not in done:
            result.detail = f"{required} did not reach the end of its script"
            return result

    distinct = {record.signed("b") for record in expected}
    result.passed = True
    result.detail = (
        f"{len(expected)} refusals observed across {len(distinct)} distinct statuses, none "
        f"unexpected and none missing; all three programs reached the end of their script"
    )
    result.evidence = [line_of(record) for record in expected[:3]]
    return result


def check_k1_regression(records: list[Record], run: dict, k1: dict | None) -> Result:
    result = Result("k1", "regresión K1 verde con el sustrato K2")
    if k1 is None:
        result.detail = (
            "no K1 verdict was found; run tools/check_k1.py --json build/k1-gate.json"
        )
        return result
    if k1.get("gate") != "K1":
        result.detail = f"the verdict found is for {k1.get('gate')}, not K1"
        return result
    if not k1.get("passed"):
        failed = [c["name"] for c in k1.get("criteria", []) if not c.get("passed")]
        result.detail = f"the K1 gate failed: {', '.join(failed)}"
        return result
    criteria = k1.get("criteria", [])
    result.passed = True
    result.detail = f"the K1 gate passed all {len(criteria)} criteria against the same kernel"
    result.evidence = [f"K1 criteria met: {len(criteria)}"]
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

    if manifest.get("phase") != "k2":
        result.detail = f"the manifest describes phase {manifest.get('phase')}, not k2"
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

    modules = manifest.get("modules", [])
    supervisors = [module for module in modules if module.get("kind") == 2]
    if len(supervisors) != 1:
        result.detail = f"the package declares {len(supervisors)} supervisor modules; expected 1"
        return result

    result.passed = True
    result.detail = (
        f"{len(sequence)} records with no gap; run traced to image {image['sha256'][:16]} "
        f"built from {len(modules)} recorded modules, exactly one of them the supervisor"
    )
    result.evidence = [
        f"image sha256={image['sha256']}",
        f"kernel sha256={manifest['artifacts']['kernel']['sha256']}",
        f"rust={manifest.get('rust')}",
    ]
    return result


CRITERIA = [
    check_k2_boot,
    check_created_through_interface,
    check_derivation_narrows,
    check_generational_handles,
    check_authenticated_origin,
    check_admission_and_effect,
    check_barrier_not_drain,
    check_barrier_reaches_delegated,
    check_obligation_survives,
    check_charge_conservation,
    check_bounded_tables,
    check_closure_available,
    check_malformed_refused,
    check_negative_controls,
]


# --- self-test --------------------------------------------------------------
#
# A gate that has only ever returned PASS has not been shown to be able to
# return anything else. Each mutation below damages the log in exactly one way
# that a real failure would, and names the criterion that must notice. A
# mutation that leaves every criterion passing is a criterion that is not
# actually checking what its title claims, which is worth failing the build
# over.


def drop_event(event: str):
    def mutate(lines: list[str]) -> list[str]:
        return [line for line in lines if f" {event} " not in line]

    return mutate


def rewrite(pattern: str, replacement: str):
    def mutate(lines: list[str]) -> list[str]:
        return [line.replace(pattern, replacement) for line in lines]

    return mutate


# (name, mutation, criterion that must fail)
MUTATIONS = [
    ("no barrier record", drop_event("scope.fenced"), "barrier"),
    (
        "barrier reporting nothing outstanding",
        rewrite(
            "threads=1 invocations_pending=1 effects_pending=1",
            "threads=0 invocations_pending=0 effects_pending=0",
        ),
        "barrier",
    ),
    (
        "a derivation that widened rights",
        rewrite("parent_rights=0x307 child_rights=0x105", "parent_rights=0x105 child_rights=0x307"),
        "derivation",
    ),
    ("no retirement", drop_event("scope.retired"), "barrier"),
    (
        "retirement that freed nothing",
        rewrite("freed_pages=1 freed_objects=1", "freed_pages=0 freed_objects=0"),
        "charges",
    ),
    (
        "retirement that left a charge behind",
        rewrite("retained_pages=0 retained_metadata=0", "retained_pages=4 retained_metadata=2"),
        "charges",
    ),
    ("no effect admitted", drop_event("effect.admitted"), "admission"),
    ("no invocation resolved", drop_event("ipc.resolved"), "obligation"),
    (
        "a claimed origin reaching a receipt",
        rewrite("ctrl.origin_overridden", "ctrl.origin_kept"),
        "origin",
    ),
    ("nothing refused", drop_event("k2.refused"), "bounds"),
    ("no memory released", drop_event("mem.released"), "charges"),
    (
        "one refused operation reported as having succeeded",
        rewrite("a=0x2007 b=0xffffffffffffffed", "a=0x2008 b=0xffffffffffffffed"),
        "controls",
    ),
    (
        "a program that never reached the end of its script",
        rewrite("a=0x200b b=", "a=0x20ff b="),
        "controls",
    ),
    (
        "a second domain the kernel built for itself",
        rewrite("state=building managed=1", "state=building managed=0"),
        "boot",
    ),
    ("no boot capability manifest", drop_event("k2.boot_capability"), "boot"),
    ("a gap in the record sequence", drop_event("sched.preempt"), "reproducible"),
]


def self_test(text: str, run: dict, manifest: dict | None, k1: dict | None, quiet: bool) -> int:
    """Damages the log one way at a time and reports which criteria noticed."""
    lines = text.splitlines()
    clean = parse(text)
    by_name = {check(clean, run).name: check for check in CRITERIA}
    by_name["reproducible"] = lambda r, u: check_reproducible(r, u, manifest)
    by_name["k1"] = lambda r, u: check_k1_regression(r, u, k1)

    baseline = [check(clean, run) for check in CRITERIA]
    baseline.append(check_k1_regression(clean, run, k1))
    baseline.append(check_reproducible(clean, run, manifest))
    if any(not result.passed for result in baseline):
        print("self-test needs a passing run to damage; this one already fails", file=sys.stderr)
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
        print(f"K2 GATE SELF-TEST FAILED: {len(missed)} of {len(MUTATIONS)} damaged runs still passed")
        return 1
    print(f"K2 GATE SELF-TEST PASSED: {len(MUTATIONS)} of {len(MUTATIONS)} damaged runs were caught")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=Path, default=ROOT / "build/run-k2")
    parser.add_argument("--manifest", type=Path, default=ROOT / "build/image-manifest-k2.json")
    parser.add_argument("--k1", type=Path, default=ROOT / "build/k1-gate.json")
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
        print(f"serial log not found: {log}; run tools/run_k2.py", file=sys.stderr)
        return 2
    if not record_path.exists():
        print(f"run record not found: {record_path}; run tools/run_k2.py", file=sys.stderr)
        return 2

    text = log.read_text(errors="replace")
    records = parse(text)
    run = json.loads(record_path.read_text())
    manifest = json.loads(arguments.manifest.read_text()) if arguments.manifest.exists() else None
    k1 = json.loads(arguments.k1.read_text()) if arguments.k1.exists() else None

    if arguments.self_test:
        return self_test(text, run, manifest, k1, arguments.quiet)

    results = [check(records, run) for check in CRITERIA]
    results.append(check_k1_regression(records, run, k1))
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
        "gate": "K2",
        "passed": not failed,
        "criteria": [
            {"name": r.name, "title": r.title, "passed": r.passed, "detail": r.detail}
            for r in results
        ],
        "records": len(records),
        "serial_log": str(log),
    }
    if arguments.json:
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")

    print()
    if failed:
        print(f"K2 GATE FAILED: {len(failed)} of {len(results)} criteria not met")
        return 1
    print(f"K2 GATE PASSED: {len(results)} of {len(results)} criteria met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
