#!/usr/bin/env python3
"""Write the GGUF the inference engine runs, deterministically and from here.

The engine this repository ports has to run a real model file in the real
format, and the file has to be the same one an independent implementation of
that format reads, or the comparison that makes the port honest is not a
comparison. Two ways to get that were available:

  * write the file with llama.cpp's own `gguf-py`, which makes the *writer*
    llama.cpp's; or
  * write it here and make the *reader* llama.cpp's.

This does the second. The file below is produced by this script alone -- no
numpy, no checkout, no network -- and the pinned llama.cpp is what says it is a
valid GGUF, by loading it and generating from it. A file this script's author
invented that llama.cpp refused would fail loudly; one it accepts is a file
whose interpretation both implementations agree on, which is the property the
engine's evidence needs.

The shape is the one the pinned Thalyx uses to measure what an inference engine
needs of a system: two layers and sixty-four dimensions. That is a deliberate
choice and its limit is stated wherever the result is: it says which operations
an engine performs and whether two engines agree about them. It says nothing
about model quality, and the weights are pseudo-random, so nothing about the
text produced is meaningful beyond both engines producing the same text.

Usage: tools/make_model.py [--out build/native/tiny.gguf]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

GGUF_MAGIC = b"GGUF"
GGUF_VERSION = 3
ALIGNMENT = 32

# GGUF metadata value types.
T_UINT32 = 4
T_INT32 = 5
T_FLOAT32 = 6
T_STRING = 8
T_BOOL = 7
T_ARRAY = 9

# GGML tensor types.
GGML_F32 = 0

DIM = 64
LAYERS = 2
HEADS = 4
KV_HEADS = 4
FEED_FORWARD = 128
CONTEXT = 512
ROPE_EPS = 1e-5
ROPE_FREQ_BASE = 10000.0


class Splitmix:
    """One well-known integer mixer, so the weights are a function of a seed and
    of nothing else -- the same on any machine, in any language, forever."""

    def __init__(self, seed: int) -> None:
        self.state = seed & 0xFFFFFFFFFFFFFFFF

    def next_u64(self) -> int:
        self.state = (self.state + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        z = self.state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        return z ^ (z >> 31)

    def uniform(self) -> float:
        """A double in [0, 1) from the top 53 bits, the standard construction."""
        return (self.next_u64() >> 11) * (2.0 ** -53)

    def normal(self) -> float:
        """Box-Muller, so the weights are Gaussian like a trained model's."""
        u1 = max(self.uniform(), 1e-12)
        u2 = self.uniform()
        return math.sqrt(-2.0 * math.log(u1)) * math.cos(2.0 * math.pi * u2)


def string(text: str) -> bytes:
    raw = text.encode("utf-8")
    return struct.pack("<Q", len(raw)) + raw


def kv_u32(key: str, value: int) -> bytes:
    return string(key) + struct.pack("<II", T_UINT32, value)


def kv_i32(key: str, value: int) -> bytes:
    return string(key) + struct.pack("<Ii", T_INT32, value)


def kv_f32(key: str, value: float) -> bytes:
    return string(key) + struct.pack("<If", T_FLOAT32, value)


def kv_bool(key: str, value: bool) -> bytes:
    return string(key) + struct.pack("<IB", T_BOOL, 1 if value else 0)


def kv_str(key: str, value: str) -> bytes:
    return string(key) + struct.pack("<I", T_STRING) + string(value)


def kv_array_str(key: str, values: list[str]) -> bytes:
    out = string(key) + struct.pack("<IIQ", T_ARRAY, T_STRING, len(values))
    return out + b"".join(string(value) for value in values)


def kv_array_f32(key: str, values: list[float]) -> bytes:
    out = string(key) + struct.pack("<IIQ", T_ARRAY, T_FLOAT32, len(values))
    return out + struct.pack(f"<{len(values)}f", *values)


def kv_array_i32(key: str, values: list[int]) -> bytes:
    out = string(key) + struct.pack("<IIQ", T_ARRAY, T_INT32, len(values))
    return out + struct.pack(f"<{len(values)}i", *values)


def vocabulary() -> tuple[list[str], list[float], list[int]]:
    """A SentencePiece vocabulary with every byte in it.

    The 256 byte tokens are not padding: an SPM vocabulary resolves anything it
    does not know byte by byte, and one without them fails on the first
    character it has never seen. With them, any prompt at all tokenises, which
    is what lets the host choose a prompt the guest could not have precomputed.
    """
    tokens = ["<unk>", "<s>", "</s>"]
    kinds = [3, 3, 3]                      # CONTROL
    tokens += [f"<0x{byte:02X}>" for byte in range(256)]
    kinds += [6] * 256                     # BYTE
    for word in ["hola", "▁hola", "▁the", "▁a", "▁of", "▁and", "▁to"]:
        tokens.append(word)
        kinds.append(1)                    # NORMAL
    scores = [0.0] * len(tokens)
    return tokens, scores, kinds


