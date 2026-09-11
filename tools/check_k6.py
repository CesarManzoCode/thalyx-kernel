#!/usr/bin/env python3
"""Evaluate a K6 campaign against the gate.

K6 is the comparison: this kernel against Linux on the same virtual machine,
from one source, with what each result may say decided in the schema before
anything ran. The thing that is easiest to fake about a comparison is its
equivalence -- that both sides did the same work under the same conditions --
and the thing that is easiest to overstate is its conclusion. This gate is
written against both.

  * **The native numbers are the kernel's records.** A sample reaches the host
    as a `user.note` the kernel stamps with the domain that wrote it; the
    entries a latency benchmark timed are counted by the kernel in that
    thread's syscall counter; the trace records the kernel withheld are
    declared in its own summary. A guest printing numbers proves nothing; a
    kernel counting them proves it ran them.
  * **Both sides ran the same plan.** The Linux guest's entry sequence is the
    native one minus what the schema declares native-only; both calibrated the
    same counter within a fraction of a percent; the engine on both answered
    the fixture's questions with the bytes Thalyx's own engine gives.
  * **A verdict is what the schema allows and no more.** An `equivalent`
    benchmark may say faster or slower only when the paired interval excludes
    one; a `comparable` one reports a cost ratio under stated guarantees; a
    `different` one concludes nothing about speed; a `native_only` one is not
    compared.

Each criterion is decided separately and carries what it was decided from. The
K1 to K5 regressions are criteria here too, read from the gate verdicts the
canonical runs left, so a K6 gate never passes over a kernel that broke the
phases it grew inside.

Usage: tools/check_k6.py [--campaign build/k6/campaigns/baseline] [--self-test]
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
import build_reference  # noqa: E402
import k6_analysis as analysis  # noqa: E402

FORMAT = "THLX1"
RECORD = re.compile(
    r"^" + FORMAT + r" (?P<source>loader|kernel) (?P<seq>\d+) (?P<ns>\d+|-) "
    r"(?P<event>\S+)(?P<rest>.*)$"
)
NATIVE_EXIT = 33
CLOCK_TOLERANCE = 0.005
ALLOWED = {
    "equivalent": {"NATIVE_FASTER", "NATIVE_SLOWER", "INCONCLUSIVE", "DIFFERENT_ENFORCEMENT",
                   "NOT_ENOUGH_ROUNDS"},
    "comparable": {"COST_RATIO_UNDER_STATED_GUARANTEES", "NOT_ENOUGH_ROUNDS"},
    "different": {"NO_SPEED_CONCLUSION", "NOT_ENOUGH_ROUNDS"},
    "native_only": set(),
}
# Kernel entries one timed operation makes on the native side, at least.
ENTRIES_PER_SAMPLE = {
    "entry.version": 1, "entry.clock": 1, "ipc.call": 1, "ipc.caps": 1, "ipc.lineage": 1,
    "mem.map": 4, "mem.seal": 6, "cap.derive": 2,
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


@dataclass
class Boot:
    arm: str
    round: int
    record: dict
    entries: list[dict]
    parsed: dict
    extra: dict
    records: list[Record]
    linux_lines: list[str]


@dataclass
class Campaign:
    directory: Path
    campaign: dict
    results: dict
    boots: list[Boot]
    inventory: dict | None
    reference_note: str | None


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
        records.append(Record(match.group("source"), int(match.group("seq")),
                              None if match.group("ns") == "-" else int(match.group("ns")),
                              match.group("event"), fields))
    return records


def by_event(records: list[Record], event: str) -> list[Record]:
    return [record for record in records if record.event == event]


def line_of(record: Record) -> str:
    fields = " ".join(f"{k}={v}" for k, v in record.fields.items())
    return f"{FORMAT} {record.source} {record.seq} {record.ns if record.ns is not None else '-'} {record.event} {fields}"


def load(directory: Path) -> Campaign:
    campaign = json.loads((directory / "campaign.json").read_text())
    results = json.loads((directory / "results.json").read_text())
    samples = json.loads((directory / "samples.json").read_text())
    by_key = {(s["arm"], s["round"]): s for s in samples}
    boots = []
    for record in campaign["boots"]:
        sample = by_key.get((record["arm"], record["round"]), {})
        boot_dir = ROOT / record["directory"]
        text = ""
        linux_lines: list[str] = []
        if record["arm"] == "native":
            path = boot_dir / "serial.log"
            text = path.read_text() if path.exists() else ""
        else:
            path = boot_dir / "debugcon.log"
            linux_lines = path.read_text().splitlines() if path.exists() else []
        boots.append(Boot(
            arm=record["arm"], round=record["round"], record=record,
            entries=sample.get("entries", []),
            parsed={k: sample.get(k) for k in ("tsc_hz", "seed", "done")},
            extra=sample.get("extra", {}),
            records=parse(text), linux_lines=linux_lines,
        ))
    inventory_path = directory / "host-inventory.json"
    inventory = json.loads(inventory_path.read_text()) if inventory_path.exists() else None
    reference_note = None
    for candidate in (ROOT / "build/k5-runs/engine/reference.json",
                      ROOT / "build/k5-cases/reference.json"):
        if candidate.exists():
            reference_note = json.loads(candidate.read_text()).get("note")
            break
    return Campaign(directory, campaign, results, boots, inventory, reference_note)


def native_boots(c: Campaign) -> list[Boot]:
    return [b for b in c.boots if b.arm == "native"]


def linux_boots(c: Campaign) -> list[Boot]:
    return [b for b in c.boots if b.arm != "native"]


def bench_names() -> dict[int, dict]:
    return analysis.benchmarks()


def native_only_ids() -> set[int]:
    return {b["id"] for b in analysis.schema()["benchmarks"] if b["equivalence"] == "native_only"}


def result_for(c: Campaign, name: str, param: int) -> dict | None:
    for item in c.results["results"]:
        if item["bench"] == name and item["param"] == param:
            return item
    return None


# --- criteria ----------------------------------------------------------------


def check_campaign_ran(c: Campaign) -> Result:
    result = Result("ran", "every round booted every arm and every plan ran to its end")
    rounds = c.campaign["rounds"]
    arms = c.campaign["arms"]
    problems = []
    seen = set()
    for boot in c.boots:
        seen.add((boot.arm, boot.round))
        expected_exit = NATIVE_EXIT if boot.arm == "native" else 0
        if boot.record.get("exit_status") != expected_exit:
            problems.append(f"{boot.arm} r{boot.round} exit {boot.record.get('exit_status')}")
        if boot.record.get("entries_reported") != boot.record.get("entries_planned"):
            problems.append(f"{boot.arm} r{boot.round} reported {boot.record.get('entries_reported')} "
                            f"of {boot.record.get('entries_planned')} entries")
        if boot.parsed.get("done") != boot.record.get("entries_planned"):
            problems.append(f"{boot.arm} r{boot.round} DONE said {boot.parsed.get('done')}")
    for r in range(1, rounds + 1):
        for arm in arms:
            if (arm, r) not in seen:
                problems.append(f"{arm} r{r} never booted")
    result.passed = not problems and len(c.boots) == rounds * len(arms)
    result.detail = (f"{len(c.boots)} boots over {rounds} rounds of {arms}"
                     + (f"; {'; '.join(problems[:4])}" if problems else ""))
    return result


def check_same_plan(c: Campaign) -> Result:
    result = Result("same_plan", "both sides ran the same plan, less what the schema declares native-only")
    skip = native_only_ids()
    problems = []
    for native in native_boots(c):
        expected = [(e["bench"], e["param"], e["requested"]) for e in native.entries
                    if e["bench"] not in skip]
        for other in linux_boots(c):
            if other.round != native.round:
                continue
            got = [(e["bench"], e["param"], e["requested"]) for e in other.entries]
            if got != expected:
                problems.append(f"{other.arm} r{other.round} ran a different sequence")
            if other.parsed.get("seed") != native.parsed.get("seed"):
                problems.append(f"{other.arm} r{other.round} ran plan seed {other.parsed.get('seed')} "
                                f"against native {native.parsed.get('seed')}")
    compared = sum(1 for b in linux_boots(c))
    result.passed = compared > 0 and not problems
    result.detail = (f"{compared} Linux boots each ran the native sequence of their round minus "
                     f"{len(skip)} native-only benchmarks, under the same plan seed"
                     if result.passed else "; ".join(problems) or "no Linux boot to compare")
    return result


def check_native_is_kernel_record(c: Campaign) -> Result:
    result = Result("kernel_record", "the native samples are notes of domains the kernel built on the K2 path")
    problems = []
    evidence = []
    for boot in native_boots(c):
        built = by_event(boot.records, "k2.supervisor_built")
        terminal = by_event(boot.records, "k1.terminal")
        modules = [r for r in by_event(boot.records, "loader.module") if r.get("name") == "k6super"]
        summaries = {r.get("name"): r for r in by_event(boot.records, "k1.domain_summary")}
        if not built:
            problems.append(f"r{boot.round}: no supervisor built")
        if not terminal or terminal[0].get("boot_path") != "k2_supervisor":
            problems.append(f"r{boot.round}: not the K2 boot path")
        if not modules or (modules[0].number("flags") or 0) & 2 == 0:
            problems.append(f"r{boot.round}: the supervisor module did not ask for summaries only")
        client = summaries.get("k6client")
        if client is None or (client.number("notes") or 0) == 0:
            problems.append(f"r{boot.round}: no client domain summary with notes")
        if "k6server" not in summaries:
            problems.append(f"r{boot.round}: no server domain summary")
        sources = {s for e in boot.entries for s in e.get("sources", [])}
        if not sources <= {"k6client", "supervisor", "k6spin"}:
            problems.append(f"r{boot.round}: samples came from {sorted(sources)}")
        if built and client is not None and not evidence:
            evidence = [line_of(built[0]), line_of(client)]
    result.passed = bool(native_boots(c)) and not problems
    result.detail = (f"{len(native_boots(c))} native boots: supervisor built by the kernel, client and "
                     f"server domains summarised, samples from the client, the supervisor and the "
                     f"quota domain only" if result.passed else "; ".join(problems[:4]))
    result.evidence = evidence
    return result


def check_trace_declared(c: Campaign) -> Result:
    result = Result("trace_declared", "the kernel wrote summaries only and declared how many trace records it withheld")
    problems = []
    evidence = []
    for boot in native_boots(c):
        diag = boot.extra.get("diag", {})
        if diag.get("trace") != "off" or not isinstance(diag.get("trace_withheld"), int) \
                or diag.get("trace_withheld", 0) <= 0:
            problems.append(f"r{boot.round}: diag.summary trace={diag.get('trace')} "
                            f"withheld={diag.get('trace_withheld')}")
        switched = by_event(boot.records, "diag.trace")
        if not switched or switched[0].get("per_operation") != "off":
            problems.append(f"r{boot.round}: no diag.trace record turning tracing off")
        elif not evidence:
            evidence = [line_of(switched[0])]
    result.passed = bool(native_boots(c)) and not problems
    withheld = [b.extra.get("diag", {}).get("trace_withheld") for b in native_boots(c)]
    result.detail = (f"trace off in every native boot; withheld per boot {withheld}"
                     if result.passed else "; ".join(problems[:4]))
    result.evidence = evidence
    return result


def client_syscalls(boot: Boot) -> list[tuple[int, int, int]]:
    """(code, value, syscalls) of the client's K6 notes, in order."""
    out = []
    for record in by_event(boot.records, "user.note"):
        if record.get("name") != "k6client":
            continue
        code = record.number("a")
        value = record.number("b")
        calls = record.number("syscalls")
        if code is not None and value is not None and calls is not None:
            out.append((code, value, calls))
    return out


