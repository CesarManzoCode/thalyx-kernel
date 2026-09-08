#!/usr/bin/env python3
"""Finite design checks; these are not kernel implementation tests."""

from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import platform
from collections import deque
from dataclasses import dataclass, replace
from pathlib import Path


NEW, QUEUED, RECEIVED, EFFECT, ABORTED, COMMITTED = range(6)
PENDING = {QUEUED, RECEIVED, EFFECT}


@dataclass(frozen=True)
class WorkState:
    grant_open: bool = True
    scope_open: bool = True
    derived: bool = False
    phases: tuple[int, int] = (NEW, NEW)
    effect_prechecked: tuple[bool, bool] = (False, False)
    retired: bool = False


def work_edges(state: WorkState, unsafe_effect_admission: bool = False):
    if state.retired:
        return
    live = state.grant_open and state.scope_open
    if live and not state.derived:
        yield "derive", replace(state, derived=True)
    if state.grant_open:
        yield "fence_grant", replace(state, grant_open=False)
    if state.scope_open:
        yield "fence_scope", replace(state, scope_open=False)
    for index, phase in enumerate(state.phases):
        transitions = []
        if phase == NEW and state.derived and live:
            transitions.append(("enqueue", QUEUED))
        if phase == QUEUED and live:
            transitions.append(("receive", RECEIVED))
        if phase == RECEIVED and live and not state.effect_prechecked[index]:
            checked = list(state.effect_prechecked)
            checked[index] = True
            yield f"precheck_effect:{index}", replace(
                state, effect_prechecked=tuple(checked)
            )
        if (
            phase == RECEIVED
            and state.effect_prechecked[index]
            and (live or unsafe_effect_admission)
        ):
            transitions.append(("begin_effect", EFFECT))
        if phase in {QUEUED, RECEIVED}:
            transitions.append(("abort", ABORTED))
        if phase == EFFECT:
            transitions.append(("complete", COMMITTED))
        for action, new_phase in transitions:
            phases = list(state.phases)
            phases[index] = new_phase
            yield f"{action}:{index}", replace(state, phases=tuple(phases))
    if not state.scope_open and not any(p in PENDING for p in state.phases):
        yield "retire", replace(state, retired=True)


def check_work_model():
    initial = WorkState()
    queue = deque([initial])
    paths = {initial: []}
    edge_count = 0
    late_effect = None
    while queue:
        state = queue.popleft()
        for action, successor in work_edges(state):
            edge_count += 1
            live = state.grant_open and state.scope_open
            admission = action == "derive" or action.startswith(
                ("enqueue:", "begin_effect:")
            )
            assert not admission or live, (state, action)
            assert not successor.retired or (
                not successor.scope_open
                and not any(p in PENDING for p in successor.phases)
            ), successor
            assert not state.derived or successor.derived
            assert state.grant_open or not successor.grant_open
            assert state.scope_open or not successor.scope_open
            if action.startswith("complete:") and not live and late_effect is None:
                late_effect = paths[state] + [action]
            if successor not in paths:
                paths[successor] = paths[state] + [action]
                queue.append(successor)
    assert late_effect is not None, "Expected the false immediate-effect claim to fail"
    # The rival splits checking authority from admitting the effect.
    unsafe_queue = deque([(initial, [])])
    unsafe_seen = {initial}
    stale_check = None
    while unsafe_queue and stale_check is None:
        state, path = unsafe_queue.popleft()
        for action, successor in work_edges(state, unsafe_effect_admission=True):
            if action.startswith("begin_effect:") and not (
                state.grant_open and state.scope_open
            ):
                stale_check = path + [action]
                break
            if successor not in unsafe_seen:
                unsafe_seen.add(successor)
                unsafe_queue.append((successor, path + [action]))
    assert stale_check is not None, "Expected split check/admission to fail"
    return {
        "status": "PASS_WITH_EXPECTED_COUNTEREXAMPLE",
        "states": len(paths),
        "transitions": edge_count,
        "bounds": {"scopes": 1, "grant_derivations": 1, "invocations": 2},
        "checked": [
            "no admission under a fenced grant or scope",
            "no retirement with modeled tickets outstanding",
            "fences and grant lineage are monotonic",
        ],
        "false_claim": "No effect may complete after a fence",
        "counterexample": late_effect,
        "unsafe_check_then_admit_counterexample": stale_check,
    }


