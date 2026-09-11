#!/usr/bin/env python3
"""Read K6 benchmark runs and decide what they say, and only what they say.

Two kinds of log reach this file. A native run is the kernel's diagnostic
plane: a benchmark note is a `user.note` record whose `a` is a code from
abi/schema/k6-bench-v1.json and whose `b` is the value, stamped by the kernel
with the domain that wrote it. A Linux run is its guest's debug console: `K6N
<code> <value>` lines, and `K6T <key> <text>` lines that say what the guest is.
Both are parsed into the same thing -- entries, each a benchmark, a parameter
and its samples -- so the comparison never sees which parser produced a number.

What a comparison may conclude is not decided here. It is the `equivalence`
the schema declared before anything ran:

  equivalent   a difference is stated only when the confidence interval of
               the paired ratio excludes one; otherwise INCONCLUSIVE.
  comparable   the ratio and its interval are reported as the cost of one
               primitive with its guarantees against the other with its own;
               no statement that one kernel is faster.
  different    both numbers, no ratio interpreted.
  native_only  a characterisation; no comparison.

The statistics are chosen for what the design can support. A round is one
boot of each backend, in an order drawn at random per round, so the pairing
unit is the round and the confidence intervals are bootstrap intervals over
rounds: resampling rounds keeps a boot's own samples together, which is what
their correlation demands. Within a boot the median is the location statistic;
p95 is reported only with at least 200 samples and p99 only with at least 1000
-- ten beyond the percentile, so a p99 is never one sample's opinion.
"""

from __future__ import annotations

import hashlib
import json
import math
import random
import re
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "abi/schema/k6-bench-v1.json"

NOTE_RE = re.compile(
    r"^THLX1 kernel (\d+) (\S+) user\.note domain=(\d+) name=(\S+) thread=\d+ "
    r"kind=self_check kind_id=2 a=0x([0-9a-f]+) b=0x([0-9a-f]+)"
)
DIAG_RE = re.compile(r"^THLX1 kernel \d+ \S+ diag\.summary (.*)$")
KV_RE = re.compile(r"(\w+)=(\S+)")

P95_MIN = 200
P99_MIN = 1000
BOOTSTRAP = 2000


def schema() -> dict:
    return json.loads(SCHEMA.read_text())


def benchmarks() -> dict[int, dict]:
    return {bench["id"]: bench for bench in schema()["benchmarks"]}


def notes() -> dict[str, int]:
    return {name: item["value"] for name, item in schema()["notes"].items()}


def fnv1a(data: bytes) -> int:
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


# --------------------------------------------------------------- parsing


def native_stream(text: str) -> tuple[list[tuple[int, int, str]], dict]:
    """Benchmark notes of a native log, in the kernel's order, and its diag cost."""
    k6 = set(notes().values())
    stream = []
    diag: dict = {}
    for line in text.splitlines():
        match = NOTE_RE.match(line)
        if match:
            code = int(match.group(5), 16)
            if code in k6:
                stream.append((code, int(match.group(6), 16), match.group(4)))
            continue
        summary = DIAG_RE.match(line)
        if summary:
            diag = {key: _number(value) for key, value in KV_RE.findall(summary.group(1))}
    return stream, diag


def linux_stream(text: str) -> tuple[list[tuple[int, int, str]], dict]:
    """Benchmark notes of a Linux guest's debug console, and what it said it is."""
    stream = []
    info: dict = {"vulnerabilities": {}}
    for line in text.splitlines():
        if line.startswith("K6N "):
            parts = line.split()
            if len(parts) == 3:
                stream.append((int(parts[1], 16), int(parts[2], 16), "linux"))
        elif line.startswith("K6T "):
            _, key, *rest = line.split(" ", 2)
            value = rest[0] if rest else ""
            if key == "vulnerability":
                name, _, text_value = value.partition(" ")
                info["vulnerabilities"][name] = text_value
            else:
                info[key] = value
    return stream, info