def check_kernel_counted_entries(c: Campaign) -> Result:
    result = Result("entries_counted", "the kernel counted at least as many entries by the client as samples it timed (latency benchmarks)")
    code = analysis.notes()
    names = {b["id"]: b["name"] for b in analysis.schema()["benchmarks"]}
    checked = 0
    problems = []
    for boot in native_boots(c):
        notes = client_syscalls(boot)
        current = None
        for note, value, calls in notes:
            if note == code["BEGIN"]:
                current = (value & 0xFFFF, (value >> 16) & 0xFFFF, value >> 32, calls)
            elif note == code["END"] and current is not None:
                bench, param, requested, at_begin = current
                name = names.get(bench)
                per = ENTRIES_PER_SAMPLE.get(name)
                if per is not None:
                    needed = requested * per
                    if calls - at_begin < needed:
                        problems.append(f"r{boot.round} {name}:{param}: {calls - at_begin} entries "
                                        f"for {requested} samples")
                    checked += 1
                current = None
    result.passed = checked > 0 and not problems
    result.detail = (f"{checked} latency entries, each with at least one kernel entry per sample "
                     f"counted on the client thread" if result.passed else "; ".join(problems[:4]))
    return result


def check_no_lost_samples(c: Campaign) -> Result:
    result = Result("no_loss", "no entry on any arm lost a sample or refused an operation")
    problems = []
    entries = 0
    for boot in c.boots:
        for entry in boot.entries:
            entries += 1
            if entry["lost"] or entry["errors"] or entry["error_status"]:
                problems.append(f"{boot.arm} r{boot.round} bench {entry['bench']}:{entry['param']} "
                                f"lost={entry['lost']} errors={entry['errors']} "
                                f"status={entry['error_status']}")
    result.passed = entries > 0 and not problems
    result.detail = (f"{entries} entries across {len(c.boots)} boots, none lost or refused"
                     if result.passed else "; ".join(problems[:4]))
    return result


