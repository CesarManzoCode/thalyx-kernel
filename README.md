# Thalyx-Kernel

A capability microkernel for running agent work under explicitly bounded authority, resources, and effects — with the resulting cost measured against Linux, not assumed.

Thalyx-Kernel is a from-scratch x86_64 microkernel built to give a specific class of program — a supervising system that dispatches delegated work to untrusted or semi-trusted agents — primitives that Linux does not offer as first-class kernel objects: capability-scoped authority, attributable IPC, versioned durable state, and per-unit-of-work resource accounting. **Thalyx** is its reference consumer: an agent runtime whose Linux implementation defines the semantics this kernel is built to carry. The two projects are independent repositories; Thalyx-Kernel does not depend on Thalyx code, and Linux remains Thalyx's production platform.

## Why this exists

A system that runs work on behalf of an agent — with its own budget, its own capability set, and its own failure domain — is not well served by a general-purpose process model where authority is ambient (uid/gid, open file descriptors, `ptrace`) and accounting is per-process rather than per-unit-of-work. Thalyx-Kernel's founding bet, recorded in [`vault/constitution.md`](vault/constitution.md), is that these properties are cheaper and more honest to build as kernel primitives than to simulate on top of Linux. K0–K6 is the test of that bet: build the primitives, run real workloads over them, and measure the result against Linux on the same hardware.

## Architecture: capability microkernel, explicit work-scopes

- **Capabilities, not ambient authority.** Every object — memory, IPC endpoint, device, log — is reached through a capability: a generational, revocable handle carrying its own rights mask. There is no global namespace a domain can reach into by name.
- **Work-scopes.** A *scope* is the unit that bounds an operation's authority, its aggregate CPU budget, and its lifetime. Authority delegated into a scope dies when the scope is fenced and drained; work started under it can be cancelled by closing the scope, not by finding and killing a process tree.
- **Attributable IPC.** Every call carries its origin, its causal parent, and a charge; a control-log receipt is written for it. Work can cross a service boundary (client → server → engine) without losing the identity of who is paying and who is accountable.
- **Domains as the adversarial boundary.** User code runs in ring 3, in a separate address space, preempted by timer; a domain that misbehaves faults and is torn down without taking the kernel or unrelated domains with it.
- **A user-space state service, not a kernel filesystem.** Durable state (K4) is versioned, immutable-once-published objects served from user space over the same IPC and capability model everything else uses — publication is conditional (compare-and-swap on a generation), and recovery is driven by what a crash actually left on the medium, never by a flag the writer set.

Interpretation of intent, human policy, model behavior, and domain knowledge are explicitly **not** kernel concerns — they belong to the consumer (Thalyx) running on top. See [`vault/architecture/overview.md`](vault/architecture/overview.md) for the full derivation and the thirteen architecture contracts it links.

## Status: K0–K6 complete, in their stated scope

Six phases have run on this kernel, each gated by an independent checker reading kernel logs and (from K4 on) raw medium bytes — not the program's own claims about itself:

| Phase | What it demonstrated | Gate |
|---|---|---|
| K1 | Protected boot: ring-3 domains, timer preemption, illegal accesses contained | 13/13 |
| K2 | A working capability system: 51/51 scheduled interface operations exercised, authority delegation and revocation, attributable work | 21/21, self-test 28/28 |
| K3 | SMP (4 processors) and a real device: cross-CPU scheduling, TLB shootdown, a user-space virtio-blk driver | 28/28, self-test 57/57 |
| K4 | Durable managed state: versioned publication over a block medium that survives a crash at every write point in the matrix | 31/31, self-test 48/48 |
| K5 | The Thalyx port: real QuickJS, a real native validation tool, llama.cpp as a resident engine, under rivals/cancellation/crashes | 47/47, self-test 88/88 |
| K6 | Comparison and hardening: 17 paired native/Linux benchmarks, 8 bottlenecks found and fixed by measuring, a KVM reference campaign | 19/19 + 5 regressions, self-test 27/27 |