def _number(value: str):
    try:
        return int(value)
    except ValueError:
        return value


def entries_of(stream: list[tuple[int, int, str]]) -> dict:
    """Groups a note stream into entries: BEGIN opens one, END closes it, and
    everything between belongs to it whichever domain wrote it."""
    code = notes()
    lost = schema()["constants"]["LOST_SAMPLE"]
    run: dict = {"tsc_hz": None, "seed": None, "done": None, "entries": [], "orphans": 0}
    current = None
    for note, value, source in stream:
        if note == code["PLAN"]:
            run["seed"] = value
        elif note == code["TSC_HZ"]:
            run["tsc_hz"] = value
        elif note == code["DONE"]:
            run["done"] = value
        elif note == code["BEGIN"]:
            current = {"bench": value & 0xFFFF, "param": (value >> 16) & 0xFFFF,
                       "requested": value >> 32, "samples": [], "lost": 0, "errors": 0,
                       "error_status": [], "aux": [], "digest": None, "check": None,
                       "mismatch": [], "sources": set()}
        elif current is None:
            run["orphans"] += 1
        elif note == code["SAMPLE"]:
            current["sources"].add(source)
            for half in (value & 0xFFFFFFFF, value >> 32):
                if half == lost:
                    current["lost"] += 1
                else:
                    current["samples"].append(half)
        elif note == code["ERROR"]:
            status = (value >> 32) & 0xFFFFFFFF
            current["error_status"].append(status - (1 << 32) if status >= 1 << 31 else status)
        elif note == code["AUX"]:
            current["aux"].append(((value >> 16) & 0xFFFF, value >> 32))
        elif note == code["DIGEST"]:
            current["digest"] = value
        elif note == code["CHECK"]:
            current["check"] = value >> 32
        elif note == code["MISMATCH"]:
            current["mismatch"].append(value)
        elif note == code["END"]:
            current["errors"] = value >> 32
            # Samples are sent two to a note, so an odd count ends with one
            # the sender marked lost. That one was never a sample.
            current["lost"] = max(0, current["lost"] - (len(current["samples"]) + current["lost"]
                                                         - current["requested"]))
            current["sources"] = sorted(current["sources"])
            run["entries"].append(current)
            current = None
    return run


# ------------------------------------------------------------ statistics


def quantile(sorted_values: list[float], q: float) -> float:
    if not sorted_values:
        return math.nan
    position = (len(sorted_values) - 1) * q
    low = math.floor(position)
    high = math.ceil(position)
    if low == high:
        return sorted_values[low]
    return sorted_values[low] + (sorted_values[high] - sorted_values[low]) * (position - low)


def describe(values: list[float]) -> dict:
    ordered = sorted(values)
    n = len(ordered)
    out = {"n": n}
    if n == 0:
        return out
    out["median"] = quantile(ordered, 0.5)
    out["mean"] = statistics.fmean(ordered)
    out["min"] = ordered[0]
    out["max"] = ordered[-1]
    out["cv"] = (statistics.pstdev(ordered) / out["mean"]) if out["mean"] else math.nan
    out["p95"] = quantile(ordered, 0.95) if n >= P95_MIN else None
    out["p99"] = quantile(ordered, 0.99) if n >= P99_MIN else None
    return out


def bootstrap_ci(values: list[float], statistic, seed: int, level: float = 0.95) -> tuple:
    """Percentile bootstrap over the given units (rounds)."""
    if len(values) < 2:
        return (math.nan, math.nan)
    rng = random.Random(seed)
    draws = sorted(statistic([rng.choice(values) for _ in values]) for _ in range(BOOTSTRAP))
    low = draws[int((1 - level) / 2 * BOOTSTRAP)]
    high = draws[min(BOOTSTRAP - 1, int((1 + level) / 2 * BOOTSTRAP))]
    return (low, high)