def check_engine_reference(c: Campaign) -> Result:
    result = Result("engine_reference", "on both sides the engine answered the fixture's prompts with the bytes Thalyx's engine gives on Linux, every time")
    references = c.results.get("references", {})
    problems = []
    checked = 0
    for boot in c.boots:
        for entry in boot.entries:
            if entry["bench"] != 14:
                continue
            ref = references.get(str(entry["param"]))
            if ref is None:
                problems.append(f"{boot.arm} r{boot.round}: no reference for prompt {entry['param']}")
                continue
            if entry["digest"] != ref["fnv1a"] or entry["check"] != entry["requested"] \
                    or entry["mismatch"]:
                problems.append(f"{boot.arm} r{boot.round} prompt {entry['param']}: digest "
                                f"{entry['digest']:016x} check {entry['check']}/{entry['requested']} "
                                f"mismatch {entry['mismatch']}")
            checked += 1
    result.passed = checked > 0 and not problems and c.reference_note is not None \
        and "host execution" in c.reference_note
    result.detail = (f"{checked} engine entries, every answer the reference's FNV-1a and every "
                     f"repetition identical; the reference is labelled host execution"
                     if result.passed else "; ".join(problems[:4]) or "no reference labelled as host execution")
    return result


def check_counters_agree(c: Campaign) -> Result:
    result = Result("counters", "every boot calibrated the same time-stamp counter to within half a percent")
    values = [(b.arm, b.round, b.parsed.get("tsc_hz")) for b in c.boots]
    hz = [v for _, _, v in values if v]
    if len(hz) != len(values) or not hz:
        result.detail = "a boot reported no calibration"
        return result
    spread = (max(hz) - min(hz)) / min(hz)
    result.passed = spread <= CLOCK_TOLERANCE
    result.detail = f"{min(hz)} to {max(hz)} Hz across {len(hz)} boots, spread {spread * 100:.3f}%"
    return result


