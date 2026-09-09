#!/usr/bin/env python3
"""Evaluate a K1 run against the gate.

The run script deliberately does not interpret the serial log, and the kernel's
exit status only reports that it ran out of runnable domains. Neither of those
shows that the run reached ring 3, that preemption was involuntary, that an
illegal access was contained, or that anything kept running afterwards. A boot
that printed from ring 0 and halted would produce the same status.

This reads the kernel's own records and decides each gate criterion separately,
so a criterion fails on its own rather than being carried by the others. It adds
no knowledge of what should have happened beyond the record vocabulary: every
judgement below is a statement about lines the kernel emitted.

Usage: tools/check_k1.py [--run build/run] [--manifest build/image-manifest.json]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Mirrors `thalyx_kernel::diag::FORMAT`. A change to the record shape changes
# this tag, and this checker refuses a log that does not carry it.
FORMAT = "THLX1"

RECORD = re.compile(
    r"^" + FORMAT + r" (?P<source>loader|kernel) (?P<seq>\d+) (?P<ns>\d+|-) (?P<event>\S+)(?P<rest>.*)$"
)

# Selector values the gate depends on. They are the architectural constants of
# `kernel/src/arch/x86_64/gdt.rs`, repeated here so that a kernel that silently
# changed them would fail rather than redefine the check.
USER_CS = 0x23
USER_DS = 0x1B
VECTOR_PAGE_FAULT = 14

# `run_k1.py` maps these to QEMU's `(value << 1) | 1`.
EXIT_COMPLETE = 33


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


def line_of(record: Record) -> str:
    rendered = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{record.seq} {record.event} {rendered}".rstrip()


# --- criteria ---------------------------------------------------------------
#
# Each function receives the parsed records and the run record, fills in a
# Result, and never raises: a missing record is a failed criterion, not a crash.


def check_bootable(records: list[Record], run: dict) -> Result:
    result = Result("bootable", "imagen arrancable en QEMU")
    entry = by_event(records, "loader.entry")
    handoff = by_event(records, "loader.handoff")
    kernel_entry = by_event(records, "kernel.entry")
    if not entry:
        result.detail = "the loader never ran: no loader.entry record"
        return result
    if not handoff:
        result.detail = "the loader never handed control over: no loader.handoff record"
        return result
    if not kernel_entry:
        result.detail = "the kernel never ran: no kernel.entry record"
        return result
    if kernel_entry[0].number("magic_ok") != 1:
        result.detail = "the kernel rejected the boot information it was handed"
        return result
    if run.get("exit_status") != EXIT_COMPLETE:
        result.detail = f"run exited with {run.get('exit_status')}, expected {EXIT_COMPLETE}"
        return result
    if run.get("timed_out"):
        result.detail = "the run timed out"
        return result
    result.passed = True
    result.detail = "firmware started the loader, which entered the kernel; run exited complete"
    result.evidence = [line_of(entry[0]), line_of(handoff[0]), line_of(kernel_entry[0])]
    return result


def check_bootstrap(records: list[Record], run: dict) -> Result:
    result = Result("bootstrap", "bootstrap")
    validated = by_event(records, "loader.kernel_validated")
    package = by_event(records, "loader.package_validated")
    exited = by_event(records, "loader.exit_boot_services")
    accepted = by_event(records, "boot.validated")
    if not (validated and package and exited and accepted):
        result.detail = "the loader did not complete validate, exit-boot-services and handoff"
        return result
    if exited[0].get("status") != "ok":
        result.detail = f"exit_boot_services reported {exited[0].get('status')}"
        return result
    regions = accepted[0].number("regions") or 0
    modules = accepted[0].number("modules") or 0
    if regions <= 0 or modules <= 0:
        result.detail = "the kernel accepted a boot record with no regions or no modules"
        return result
    result.passed = True
    result.detail = (
        f"kernel and package validated before exit-boot-services; kernel accepted "
        f"{regions} memory regions and {modules} modules"
    )
    result.evidence = [line_of(validated[0]), line_of(package[0]), line_of(exited[0]), line_of(accepted[0])]
    return result


def check_memory(records: list[Record], run: dict) -> Result:
    result = Result("memory", "control propio de memoria")
    frames = by_event(records, "mm.frames")
    paging = by_event(records, "mm.paging_installed")
    mapped = by_event(records, "mm.kernel_image_mapped")
    reclaimed = by_event(records, "mm.reclaimed")
    if not (frames and paging and mapped):
        result.detail = "the kernel did not take over frames and page tables"
        return result
    if frames[0].get("bitmap_owner") != "kernel":
        result.detail = "the frame allocator is not owned by the kernel"
        return result
    if paging[0].get("owner") != "kernel":
        result.detail = "the kernel is still running on the loader's page tables"
        return result
    # Taking ownership is only half of it: a kernel that never gave a frame back
    # would pass every check above. Each domain's charge must return to zero.
    leaked = [record for record in reclaimed if record.number("charged_after") != 0]
    if not reclaimed:
        result.detail = "no domain memory was ever reclaimed"
        return result
    if leaked:
        result.detail = f"{len(leaked)} domain(s) still held frames after reclaim"
        return result
    result.passed = True
    result.detail = (
        f"kernel owns the frame bitmap and its own tables (cr3={paging[0].get('cr3')}); "
        f"all {len(reclaimed)} domains reclaimed to zero charged frames"
    )
    result.evidence = [line_of(frames[0]), line_of(paging[0]), line_of(mapped[0]), line_of(reclaimed[-1])]
    return result


def check_traps(records: list[Record], run: dict) -> Result:
    result = Result("traps", "traps/interrupciones")
    idt = by_event(records, "cpu.idt_installed")
    tss = by_event(records, "cpu.tss_installed")
    faults = by_event(records, "user.fault")
    preempts = by_event(records, "sched.preempt")
    if not idt:
        result.detail = "no IDT was installed"
        return result
    if not tss:
        result.detail = "no TSS was installed, so faults have no stack to take"
        return result
    # An installed IDT proves configuration, not delivery. Both a fault and an
    # interrupt must have actually been taken and returned from.
    if not faults:
        result.detail = "the IDT was installed but no exception was ever delivered"
        return result
    if not preempts:
        result.detail = "the IDT was installed but no interrupt was ever delivered"
        return result
    result.passed = True
    result.detail = (
        f"{idt[0].get('entries')} IDT entries installed with emergency stacks; "
        f"{len(faults)} exception(s) and {len(preempts)} interrupt(s) delivered"
    )
    result.evidence = [line_of(idt[0]), line_of(tss[0]), line_of(faults[0])]
    return result


def check_timer(records: list[Record], run: dict) -> Result:
    result = Result("timer", "timer")
    armed = by_event(records, "timer.armed")
    source = by_event(records, "time.source")
    summary = by_event(records, "k1.summary")
    if not armed:
        result.detail = "no timer was armed"
        return result
    if armed[0].get("mode") != "periodic":
        result.detail = f"timer armed in {armed[0].get('mode')} mode, not periodic"
        return result
    ticks = summary[0].number("timer_ticks") if summary else None
    if not ticks:
        result.detail = "the timer was armed but never fired"
        return result
    result.passed = True
    result.detail = (
        f"periodic timer on vector {armed[0].get('vector')} at {armed[0].get('tick_hz')} Hz "
        f"fired {ticks} times"
    )
    result.evidence = [line_of(source[0]) if source else "", line_of(armed[0]), line_of(summary[0])]
    return result


def check_ring3(records: list[Record], run: dict) -> Result:
    result = Result("ring3", "ring 3")
    confirmed = by_event(records, "user.ring3_confirmed")
    if not confirmed:
        result.detail = "no domain was ever observed executing at CPL 3"
        return result
    # The record is taken from the interrupted frame of a domain already
    # running, so it reports the privilege the CPU was actually at.
    for record in confirmed:
        if record.number("cpl") != 3:
            result.detail = f"{record.get('name')} reported CPL {record.get('cpl')}"
            return result
        if record.number("cs") != USER_CS or record.number("ss") != USER_DS:
            result.detail = f"{record.get('name')} ran with kernel selectors"
            return result
        if record.number("iopl") != 0:
            result.detail = f"{record.get('name')} ran with IOPL {record.get('iopl')}"
            return result
    names = sorted({record.get("name") or "?" for record in confirmed})
    result.passed = True
    result.detail = (
        f"{len(confirmed)} domain(s) confirmed at CPL 3 with cs={hex(USER_CS)} "
        f"ss={hex(USER_DS)} iopl=0: {', '.join(names)}"
    )
    result.evidence = [line_of(record) for record in confirmed[:2]]
    return result


def check_two_domains(records: list[Record], run: dict) -> Result:
    result = Result("domains", "dos programas/dominios")
    activated = by_event(records, "domain.activated")
    created = {record.number("domain"): record for record in by_event(records, "domain.created")}
    if len(activated) < 2:
        result.detail = f"only {len(activated)} domain(s) reached a runnable state"
        return result
    # Separate address spaces are what make them domains rather than threads.
    spaces = {record.get("cr3") for record in created.values() if record.get("cr3")}
    if len(spaces) < len(created):
        result.detail = "two domains were built on the same address space"
        return result
    names = [record.get("name") for record in activated]
    result.passed = True
    result.detail = (
        f"{len(activated)} domains activated in {len(spaces)} distinct address spaces: "
        f"{', '.join(str(name) for name in names)}"
    )
    result.evidence = [line_of(record) for record in activated[:2]]
    return result


def check_preemption(records: list[Record], run: dict) -> Result:
    result = Result("preemption", "preempción real")
    preempts = by_event(records, "sched.preempt")
    involuntary = [
        record
        for record in preempts
        if record.number("voluntary") == 0
        and record.get("trigger") == "timer"
        and record.number("cpl") == 3
    ]
    if not involuntary:
        result.detail = "no domain was interrupted by the timer while running in user mode"
        return result
    # One domain yielding repeatedly is not preemption. The timer must have
    # taken the CPU away from more than one domain, against its will.
    victims = {record.get("domain") for record in involuntary}
    if len(victims) < 2:
        result.detail = f"only domain {victims} was ever preempted"
        return result
    summary = by_event(records, "k1.summary")
    total = summary[0].number("preemptions") if summary else len(involuntary)
    result.passed = True
    result.detail = (
        f"{total} preemptions total; {len(involuntary)} recorded as involuntary timer "
        f"interrupts of user code across {len(victims)} domains"
    )
    result.evidence = [line_of(record) for record in involuntary[:2]]
    return result


def check_fp_context(records: list[Record], run: dict) -> Result:
    result = Result("fp", "contexto FP por dominio")
    initialised = by_event(records, "cpu.fpu_initialized")
    if not initialised:
        result.detail = "FP state was never initialised"
        return result

    notes = by_event(records, "user.note")
    # Each domain plants a pattern derived from its own identifier, so two
    # domains built from the same image still differ. Without that, "every
    # domain found its pattern intact" would also hold for a kernel that never
    # switched FP state at all.
    planted = {
        note.get("name"): note.number("b")
        for note in notes
        if note.get("kind") == "self_check" and note.number("a") == 0x02
    }
    if len(planted) < 2:
        result.detail = f"only {len(planted)} domain(s) planted an FP pattern"
        return result
    if len(set(planted.values())) != len(planted):
        result.detail = "two domains planted the same FP pattern, so the check proves nothing"
        return result

    # The verification that matters is the one taken after the domain lost the
    # CPU: it says the state came back, not merely that it was set.
    verified_after_preemption = 0
    for note in notes:
        if note.get("kind") != "progress":
            continue
        if note.number("b") != 1:
            result.detail = (
                f"{note.get('name')} found its FP state altered at note {note.get('note_seq')}"
            )
            return result
        if (note.number("preemptions") or 0) > 0:
            verified_after_preemption += 1
    if not verified_after_preemption:
        result.detail = "no domain verified its FP state after being preempted"
        return result

    result.passed = True
    result.detail = (
        f"{len(planted)} distinct FP patterns planted, policy={initialised[0].get('policy')}; "
        f"{verified_after_preemption} verifications intact after preemption"
    )
    result.evidence = [line_of(initialised[0])] + [
        f"{name} planted {hex(pattern)}" for name, pattern in list(planted.items())[:2]
    ]
    return result


def check_contained_fault(records: list[Record], run: dict) -> Result:
    result = Result("fault", "fallo ilegal contenido")
    faults = by_event(records, "user.fault")
    if not faults:
        result.detail = "no domain ever attempted an illegal access"
        return result
    terminated = {
        record.get("domain"): record
        for record in by_event(records, "domain.terminated")
        if record.get("reason") == "user_fault"
    }
    for fault in faults:
        if fault.number("cpl") != 3:
            result.detail = f"the fault at seq {fault.seq} was not taken from user mode"
            return result
        if fault.get("action") != "terminate_domain":
            result.detail = f"the fault at seq {fault.seq} did not terminate its domain"
            return result
        if fault.get("domain") not in terminated:
            result.detail = f"domain {fault.get('domain')} faulted but was never terminated"
            return result
    # The rejected module shows the same containment before execution: the
    # validator refuses an image on the path that accepted the others.
    rejected = by_event(records, "module.rejected")
    faulted = ", ".join(f"{f.get('name')} (cr2={f.get('cr2')})" for f in faults)
    result.passed = True
    result.detail = f"{len(faults)} illegal access(es) contained to the faulting domain: {faulted}"
    result.evidence = [line_of(fault) for fault in faults]
    if rejected:
        result.evidence.append(line_of(rejected[0]))
    return result


def check_kernel_survives(records: list[Record], run: dict) -> Result:
    result = Result("survival", "supervivencia del kernel")
    faults = by_event(records, "user.fault")
    terminal = by_event(records, "k1.terminal")
    if not faults:
        result.detail = "nothing happened that the kernel had to survive"
        return result
    for fault in faults:
        if fault.get("kernel") != "survives":
            result.detail = f"the kernel did not record surviving the fault at seq {fault.seq}"
            return result
    if any(record.event.startswith("panic") for record in records):
        result.detail = "the kernel panicked"
        return result
    if not terminal:
        result.detail = "the kernel never reached a terminal state"
        return result
    if terminal[-1].get("status") != "complete":
        result.detail = f"terminal status was {terminal[-1].get('status')}"
        return result
    if terminal[-1].get("reason") != "no_runnable_domain":
        result.detail = f"the run ended for the wrong reason: {terminal[-1].get('reason')}"
        return result
    # Continuing to emit records after the last fault is the observable form of
    # survival: the kernel kept scheduling rather than merely not crashing.
    after = [record for record in records if record.seq > faults[-1].seq]
    if not after:
        result.detail = "the kernel emitted nothing after the last fault"
        return result
    result.passed = True
    result.detail = (
        f"kernel took {len(faults)} user fault(s), emitted {len(after)} further records, "
        f"and ended at its own terminal state"
    )
    result.evidence = [line_of(terminal[-1])]
    return result


def check_progress_after_fault(records: list[Record], run: dict) -> Result:
    result = Result("progress", "progreso posterior del otro programa")
    faults = by_event(records, "user.fault")
    if not faults:
        result.detail = "no fault occurred, so there is nothing to have survived it"
        return result
    last_fault = faults[-1].seq
    faulted = {fault.get("domain") for fault in faults}

    # Only a domain that never faulted counts, and only its own monotonic
    # counter counts: records emitted by the kernel about it would not show
    # that user code resumed.
    progress: dict[str, list[int]] = {}
    for record in by_event(records, "user.note"):
        if record.seq <= last_fault or record.get("kind") != "progress":
            continue
        domain = record.get("domain")
        if domain in faulted:
            continue
        value = record.number("a")
        if value is not None:
            progress.setdefault(f"{domain}:{record.get('name')}", []).append(value)

    advancing = {name: values for name, values in progress.items() if len(values) >= 2 and values[-1] > values[0]}
    if not advancing:
        result.detail = "no surviving domain advanced its counter after the last fault"
        return result
    for name, values in advancing.items():
        if values != sorted(values):
            result.detail = f"{name} emitted progress out of order after the fault"
            return result
    described = ", ".join(
        f"{name} advanced {hex(values[0])}->{hex(values[-1])} in {len(values)} notes"
        for name, values in sorted(advancing.items())
    )
    result.passed = True
    result.detail = f"after the last fault at seq {last_fault}: {described}"
    result.evidence = [
        line_of(record)
        for record in by_event(records, "user.note")
        if record.seq > last_fault and record.get("kind") == "progress"
    ][:2]
    return result


def check_reproducible(records: list[Record], run: dict, manifest: dict | None) -> Result:
    result = Result("reproducible", "evidencia reproducible")
    if manifest is None:
        result.detail = "no image manifest was found next to the run"
        return result
    # The log must be whole. The sequence is shared by loader and kernel, so a
    # gap means records were lost and the run cannot be judged from it.
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
    # The run must be traceable to the image whose inputs are recorded.
    image = manifest.get("artifacts", {}).get("image", {})
    if not image.get("sha256"):
        result.detail = "the manifest records no image digest"
        return result
    ran = Path(run.get("image", ""))
    if ran.exists():
        import hashlib

        digest = hashlib.sha256(ran.read_bytes()).hexdigest()
        if digest != image["sha256"]:
            result.detail = "the image that was run is not the image in the manifest"
            return result
    modules = manifest.get("modules", [])
    if not modules:
        result.detail = "the manifest records no modules"
        return result
    result.passed = True
    result.detail = (
        f"{len(sequence)} records with no gap in the shared sequence; run traced to image "
        f"{image['sha256'][:16]} built from {len(modules)} recorded modules and a pinned toolchain"
    )
    result.evidence = [
        f"image sha256={image['sha256']}",
        f"kernel sha256={manifest['artifacts']['kernel']['sha256']}",
        f"rust={manifest.get('rust')}",
    ]
    return result


CRITERIA = [
    check_bootable,
    check_bootstrap,
    check_memory,
    check_traps,
    check_timer,
    check_ring3,
    check_two_domains,
    check_preemption,
    check_fp_context,
    check_contained_fault,
    check_kernel_survives,
    check_progress_after_fault,
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=Path, default=ROOT / "build/run")
    parser.add_argument("--manifest", type=Path, default=ROOT / "build/image-manifest.json")
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    parser.add_argument("--quiet", action="store_true", help="print only the verdict line")
    arguments = parser.parse_args()

    log = arguments.run / "serial.log"
    record_path = arguments.run / "run.json"
    if not log.exists():
        print(f"serial log not found: {log}; run tools/run_k1.py", file=sys.stderr)
        return 2
    if not record_path.exists():
        print(f"run record not found: {record_path}; run tools/run_k1.py", file=sys.stderr)
        return 2

    records = parse(log.read_text(errors="replace"))
    run = json.loads(record_path.read_text())
    manifest = json.loads(arguments.manifest.read_text()) if arguments.manifest.exists() else None

    results = [check(records, run) for check in CRITERIA]
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
        "gate": "K1",
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
        print(f"K1 GATE FAILED: {len(failed)} of {len(results)} criteria not met")
        return 1
    print(f"K1 GATE PASSED: {len(results)} of {len(results)} criteria met")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