Each gate also re-runs every prior gate's criteria against the same kernel binary — K1's 13 criteria still pass compiled into the K6 kernel, and so on. Every image, K1 through K6, is the same kernel binary with a different user-space package selected at boot. Full narrative and limits for each phase: [K1](vault/evidence/k1-protected-boot.md) · [K2](vault/evidence/k2-objects-authority-work.md) · [K3](vault/evidence/k3-smp-devices.md) · [K4](vault/evidence/k4-durable-state.md) · [K5](vault/evidence/k5-thalyx-port.md) · [K6](vault/evidence/k6-comparison-hardening.md). The single current-state checkpoint is [`vault/roadmap/current-state.md`](vault/roadmap/current-state.md).

## K6 results: native vs. Linux, on the same KVM virtual machine

K6 built one benchmark source (`tests/k6/bench.c`) compiled against two backends — this kernel with a native supervisor and Thalyx's real engine, and Linux (the host's own kernel) as a guest running the same engine unmodified — and ran a reference campaign of six rounds × three arms (`native`, `linux`, `linux-nomitig`) on a fixed QEMU/KVM machine. Every benchmark declares, before measurement, what it is entitled to conclude: *equivalent* (same contract, a real speed difference), *comparable* (same question, different guarantees — a cost, never a "faster"), or *distinct* (no speed conclusion). Selected results (native ÷ Linux, KVM, median):

- **IPC round trip:** 2.7 µs native vs. 6.5 µs Linux (`SEQPACKET` + credentials) — comparable, not equivalent: the native call additionally stamps origin/scope/facet/causal-parent and writes an auditable receipt.
- **Mapping / sealing / capability derivation:** native costs 0.46×–4.7× the Linux equivalents depending on operation, each difference attributed to a stated guarantee difference (write-fault-free, cross-CPU acknowledged unmap, etc.).
- **Compute scaling (4 CPUs, no kernel entries):** 3.95× native vs. 4.0× Linux — the two scale the same way.
- **IPC scaling (4 pairs):** 1,806 round trips/10 ms native vs. 11,912 Linux — **IPC does not scale** on this kernel; a single machine-wide lock serializes every kernel entry that touches an object. This is the largest limitation K6 measured, not one it fixed.
- **Engine inference, byte-identical output to Thalyx's Linux reference in every repetition:** native ~10% slower (1.8 ms vs. 1.63 ms for a fixed prompt/model).

Full tables, confidence intervals, the eight bottlenecks found and fixed while measuring, and exactly what is and isn't claimed: [`vault/evidence/k6-comparison-hardening.md`](vault/evidence/k6-comparison-hardening.md).

## What is not demonstrated

In order of how close each is to being closed:

- **IPC scalability.** A single global lock serializes every object-touching kernel entry. K6 measured this precisely and removed the starvation it caused (a ticket lock), but did not shard it — that is a redesign with its own evidence burden, tracked as [OQ-04](vault/roadmap/open-questions.md).
- **Physical hardware.** Every result above, K1 through K6, is QEMU: TCG for the canonical gates, KVM for K6 and for a second-platform run of K1–K5. KVM executes guest instructions on the physical CPU, but every device, the firmware, and interrupt delivery are still emulated. The development machine is inventoried (`tools/inventory_host.py`) but this kernel has never booted on it outside a VM.
- **DMA isolation.** No IOMMU is programmed. The one DMA profile this platform supports is declared `WEAK_TRUSTED_DRIVER` (`enforced_by=nothing_driver_is_trusted`); the strong/IOMMU-backed profile is refused outright with its own status code, including when a DMAR unit is described but not programmed. An untrusted DMA-capable driver is **not** contained here.
- **Power-loss durability.** K4 proves the store survives the *writer* disappearing at any point QEMU can stop it — the guest driver's own fault injection. What a real controller's write cache does, and what actually survives mains power loss, has not been measured and is not claimed.
- **Thalyx entire, on this kernel.** K5 runs Thalyx's real semantics (version, context, tool validation, resident engine) with real components (QuickJS, llama.cpp), not the Thalyx binary itself — no Rust compiler or type checker runs inside a domain.

The complete, actively maintained list — with the provisional decision used to keep moving and exactly what would close each — is [`vault/roadmap/open-questions.md`](vault/roadmap/open-questions.md).

## Repository layout