def check_linux_guest(c: Campaign) -> Result:
    result = Result("linux_guest", "each Linux guest is the recorded kernel, on four processors, with the mitigations its arm declares")
    manifest = c.campaign.get("linux_manifest") or {}
    release = (manifest.get("kernel") or {}).get("release")
    problems = []
    for boot in linux_boots(c):
        info = boot.extra.get("guest", {})
        if info.get("kernel_release") != release:
            problems.append(f"{boot.arm} r{boot.round}: kernel {info.get('kernel_release')} not {release}")
        cmdline = info.get("cmdline", "")
        off = "mitigations=off" in cmdline
        if off != (boot.arm == "linux-nomitig"):
            problems.append(f"{boot.arm} r{boot.round}: cmdline {cmdline!r}")
        if info.get("processors_online") != str(c.campaign["processors"]):
            problems.append(f"{boot.arm} r{boot.round}: {info.get('processors_online')} processors")
        vulns = info.get("vulnerabilities", {})
        texts = " ".join(vulns.values())
        if not vulns:
            problems.append(f"{boot.arm} r{boot.round}: no vulnerability report")
        elif boot.arm == "linux-nomitig" and "Vulnerable" not in texts:
            problems.append(f"{boot.arm} r{boot.round}: mitigations off but nothing reads Vulnerable")
        elif boot.arm == "linux" and "Mitigation:" not in texts:
            problems.append(f"{boot.arm} r{boot.round}: mitigations on but nothing reads Mitigation")
    result.passed = bool(linux_boots(c)) and release is not None and not problems
    result.detail = (f"{len(linux_boots(c))} Linux boots of {release}, each reporting its command line, "
                     f"processor count and the kernel's own vulnerability view"
                     if result.passed else "; ".join(problems[:4]) or "no Linux boot")
    return result


def check_verdicts(c: Campaign) -> Result:
    result = Result("verdicts", "every verdict is one the schema's equivalence allows, and a speed difference is stated only with an interval that excludes one")
    problems = []
    counted = 0
    for item in c.results["results"]:
        kind = item["equivalence"]
        if kind not in ALLOWED:
            problems.append(f"{item['bench']}: equivalence {kind!r}")
            continue
        if kind == "native_only" and item["comparisons"]:
            problems.append(f"{item['bench']}: native_only yet compared")
        for comparison in item["comparisons"]:
            counted += 1
            verdict = comparison["verdict"]
            if verdict not in ALLOWED[kind]:
                problems.append(f"{item['bench']}:{item['param']} {verdict} under {kind}")
            ci = comparison["ratio_ci95"]
            excludes = isinstance(ci[0], (int, float)) and isinstance(ci[1], (int, float)) \
                and (ci[0] > 1 or ci[1] < 1)
            if verdict in ("NATIVE_FASTER", "NATIVE_SLOWER"):
                lower_better = item["statistic"] == "latency"
                ratio = comparison["ratio_native_over_other"]
                expected = "NATIVE_FASTER" if ((ratio < 1) if lower_better else (ratio > 1)) else "NATIVE_SLOWER"
                if not excludes or comparison["paired_rounds"] < 3 or verdict != expected:
                    problems.append(f"{item['bench']}:{item['param']} {verdict} with ci {ci} over "
                                    f"{comparison['paired_rounds']} rounds")
            if verdict == "INCONCLUSIVE" and excludes:
                problems.append(f"{item['bench']}:{item['param']} INCONCLUSIVE with ci {ci}")
    result.passed = counted > 0 and not problems
    result.detail = (f"{counted} comparisons, each verdict within its equivalence and each stated "
                     f"speed difference backed by an interval excluding one"
                     if result.passed else "; ".join(problems[:4]))
    return result