def geometric_mean(values: list[float]) -> float:
    positive = [v for v in values if v > 0]
    if not positive:
        return math.nan
    return math.exp(statistics.fmean(math.log(v) for v in positive))


def to_unit(value: float, unit: str, tsc_hz: int | None) -> float:
    """Cycles become nanoseconds by the calibration the backend reported."""
    if unit == "cycles":
        return value * 1e9 / tsc_hz if tsc_hz else math.nan
    return float(value)


def reported_unit(unit: str) -> str:
    return "ns" if unit == "cycles" else unit


# ------------------------------------------------------------- analysis


def reference_digests() -> dict[int, dict]:
    """FNV-1a of what Thalyx's engine answers on Linux, per fixture prompt."""
    fixture = json.loads((ROOT / "abi/schema/k5-proto-v1.json").read_text())["fixtures"]["engine"]
    flat = [(prompt, case["predict"]) for case in fixture["cases"] for prompt in case["prompts"]]
    answers = {}
    for candidate in (ROOT / "build/k5-runs/engine/reference.json",
                      ROOT / "build/k5-cases/reference.json"):
        if candidate.exists():
            for answer in json.loads(candidate.read_text()).get("answers", []):
                if "completion_hex" in answer:
                    answers[(answer["prompt"], answer["predict"])] = bytes.fromhex(answer["completion_hex"])
    out = {}
    for index, key in enumerate(flat):
        if key in answers:
            out[index] = {"prompt": key[0], "predict": key[1], "bytes": len(answers[key]),
                          "fnv1a": fnv1a(answers[key])}
    return out


def analyze(runs: list[dict]) -> dict:
    """Runs are dicts: backend, round, parsed (from `entries_of`), meta."""
    table = benchmarks()
    references = reference_digests()
    backends = sorted({run["backend"] for run in runs})
    keys = sorted({(e["bench"], e["param"]) for run in runs for e in run["parsed"]["entries"]})
    results = []
    for bench_id, param in keys:
        bench = table.get(bench_id, {"name": f"unknown-{bench_id}", "unit": "cycles",
                                      "equivalence": "different"})
        unit = bench["unit"]
        per_backend = {}
        for backend in backends:
            rounds = []
            pooled: list[float] = []
            problems = []
            for run in runs:
                if run["backend"] != backend:
                    continue
                for entry in run["parsed"]["entries"]:
                    if (entry["bench"], entry["param"]) != (bench_id, param):
                        continue
                    values = [to_unit(v, unit, run["parsed"]["tsc_hz"]) for v in entry["samples"]]
                    pooled += values
                    if entry["errors"] or entry["lost"]:
                        problems.append({"round": run["round"], "errors": entry["errors"],
                                         "lost": entry["lost"], "status": entry["error_status"]})
                    engine = None
                    if bench["name"] == "engine.infer" and entry["digest"] is not None:
                        ref = references.get(param)
                        engine = {"digest": f"{entry['digest']:016x}", "check": entry["check"],
                                  "mismatch": entry["mismatch"],
                                  "matches_reference": bool(ref) and entry["digest"] == ref["fnv1a"],
                                  "answer_bytes": next((v for q, v in entry["aux"] if q == param), None)}
                    rounds.append({"round": run["round"], "n": len(values),
                                   "median": quantile(sorted(values), 0.5) if values else math.nan,
                                   "aux": entry["aux"], "engine": engine})
            if not rounds:
                continue
            medians = [r["median"] for r in rounds if not math.isnan(r["median"])]
            per_backend[backend] = {
                "unit": reported_unit(unit),
                "rounds": rounds,
                "pooled": describe(pooled),
                "round_medians": describe(medians),
                "round_median_ci": bootstrap_ci(medians, statistics.fmean, seed=bench_id * 1000 + param),
                "problems": problems,
            }
        comparisons = []
        native = per_backend.get("native")
        for other in [b for b in backends if b != "native"]:
            linux = per_backend.get(other)
            if not native or not linux or bench["equivalence"] == "native_only":
                continue
            by_round = {r["round"]: r["median"] for r in linux["rounds"]}
            ratios = [r["median"] / by_round[r["round"]] for r in native["rounds"]
                      if r["round"] in by_round and by_round[r["round"]] and not math.isnan(r["median"])]
            ratio = geometric_mean(ratios)
            ci = bootstrap_ci(ratios, geometric_mean, seed=bench_id * 7919 + param)
            comparisons.append({
                "against": other,
                "paired_rounds": len(ratios),
                "ratio_native_over_other": ratio,
                "ratio_ci95": ci,
                "verdict": verdict(bench, ratio, ci, len(ratios)),
            })
        results.append({"bench": bench["name"], "id": bench_id, "param": param,
                        "param_name": bench.get("param"), "family": bench.get("family"),
                        "equivalence": bench["equivalence"], "statistic": bench.get("statistic"),
                        "backends": per_backend, "comparisons": comparisons})
    return {"backends": backends, "results": results,
            "references": {str(k): v for k, v in references.items()}}