PARTS = {"P0", "P1", "D0", "D1", "C0", "C1"}
DEPENDENCIES = {"P0", "P1", "D0", "D1"}
COMMIT_PARTS = {"C0", "C1"}


def subsets(items):
    items = sorted(items)
    for size in range(len(items) + 1):
        for subset in itertools.combinations(items, size):
            yield set(subset)


def crash_cases(steps):
    written = set()
    guaranteed = set()
    acknowledged = False
    prefixes = [("initial", set(), set(), False)]
    for step in steps:
        if step in PARTS:
            written.add(step)
        elif step.startswith("flush"):
            guaranteed = written.copy()
        elif step == "ack":
            acknowledged = True
        else:
            raise ValueError(step)
        prefixes.append(
            (step, written.copy(), guaranteed.copy(), acknowledged)
        )
    for index, (step, possible, durable, ack) in enumerate(prefixes):
        for extra in subsets(possible - durable):
            persisted = durable | extra
            commit_complete = COMMIT_PARTS <= persisted
            dependencies_complete = DEPENDENCIES <= persisted
            recovered_new = commit_complete and dependencies_complete
            violations = []
            if commit_complete and not dependencies_complete:
                violations.append("complete commit without durable dependencies")
            if ack and not recovered_new:
                violations.append("acknowledged publication lost after crash")
            yield {
                "boundary": index,
                "after": step,
                "persisted": sorted(persisted),
                "acknowledged": ack,
                "recovered_root": "new" if recovered_new else "old",
                "violations": violations,
            }


def check_persistence_model():
    correct = [
        "P0", "P1", "flush_prepare",
        "D0", "D1", "flush_data",
        "C0", "C1", "flush_commit", "ack",
    ]
    cases = list(crash_cases(correct))
    assert all(not case["violations"] for case in cases), cases
    variants = {
        "missing_data_flush": [
            "P0", "P1", "flush_prepare", "D0", "D1",
            "C0", "C1", "flush_commit", "ack",
        ],
        "ack_before_commit_flush": [
            "P0", "P1", "flush_prepare", "D0", "D1", "flush_data",
            "C0", "C1", "ack", "flush_commit",
        ],
    }
    counterexamples = {}
    for name, steps in variants.items():
        variant_cases = list(crash_cases(steps))
        failing = next((case for case in variant_cases if case["violations"]), None)
        assert failing is not None, f"Negative control did not fail: {name}"
        counterexamples[name] = {
            "cases": len(variant_cases),
            "counterexample": failing,
        }
    assert {case["recovered_root"] for case in cases} == {"old", "new"}
    return {
        "status": "PASS_WITH_EXPECTED_COUNTEREXAMPLES",
        "crash_cases": len(cases),
        "bounds": {"publications": 1, "persistable_fragments": 6},
        "negative_controls": counterexamples,
    }


def check_aba_case():
    expected = {"content": "A", "generation": 0}
    history = [
        {"content": "A", "generation": 0},
        {"content": "B", "generation": 1},
        {"content": "A", "generation": 2},
    ]
    current = history[-1]
    content_only_accepts = current["content"] == expected["content"]
    generation_accepts = current["generation"] == expected["generation"]
    assert content_only_accepts and not generation_accepts
    return {
        "status": "EXPECTED_COUNTEREXAMPLE",
        "history": history,
        "content_only_accepts_stale_expectation": content_only_accepts,
        "generation_accepts_stale_expectation": generation_accepts,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    script_hash = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    report = {
        "schema_version": 1,
        "evidence_kind": "bounded_design_models_not_kernel_verification",
        "python": platform.python_version(),
        "script_sha256": script_hash,
        "MODEL-01": check_work_model(),
        "MODEL-02": check_persistence_model(),
        "ABA": check_aba_case(),
    }
    encoded = json.dumps(report, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        args.output.write_text(encoded, encoding="utf-8")
    print(encoded, end="")


if __name__ == "__main__":
    main()