def check_paired(c: Campaign) -> Result:
    result = Result("paired", "every comparison paired every round")
    rounds = c.campaign["rounds"]
    problems = [f"{item['bench']}:{item['param']} vs {cmp['against']} paired {cmp['paired_rounds']}"
                for item in c.results["results"] for cmp in item["comparisons"]
                if cmp["paired_rounds"] != rounds]
    count = sum(len(item["comparisons"]) for item in c.results["results"])
    result.passed = count > 0 and not problems and rounds >= 3
    result.detail = (f"{count} comparisons each paired over all {rounds} rounds"
                     if result.passed else "; ".join(problems[:4]) or f"{rounds} rounds")
    return result


def check_artifacts(c: Campaign) -> Result:
    result = Result("artifacts", "one kernel, one set of modules and one Linux image ran the whole campaign, on KVM, and every digest is recorded")
    kernels = {b.record.get("kernel_sha256") for b in native_boots(c)}
    modules = set()
    for boot in native_boots(c):
        modules.add(tuple(sorted((m["name"], m["sha256"]) for m in boot.record.get("modules", [])
                                 if m["name"] != "k6plan")))
    vmlinuz = {b.record.get("kernel_sha256") for b in linux_boots(c)}
    manifest = c.campaign.get("linux_manifest") or {}
    model = (manifest.get("model") or {}).get("sha256")
    native_model = {m["sha256"] for b in native_boots(c) for m in b.record.get("modules", [])
                    if m["name"] == "k5model"}
    problems = []
    if len(kernels) != 1 or None in kernels:
        problems.append(f"native kernels {kernels}")
    if len(modules) != 1:
        problems.append(f"{len(modules)} distinct module sets")
    if linux_boots(c) and (len(vmlinuz) != 1 or None in vmlinuz):
        problems.append(f"Linux kernels {vmlinuz}")
    if model != build_reference.MODEL_SHA256 or native_model != {build_reference.MODEL_SHA256}:
        problems.append("the model is not the pinned reference model on both sides")
    if c.campaign.get("accelerator") != "kvm":
        problems.append(f"accelerator {c.campaign.get('accelerator')}")
    if not (c.campaign.get("revision") or {}).get("head"):
        problems.append("no revision recorded")
    if any(not b.record.get("image_sha256") and not b.record.get("initrd_sha256") for b in c.boots):
        problems.append("a boot has no image digest")
    result.passed = bool(native_boots(c)) and not problems
    result.detail = (f"kernel {next(iter(kernels))[:12]}, {len(next(iter(modules)))} modules, Linux "
                     f"{next(iter(vmlinuz))[:12] if vmlinuz else 'absent'}, model pinned, KVM, "
                     f"revision {c.campaign['revision']['head'][:12]}"
                     if result.passed else "; ".join(problems[:4]))
    return result


def check_host_inventoried(c: Campaign) -> Result:
    result = Result("host", "the physical machine under the virtual one is inventoried")
    inventory = c.inventory or {}
    cpu = inventory.get("cpu", {})
    problems = []
    if not cpu.get("model_name"):
        problems.append("no processor model")
    if not (inventory.get("kvm") or {}).get("device"):
        problems.append("no KVM device recorded")
    if not cpu.get("smt_sibling_groups"):
        problems.append("no topology")
    if not inventory.get("storage"):
        problems.append("no storage")
    if not (inventory.get("firmware") or {}).get("dmi"):
        problems.append("no firmware")
    result.passed = not problems
    result.detail = (f"{cpu.get('model_name')}, {len(cpu.get('smt_sibling_groups', []))} cores, "
                     f"{len(inventory.get('storage', []))} block devices, "
                     f"{(inventory.get('iommu') or {}).get('groups')} IOMMU groups"
                     if result.passed else "; ".join(problems))
    return result


def check_compute_scales(c: Campaign) -> Result:
    result = Result("scales", "four processors deliver at least three and a half times one to independent compute (native)")
    one = result_for(c, "scale.compute", 1)
    four = result_for(c, "scale.compute", 4)
    if not one or not four or "native" not in one["backends"] or "native" not in four["backends"]:
        result.detail = "scale.compute not run on native for 1 and 4 threads"
        return result
    a = one["backends"]["native"]["pooled"]["median"]
    b = four["backends"]["native"]["pooled"]["median"]
    ratio = b / a if a else 0
    result.passed = ratio >= 3.5
    result.detail = f"four threads {b:.0f} against one thread {a:.0f} per slice: {ratio:.2f}x"
    return result


def check_budget_enforced(c: Campaign) -> Result:
    result = Result("budget", "a quarter-window budget delivered between twenty and thirty percent of each window (native)")
    item = result_for(c, "quota.share", 25)
    window = analysis.schema()["constants"]["SLICE_NS"]
    if not item or "native" not in item["backends"]:
        result.detail = "quota.share not run on native"
        return result
    median = item["backends"]["native"]["pooled"]["median"]
    share = median / window
    result.passed = 0.20 <= share <= 0.30
    others = {arm: round(b["pooled"]["median"] / window, 3) for arm, b in item["backends"].items()
              if arm != "native"}
    result.detail = f"native {share * 100:.1f}% of each window; Linux cgroup cpu.max {others}"
    return result