def verdict(bench: dict, ratio: float, ci: tuple, rounds: int) -> str:
    """What the schema lets this comparison say, given its interval."""
    kind = bench["equivalence"]
    if rounds < 3 or any(math.isnan(x) for x in ci):
        return "NOT_ENOUGH_ROUNDS"
    if kind == "different":
        return "NO_SPEED_CONCLUSION"
    lower_is_better = bench.get("statistic") in ("latency",)
    excludes_one = ci[0] > 1 or ci[1] < 1
    if kind == "comparable":
        return "COST_RATIO_UNDER_STATED_GUARANTEES"
    if kind == "equivalent":
        if not excludes_one:
            return "INCONCLUSIVE"
        if bench.get("statistic") == "share":
            return "DIFFERENT_ENFORCEMENT"
        native_better = (ratio < 1) if lower_is_better else (ratio > 1)
        return "NATIVE_FASTER" if native_better else "NATIVE_SLOWER"
    return "NO_SPEED_CONCLUSION"


# --------------------------------------------------------- sample sizes


def plan_sizes(runs: list[dict], precision: float = 0.01, round_precision: float = 0.02) -> dict:
    """Samples per boot and rounds, from the variation a pilot showed.

    Within a boot, the median's standard error is about 1.2533 sigma/sqrt(n),
    so n = (1.2533 cv / precision)^2 samples put it within `precision` of
    itself. Between boots, R = (t cv_rounds / round_precision)^2 rounds; t is
    taken as 2.3, a two-sided 95% value for the handful of rounds this is
    about. Both are then bounded by what a boot can hold and a sprint can run,
    and the result says when the bound, not the need, decided."""
    table = benchmarks()
    out = {}
    for bench_id, param in sorted({(e["bench"], e["param"]) for run in runs
                                   for e in run["parsed"]["entries"]}):
        cvs, medians = [], []
        for run in runs:
            for entry in run["parsed"]["entries"]:
                if (entry["bench"], entry["param"]) == (bench_id, param) and len(entry["samples"]) > 5:
                    values = entry["samples"]
                    mean = statistics.fmean(values)
                    if mean:
                        cvs.append(statistics.pstdev(values) / mean)
                    medians.append(statistics.median(values))
        if not cvs:
            continue
        cv = max(cvs)
        n_needed = math.ceil((1.2533 * cv / precision) ** 2)
        rounds_cv = (statistics.pstdev(medians) / statistics.fmean(medians)) if len(medians) > 1 else math.nan
        r_needed = math.ceil((2.3 * rounds_cv / round_precision) ** 2) if not math.isnan(rounds_cv) else None
        out[f"{table.get(bench_id, {}).get('name', bench_id)}:{param}"] = {
            "within_boot_cv": cv, "between_boot_cv": rounds_cv,
            "samples_needed": n_needed, "rounds_needed": r_needed,
        }
    return out


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()