def tensors(seed: int) -> list[tuple[str, list[int], list[float]]]:
    """Every tensor of a two-layer llama, in the order the file carries them.

    Dimensions are GGUF's: `ne[0]` runs fastest, so a matrix that maps `in`
    features to `out` features is `[in, out]` here and row-major `[out][in]` in
    memory. That is the layout `ggml_mul_mat` expects and the one the native
    engine reads back.
    """
    noise = Splitmix(seed)

    def matrix(out_features: int, in_features: int) -> list[float]:
        return [noise.normal() * 0.02 for _ in range(out_features * in_features)]

    vocab = len(vocabulary()[0])
    out: list[tuple[str, list[int], list[float]]] = []
    out.append(("token_embd.weight", [DIM, vocab], matrix(vocab, DIM)))
    for layer in range(LAYERS):
        at = f"blk.{layer}."
        # Norms are not all ones: a norm of one is a norm that cannot be got
        # wrong, and this file exists to tell two implementations apart.
        out.append((at + "attn_norm.weight", [DIM],
                    [1.0 + noise.normal() * 0.05 for _ in range(DIM)]))
        out.append((at + "attn_q.weight", [DIM, DIM], matrix(DIM, DIM)))
        out.append((at + "attn_k.weight", [DIM, DIM // HEADS * KV_HEADS],
                    matrix(DIM // HEADS * KV_HEADS, DIM)))
        out.append((at + "attn_v.weight", [DIM, DIM // HEADS * KV_HEADS],
                    matrix(DIM // HEADS * KV_HEADS, DIM)))
        out.append((at + "attn_output.weight", [DIM, DIM], matrix(DIM, DIM)))
        out.append((at + "ffn_norm.weight", [DIM],
                    [1.0 + noise.normal() * 0.05 for _ in range(DIM)]))
        out.append((at + "ffn_gate.weight", [DIM, FEED_FORWARD], matrix(FEED_FORWARD, DIM)))
        out.append((at + "ffn_up.weight", [DIM, FEED_FORWARD], matrix(FEED_FORWARD, DIM)))
        out.append((at + "ffn_down.weight", [FEED_FORWARD, DIM], matrix(DIM, FEED_FORWARD)))
    out.append(("output_norm.weight", [DIM],
                [1.0 + noise.normal() * 0.05 for _ in range(DIM)]))
    out.append(("output.weight", [DIM, vocab], matrix(vocab, DIM)))
    return out


def build(seed: int) -> bytes:
    tokens, scores, kinds = vocabulary()
    body = tensors(seed)

    metadata = b"".join([
        kv_str("general.architecture", "llama"),
        kv_str("general.name", "thalyx-kernel-k5-tiny"),
        kv_u32("general.file_type", 0),
        kv_u32("llama.context_length", CONTEXT),
        kv_u32("llama.embedding_length", DIM),
        kv_u32("llama.block_count", LAYERS),
        kv_u32("llama.feed_forward_length", FEED_FORWARD),
        kv_u32("llama.attention.head_count", HEADS),
        kv_u32("llama.attention.head_count_kv", KV_HEADS),
        kv_u32("llama.rope.dimension_count", DIM // HEADS),
        kv_f32("llama.attention.layer_norm_rms_epsilon", ROPE_EPS),
        kv_f32("llama.rope.freq_base", ROPE_FREQ_BASE),
        kv_u32("general.alignment", ALIGNMENT),
        kv_str("tokenizer.ggml.model", "llama"),
        kv_array_str("tokenizer.ggml.tokens", tokens),
        kv_array_f32("tokenizer.ggml.scores", scores),
        kv_array_i32("tokenizer.ggml.token_type", kinds),
        kv_u32("tokenizer.ggml.bos_token_id", 1),
        kv_u32("tokenizer.ggml.eos_token_id", 2),
        kv_u32("tokenizer.ggml.unknown_token_id", 0),
        kv_bool("tokenizer.ggml.add_bos_token", True),
        kv_bool("tokenizer.ggml.add_eos_token", False),
    ])
    metadata_count = 22

    infos = b""
    offset = 0
    for name, dims, values in body:
        infos += string(name)
        infos += struct.pack("<I", len(dims))
        infos += b"".join(struct.pack("<Q", dim) for dim in dims)
        infos += struct.pack("<IQ", GGML_F32, offset)
        offset += 4 * len(values)
        offset = (offset + ALIGNMENT - 1) // ALIGNMENT * ALIGNMENT

    head = GGUF_MAGIC + struct.pack("<IQQ", GGUF_VERSION, len(body), metadata_count)
    head += metadata + infos
    padding = (-len(head)) % ALIGNMENT
    head += b"\0" * padding

    data = b""
    for _, _, values in body:
        raw = struct.pack(f"<{len(values)}f", *values)
        data += raw + b"\0" * ((-len(raw)) % ALIGNMENT)
    return head + data


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "build/native/tiny.gguf")
    parser.add_argument("--seed", type=lambda text: int(text, 0), default=0x7A11)
    arguments = parser.parse_args()

    blob = build(arguments.seed)
    arguments.out.parent.mkdir(parents=True, exist_ok=True)
    arguments.out.write_bytes(blob)
    record = {
        "path": str(arguments.out),
        "bytes": len(blob),
        "sha256": hashlib.sha256(blob).hexdigest(),
        "seed": arguments.seed,
        "architecture": "llama",
        "layers": LAYERS,
        "embedding": DIM,
        "heads": HEADS,
        "kv_heads": KV_HEADS,
        "feed_forward": FEED_FORWARD,
        "context": CONTEXT,
        "vocabulary": len(vocabulary()[0]),
        "note": "pseudo-random weights; nothing about output quality is claimed",
    }
    print(json.dumps(record, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