def check_closure_recycles(c: Campaign) -> Result:
    result = Result("closure", "more units were built and closed than the kernel has table slots, every one retired (native)")
    item = result_for(c, "closure.unit", 0)
    if not item or "native" not in item["backends"]:
        result.detail = "closure.unit not run on native"
        return result
    rounds = item["backends"]["native"]["rounds"]
    smallest = min((r["n"] for r in rounds), default=0)
    problems = item["backends"]["native"]["problems"]
    result.passed = smallest > 24 and not problems
    result.detail = (f"{smallest}+ closures per round across {len(rounds)} rounds, no refusal: "
                     f"more than the sixteen domain and twenty-four scope slots"
                     if result.passed else f"smallest round {smallest} closures, problems {problems[:2]}")
    return result


def check_native_only(c: Campaign) -> Result:
    result = Result("native_only", "what the schema declares native-only was run on the native side alone and compared with nothing")
    skip = native_only_ids()
    names = {b["id"]: b["name"] for b in analysis.schema()["benchmarks"]}
    problems = []
    seen = 0
    for boot in linux_boots(c):
        for entry in boot.entries:
            if entry["bench"] in skip:
                problems.append(f"{boot.arm} r{boot.round} ran {names.get(entry['bench'])}")
    for item in c.results["results"]:
        if item["equivalence"] == "native_only":
            seen += 1
            if set(item["backends"]) != {"native"} or item["comparisons"]:
                problems.append(f"{item['bench']} has backends {sorted(item['backends'])}")
    result.passed = seen > 0 and not problems
    result.detail = (f"{seen} native-only results, native samples only, no comparison"
                     if result.passed else "; ".join(problems[:4]) or "no native-only result")
    return result


def check_schema_named(c: Campaign) -> Result:
    result = Result("schema", "every benchmark in the results is one the schema declares, with its equivalence decided there")
    table = {b["name"]: b for b in analysis.schema()["benchmarks"]}
    problems = [item["bench"] for item in c.results["results"]
                if item["bench"] not in table
                or table[item["bench"]]["equivalence"] != item["equivalence"]]
    result.passed = bool(c.results["results"]) and not problems
    result.detail = (f"{len(c.results['results'])} results, each a declared benchmark with its "
                     f"declared equivalence" if result.passed else f"undeclared or redeclared: {problems}")
    return result


def check_audit_kept_up(c: Campaign) -> Result:
    result = Result("audit", "the auditor lost no receipt and the control log never filled (native)")
    problems = []
    evidence = []
    for boot in native_boots(c):
        lost = [r for r in by_event(boot.records, "user.note")
                if r.get("name") == "supervisor" and r.number("a") == 0x610B]
        high = [r for r in by_event(boot.records, "user.note")
                if r.get("name") == "supervisor" and r.number("a") == 0x610A]
        established = by_event(boot.records, "ctrl.log_established")
        capacity = established[0].number("capacity") if established else None
        reserved = established[0].number("reserved_cells") if established else None
        if not lost or lost[-1].number("b") != 0:
            problems.append(f"r{boot.round}: receipts lost {lost[-1].number('b') if lost else 'unknown'}")
        if not high or capacity is None or (high[-1].number("b") or 0) & 0xFFFFFFFF >= capacity - (reserved or 0):
            problems.append(f"r{boot.round}: high water {high[-1].number('b') if high else 'unknown'} "
                            f"of {capacity}")
        elif not evidence:
            evidence = [line_of(lost[-1]), line_of(high[-1])]
    result.passed = bool(native_boots(c)) and not problems
    result.detail = ("every native boot: zero receipts lost, the log's high water below its ordinary cells"
                     if result.passed else "; ".join(problems[:4]))
    result.evidence = evidence
    return result


def check_regressions(verdicts: dict[str, dict | None]) -> list[Result]:
    out = []
    expected = {"k1": 13, "k2": 21, "k3": 28, "k4": 31, "k5": 47}
    for gate, total in expected.items():
        result = Result(f"regression_{gate}", f"the {gate.upper()} gate still passes on this kernel")
        verdict = verdicts.get(gate)
        if verdict is None:
            result.detail = f"no {gate} verdict found; run the gates first"
        else:
            met = sum(1 for criterion in verdict.get("criteria", []) if criterion.get("passed"))
            result.passed = bool(verdict.get("passed")) and met == total
            result.detail = f"{met} of {total} criteria"
        out.append(result)
    return out