```
kernel/          the kernel: arch/x86_64, scheduler, IPC, memory objects, capability table, syscall dispatch
boot/uefi/       UEFI loader (ELF + boot-package validation, initial page tables, handoff)
boot/protocol/   boot handoff structures shared by the loader and the kernel
abi/             the single schema (abi/schema/*.json) generating Rust bindings, a C header, and byte-exact fixtures
user/            user-space packages per phase (k2super/k2server/k2client, k3*, k4*, k5*, k6super) and shared libraries (rt, k4fmt, k5pkg)
user/native/     a from-scratch libc for the x86_64-thalyx target (no glibc, no libc dependency at all)
tools/           build, run, and gate tooling — one script per concern, see below
tests/k6/        the single benchmark source compiled against both the native and Linux backends
research/models/ two small finite-state models (with negative controls) used as research aids, not kernel proof
vault/           the foundational design record: constitution, 13 architecture contracts, ADRs, invariants, per-phase evidence
```

## Prerequisites

- Rust toolchain **exactly** `1.98.1` (pinned in [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` installs it automatically on first `cargo` invocation), with the `x86_64-unknown-none` and `x86_64-unknown-uefi` targets and the `rust-src`, `llvm-tools`, `clippy`, `rustfmt` components — all listed in that file.
- Python 3 (no third-party packages) for the build/gate tooling under `tools/`.
- `qemu-system-x86_64`, an OVMF UEFI firmware pair (code + vars), and `mformat`/`mmd`/`mcopy` (from `mtools`) to build and run any image. These are resolved through `tools/toolchain.py`; if they aren't on the system path, point `THALYX_TOOL_PREFIX` at a prefix containing `usr/bin` and `usr/lib` with them, or set `THALYX_QEMU`, `THALYX_OVMF_CODE`, `THALYX_OVMF_VARS`, `THALYX_MFORMAT`, `THALYX_MMD`, `THALYX_MCOPY` individually.
- Hardware virtualization (KVM on Linux) only for the K6 timing campaign and for running the K1–K5 gates on a second platform for comparison — every canonical gate result is gathered on plain TCG and needs no virtualization extensions.

## Build

```sh
cargo build --workspace                       # every crate, host-checkable subset
cargo check --target x86_64-unknown-none      # kernel + bare-metal user crates
cargo check -p thalyx-boot-uefi --target x86_64-unknown-uefi
```

A bootable image for a given phase is produced by the single image builder, which invokes the pinned toolchain and packages loader + kernel + user package into a FAT boot medium:

```sh
python3 tools/build_image.py --phase k1        # k1 .. k6
```

`--phase k5` additionally takes `--stage smoke|surface|work|engine` (see [`vault/evidence/k5-thalyx-port.md`](vault/evidence/k5-thalyx-port.md) for what each stage runs). K5's `work` and `engine` stages and K6 first fetch pinned, digest-checked third-party sources (QuickJS, llama.cpp) via `tools/fetch_quickjs.py` / `tools/fetch_engine.py`; nothing is vendored into this repository.

## Run

```sh
python3 tools/run_k1.py            # boots build/image/k1.img in QEMU, captures serial output to build/run/
python3 tools/run_k2.py            # and run_k3.py / run_k4.py / run_k5.py / run_k6.py for the later phases
```

Each runner builds its own image if one isn't already present, boots it under QEMU (TCG by default), and writes the raw serial log and any run metadata a gate reads. Set `THALYX_ACCEL=kvm` to run under hardware virtualization instead — this is required for the K6 campaign and is recorded as a distinct platform, never merged with TCG results.

## Validate: reproducing each gate's verdict

A gate is a script that reads kernel logs (and, from K4 on, decoded medium bytes) and decides a fixed list of criteria independently — not the running program's own opinion of itself.

```sh
python3 tools/check_k1.py          # 13/13
python3 tools/check_k2.py          # 21/21, --self-test for the 28-damage self-check
python3 tools/check_k3.py          # 28/28, --self-test for 57
python3 tools/check_k4_format.py   # 12/12 — the durable store format, checked before anything else in K4
python3 tools/check_k4.py          # 31/31, --self-test for 48
python3 tools/check_k5.py          # 47/47, --self-test for 88
python3 tools/check_k6.py          # 19/19 + 5 regressions, --self-test for 27

python3 tools/run_gates.py --platform tcg      # K1 → K5 in order, one platform, into build/
python3 tools/run_gates.py --platform kvm      # the same chain under KVM, into build/platforms/kvm/
```

These require the QEMU/OVMF/mtools toolchain above and are not run in this repository's hosted CI (GitHub-hosted runners provide no KVM and no reliable TCG boot budget for a six-phase chain). They are the authoritative local reproduction path — CI checks a fast, deterministic subset instead (below).

Independent of any phase, the interface schema, the vault, and the two research models each have their own checker with no QEMU dependency:

```sh
python3 tools/check_abi.py                 # abi/schema/v0.json ↔ generated Rust/C/fixtures
python3 tools/check_k4_format.py           # abi/schema/k4-store-v1.json ↔ generated store format
python3 tools/check_k5_proto.py            # abi/schema/k5-proto-v1.json ↔ generated protocol bindings
python3 tools/check_k6_bench.py            # abi/schema/k6-bench-v1.json ↔ generated benchmark headers
python3 tools/check_vault.py               # vault note metadata and local links
python3 research/models/check_models.py    # the two finite-state models, with negative controls
```

## Reproducing the K6 reference campaign

```sh
export THALYX_ACCEL=kvm
python3 tools/run_k6.py --label baseline --rounds 6 --arms native linux linux-nomitig
python3 tools/check_k6.py
```

`run_k6.py` boots every round and arm and writes `results.json` itself, using the statistics in `tools/k6_analysis.py` (a shared module, not a standalone script — `check_k6.py` imports the same module to verify the verdicts rather than trusting the campaign's own numbers).

This needs KVM, a dedicated machine (the seal-against-live-writer and cancellation criteria in K3/K5 depend on real races that a loaded host shifts — never run two QEMU jobs at once), and takes on the order of tens of minutes for a six-round, three-arm campaign. `tools/inventory_host.py` records the physical machine the campaign ran on; it does not make that machine the platform being measured — the guest still runs under KVM. The published reference campaign's artifacts (plan, digests, results, host inventory) are checked in at [`vault/evidence/k6/baseline/`](vault/evidence/k6/baseline/); the 15 MB of raw per-boot serial logs are not, and their digests are recorded in `log-digests.json` for anyone who reruns the campaign to check against.

## Where the design record lives

This repository carries its design history in the open, as a Markdown vault under [`vault/`](vault/README.md), written in Spanish (code, identifiers, and commit messages are English — see [`AGENTS.md`](AGENTS.md)):

- [`vault/constitution.md`](vault/constitution.md) — purpose, required properties, and what would fall outside this project's scope.
- [`vault/decisions/`](vault/decisions/README.md) — ten ADRs recording what was decided, the alternatives considered, and why.
- [`vault/architecture/`](vault/architecture/overview.md) — thirteen contracts (objects, authority, IPC, memory, resources, persistence, concurrency, hardware, ABI, and more), each stating invariant → mechanism → required validation.
- [`vault/evidence/`](vault/evidence/history.md) — one note per phase, stating exactly what ran, on what platform, and what it does not prove.
- [`vault/roadmap/open-questions.md`](vault/roadmap/open-questions.md) — every open uncertainty, the provisional decision in force, and what would resolve it.

The vault distinguishes **observed fact** (a specific execution, cited), **accepted decision** (binds the first implementation, revisable with evidence), **designed contract** (required behavior, not yet necessarily built), and **hypothesis** (needs an experiment) — a model result or a successful boot is never read as proof of the whole architecture.

## Project status and what comes after K6

K0–K6 are frozen for this repository-polish work: no new kernel architecture, no attempt at the IPC lock bottleneck, no new features, no benchmark re-runs, in this pass. There is no K7 defined yet in [`vault/roadmap/phases.md`](vault/roadmap/phases.md); [`vault/roadmap/current-state.md`](vault/roadmap/current-state.md) names, in weight order, what K6's own evidence puts first: splitting the machine-wide lock ([OQ-04](vault/roadmap/open-questions.md)), booting on the inventoried physical machine ([OQ-05](vault/roadmap/open-questions.md)), and diagnosing the long tail in scope closure latency. Development after this point follows [`CONTRIBUTING.md`](CONTRIBUTING.md): short-lived branches from `main`, coherent commits, the checks above before a PR, squash merge.

## License

This repository does not yet carry a license (`license = "NOASSERTION"` in `Cargo.toml`, deliberately — see [OQ-14](vault/roadmap/open-questions.md)). Until an owner decision fixes distribution terms, the code is not licensed for reuse. Third-party sources fetched at build time (QuickJS, llama.cpp) are pinned by digest, not vendored, and carry their own upstream licenses.
