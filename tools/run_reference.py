#!/usr/bin/env python3
"""Ask Thalyx's own engine, on Linux, what it answers.

The reference side of K5's comparison. It starts `build/reference/thalyx-engine`
-- Thalyx's `engine/thalyx-engine.cpp`, unchanged, built by
tools/build_reference.py -- on the model the image carries, and speaks the
protocol that file defines: a ready frame, then a request frame naming a prompt
file and a grammar file, then an answer frame whose body is the prompt read
followed by the completion.

**This is host execution and not native evidence.** What it records is what a
native run is compared against: the completion the reference engine produced
for a prompt, with the same weights, the same context size, one thread, and a
greedy sampler, which is what the native engine is configured with.

Usage: tools/run_reference.py --case PROMPT PREDICT [--case ...] [--out FILE]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFERENCE = ROOT / "build" / "reference"
SCHEMA = ROOT / "abi" / "schema" / "k5-proto-v1.json"


def fixture() -> dict:
    """What the engine is asked, from the one place both backends read it."""
    return json.loads(SCHEMA.read_text())["fixtures"]["engine"]


def fixture_cases() -> list[dict]:
    return fixture()["cases"]


# What the native engine is built with, so both sides answer the same question:
# the fixture's numbers, not this file's.
CONTEXT = fixture()["context_tokens"]
THREADS = fixture()["compute_threads"]


def read_exactly(stream, n: int) -> bytes:
    data = b""
    while len(data) < n:
        chunk = stream.read(n - len(data))
        if not chunk:
            raise SystemExit("the reference engine closed its output early")
        data += chunk
    return data


def ask(engine: subprocess.Popen, workdir: Path, prompt: bytes, predict: int, seed: int,
        grammar: bytes) -> dict:
    prompt_path = workdir / "prompt.txt"
    prompt_path.write_bytes(prompt)
    grammar_path = b""
    if grammar:
        (workdir / "grammar.gbnf").write_bytes(grammar)
        grammar_path = str(workdir / "grammar.gbnf").encode()
    path = str(prompt_path).encode()
    frame = (b"THQ1" + struct.pack("<IQ", predict, seed)
             + struct.pack("<I", len(path)) + path
             + struct.pack("<I", len(grammar_path)) + grammar_path)
    engine.stdin.write(frame)
    engine.stdin.flush()
    magic = read_exactly(engine.stdout, 4)
    if magic != b"THA1":
        raise SystemExit(f"not an answer frame: {magic!r}")
    status = read_exactly(engine.stdout, 1)[0]
    elapsed_ms, = struct.unpack("<Q", read_exactly(engine.stdout, 8))
    length, = struct.unpack("<I", read_exactly(engine.stdout, 4))
    body = read_exactly(engine.stdout, length)
    record = {"status": status, "elapsed_ms": elapsed_ms}
    if status == 0:
        # The body is the prompt this process read, then the completion.
        if not body.startswith(prompt):
            raise SystemExit("the reference answer does not echo the prompt it was given")
        completion = body[len(prompt):]
        record["completion_hex"] = completion.hex()
        record["completion_sha256"] = hashlib.sha256(completion).hexdigest()
        record["completion_bytes"] = len(completion)
    else:
        record["reason"] = body.decode("utf-8", "replace")
    return record


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", nargs=2, action="append", metavar=("PROMPT", "PREDICT"),
                        required=True)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--out", type=Path, default=REFERENCE / "answers.json")
    arguments = parser.parse_args()

    engine_path = REFERENCE / "thalyx-engine"
    model = REFERENCE / "tiny.gguf"
    if not engine_path.exists() or not model.exists():
        print("run tools/build_reference.py first", file=sys.stderr)
        return 1

    engine = subprocess.Popen(
        [str(engine_path), "-m", str(model), "--ctx", str(CONTEXT), "--threads", str(THREADS)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )
    ready = read_exactly(engine.stdout, 4)
    if ready != b"THR1":
        raise SystemExit(f"not a ready frame: {ready!r}")
    load_ms, pid, threads, n_ctx = struct.unpack("<QIII", read_exactly(engine.stdout, 20))

    answers = []
    with tempfile.TemporaryDirectory() as scratch:
        for prompt, predict in arguments.case:
            answer = ask(engine, Path(scratch), prompt.encode(), int(predict), arguments.seed, b"")
            answer.update({"prompt": prompt, "predict": int(predict)})
            answers.append(answer)
    engine.stdin.close()
    engine.wait(timeout=30)

    record = {
        "note": "host execution; the Linux side of a comparison, not native evidence",
        "engine_pid": pid,
        "load_ms": load_ms,
        "threads": threads,
        "context": n_ctx,
        "answers": answers,
    }
    arguments.out.parent.mkdir(parents=True, exist_ok=True)
    arguments.out.write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