CRITERIA = [
    check_campaign_ran,
    check_same_plan,
    check_native_is_kernel_record,
    check_trace_declared,
    check_kernel_counted_entries,
    check_no_lost_samples,
    check_engine_reference,
    check_counters_agree,
    check_linux_guest,
    check_verdicts,
    check_paired,
    check_artifacts,
    check_host_inventoried,
    check_compute_scales,
    check_budget_enforced,
    check_closure_recycles,
    check_native_only,
    check_schema_named,
    check_audit_kept_up,
]


# --- self-test ---------------------------------------------------------------


def damaged(c: Campaign, apply) -> Campaign:
    copy_ = copy.deepcopy(c)
    apply(copy_)
    return copy_


def first_native(c: Campaign) -> Boot:
    return native_boots(c)[0]


def first_linux(c: Campaign) -> Boot:
    return linux_boots(c)[0]


def drop_records(boot: Boot, event: str) -> None:
    boot.records = [r for r in boot.records if r.event != event]


def rewrite_verdict(c: Campaign, name: str, verdict: str) -> None:
    for item in c.results["results"]:
        if item["bench"] == name:
            for comparison in item["comparisons"]:
                comparison["verdict"] = verdict


def damage_lose_boot(c):
    c.boots.pop()


def damage_entries_short(c):
    first_native(c).record["entries_reported"] = 1


def damage_linux_order(c):
    b = first_linux(c)
    b.entries[0], b.entries[1] = b.entries[1], b.entries[0]


def damage_linux_seed(c):
    first_linux(c).parsed["seed"] = 1


def damage_no_supervisor(c):
    drop_records(first_native(c), "k2.supervisor_built")


def damage_module_flag(c):
    for r in first_native(c).records:
        if r.event == "loader.module" and r.get("name") == "k6super":
            r.fields["flags"] = "0x0"


def damage_sample_source(c):
    first_native(c).entries[0]["sources"] = ["nengine"]


def damage_trace_on(c):
    first_native(c).extra["diag"]["trace"] = "on"


def damage_fewer_syscalls(c):
    boot = first_native(c)
    code = analysis.notes()
    for r in by_event(boot.records, "user.note"):
        if r.get("name") == "k6client" and r.number("a") == code["END"]:
            r.fields["syscalls"] = "0"
            break


def damage_lost_sample(c):
    first_native(c).entries[0]["lost"] = 1


def damage_engine_digest(c):
    for boot in c.boots:
        for entry in boot.entries:
            if entry["bench"] == 14:
                entry["digest"] = 1
                return


def damage_engine_check(c):
    for boot in c.boots:
        for entry in boot.entries:
            if entry["bench"] == 14:
                entry["check"] = entry["requested"] - 1
                return


def damage_clock(c):
    first_native(c).parsed["tsc_hz"] = int(first_native(c).parsed["tsc_hz"] * 1.05)


def damage_kernel_release(c):
    first_linux(c).extra["guest"]["kernel_release"] = "0.0.0"


def damage_mitigations(c):
    for boot in linux_boots(c):
        if boot.arm == "linux-nomitig":
            boot.extra["guest"]["cmdline"] = "console=ttyS0"
            return


def damage_verdict_overreach(c):
    rewrite_verdict(c, "ipc.call", "NATIVE_FASTER")


def damage_verdict_inconclusive(c):
    for item in c.results["results"]:
        if item["equivalence"] == "equivalent":
            for comparison in item["comparisons"]:
                if comparison["verdict"] in ("NATIVE_FASTER", "NATIVE_SLOWER"):
                    comparison["verdict"] = "INCONCLUSIVE"
                    return


def damage_paired(c):
    c.results["results"][0]["comparisons"][0]["paired_rounds"] = 2


def damage_kernel_sha(c):
    first_native(c).record["kernel_sha256"] = "0" * 64


def damage_accelerator(c):
    c.campaign["accelerator"] = "tcg"


def damage_inventory(c):
    c.inventory["cpu"]["model_name"] = ""


def damage_scaling(c):
    item = result_for(c, "scale.compute", 4)
    item["backends"]["native"]["pooled"]["median"] /= 2


def damage_budget(c):
    item = result_for(c, "quota.share", 25)
    item["backends"]["native"]["pooled"]["median"] *= 2


def damage_closure(c):
    item = result_for(c, "closure.unit", 0)
    item["backends"]["native"]["rounds"][0]["n"] = 10


def damage_native_only_compared(c):
    item = result_for(c, "ipc.lineage", 0)
    item["backends"]["linux"] = item["backends"]["native"]


def damage_undeclared(c):
    c.results["results"][0]["bench"] = "unknown-99"


def damage_audit_lost(c):
    for r in by_event(first_native(c).records, "user.note"):
        if r.get("name") == "supervisor" and r.number("a") == 0x610B:
            r.fields["b"] = "0x1"


DAMAGE = [
    ("a boot missing from the campaign", "ran", damage_lose_boot),
    ("a native boot that reported fewer entries than planned", "ran", damage_entries_short),
    ("a Linux guest that ran the entries in another order", "same_plan", damage_linux_order),
    ("a Linux guest that ran another plan seed", "same_plan", damage_linux_seed),
    ("no supervisor built by the kernel", "kernel_record", damage_no_supervisor),
    ("the supervisor module without the summaries-only flag", "kernel_record", damage_module_flag),
    ("samples written by the engine domain", "kernel_record", damage_sample_source),
    ("tracing left on", "trace_declared", damage_trace_on),
    ("fewer kernel entries than samples on the client", "entries_counted", damage_fewer_syscalls),
    ("a lost sample", "no_loss", damage_lost_sample),
    ("an engine answer that is not the reference", "engine_reference", damage_engine_digest),
    ("an engine answer that varied between repetitions", "engine_reference", damage_engine_check),
    ("a boot whose counter calibration disagrees by five percent", "counters", damage_clock),
    ("a Linux guest of another kernel release", "linux_guest", damage_kernel_release),
    ("the no-mitigations arm booted with mitigations", "linux_guest", damage_mitigations),
    ("a speed verdict on a merely comparable benchmark", "verdicts", damage_verdict_overreach),
    ("an INCONCLUSIVE verdict over an interval that excludes one", "verdicts", damage_verdict_inconclusive),
    ("a comparison paired over fewer rounds than run", "paired", damage_paired),
    ("two kernels in one campaign", "artifacts", damage_kernel_sha),
    ("a campaign run under TCG", "artifacts", damage_accelerator),
    ("no processor model in the host inventory", "host", damage_inventory),
    ("four threads delivering under three and a half times one", "scales", damage_scaling),
    ("a quarter budget delivering half a window", "budget", damage_budget),
    ("fewer closures than table slots", "closure", damage_closure),
    ("a native-only benchmark with Linux samples", "native_only", damage_native_only_compared),
    ("a result for a benchmark the schema does not declare", "schema", damage_undeclared),
    ("a receipt the auditor lost", "audit", damage_audit_lost),
]


def self_test(c: Campaign, verdicts: dict, quiet: bool) -> int:
    baseline = [check(c) for check in CRITERIA]
    if not all(result.passed for result in baseline):
        print("self-test needs a passing baseline; the undamaged campaign already fails",
              file=sys.stderr)
        for result in baseline:
            if not result.passed:
                print(f"  {result.name}: {result.detail}", file=sys.stderr)
        return 2
    missed = []
    for description, expected, damage in DAMAGE:
        results = {r.name: r for r in (check(damaged(c, damage)) for check in CRITERIA)}
        target = results.get(expected)
        if target is None or target.passed:
            missed.append(f"{description}: {expected} did not notice")
        elif not quiet:
            print(f"NOTICED  {expected.ljust(16)}  {description}")
    print()
    if missed:
        print(f"K6 SELF-TEST FAILED: {len(missed)} of {len(DAMAGE)} damages went unnoticed")
        for line in missed:
            print(f"  {line}")
        return 1
    print(f"K6 SELF-TEST PASSED: {len(DAMAGE)} damages, each noticed by the criterion named")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--campaign", type=Path, default=ROOT / "build/k6/campaigns/baseline")
    parser.add_argument("--gates", type=Path, default=ROOT / "build",
                        help="where the K1-K5 verdicts of the canonical (TCG) runs are")
    parser.add_argument("--json", type=Path, help="write the verdict here as well")
    parser.add_argument("--quiet", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()

    campaign = load(arguments.campaign)
    verdicts = {}
    for gate in ("k1", "k2", "k3", "k4", "k5"):
        path = arguments.gates / f"{gate}-gate.json"
        verdicts[gate] = json.loads(path.read_text()) if path.exists() else None
    if arguments.self_test:
        return self_test(campaign, verdicts, arguments.quiet)

    results = [check(campaign) for check in CRITERIA] + check_regressions(verdicts)
    for result in results:
        mark = "PASS" if result.passed else "FAIL"
        print(f"{mark}  {result.title.ljust(120)}  {result.detail}")
        if not arguments.quiet:
            for line in result.evidence:
                print(f"        {line}")
    met = sum(1 for result in results if result.passed)
    print()
    passed = met == len(results)
    if passed:
        print(f"K6 GATE PASSED: {met} of {len(results)} criteria met")
    else:
        print(f"K6 GATE FAILED: {len(results) - met} of {len(results)} criteria not met")
    if arguments.json:
        verdict = {
            "gate": "k6",
            "passed": passed,
            "campaign": str(arguments.campaign.relative_to(ROOT)) if arguments.campaign.is_relative_to(ROOT) else str(arguments.campaign),
            "criteria": [{"name": r.name, "title": r.title, "passed": r.passed, "detail": r.detail,
                          "evidence": r.evidence} for r in results],
        }
        arguments.json.write_text(json.dumps(verdict, indent=2) + "\n")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
