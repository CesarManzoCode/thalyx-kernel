# Security Policy

Thalyx-Kernel is a research/experimental microkernel. It has **not** been hardened for, or evaluated against, adversarial input on physical hardware, and several properties a reader might assume a kernel provides are explicitly not demonstrated yet — see the README's [What is not demonstrated](README.md#what-is-not-demonstrated) section before treating anything here as a security boundary in production. In particular: no DMA isolation (no IOMMU is programmed), no speculative-execution mitigations, and no verified/measured boot. Do not deploy this kernel as an isolation boundary for untrusted workloads outside a research setting.

Within its stated scope, the project does take capability confinement, authority revocation, and memory-object sealing seriously as invariants (see [`vault/architecture/authority.md`](vault/architecture/authority.md) and [`vault/validation/invariants.md`](vault/validation/invariants.md)) and treats a violation of one of those — a capability granting access it shouldn't, a sealed page becoming writable, authority surviving a scope fence that should have revoked it — as a real defect.

## Reporting a vulnerability

If you find a way to violate one of this kernel's stated invariants (not a limitation it already documents), please report it privately rather than opening a public issue: use GitHub's [private vulnerability reporting](https://github.com/CesarManzoCode/thalyx-kernel/security/advisories/new) for this repository. Include:

- the invariant or contract you believe is violated, with a link to it in `vault/architecture/` or `vault/validation/invariants.md`;
- a minimal reproduction (a program, a gate run, or a patch to an existing test package);
- which platform you ran it on (TCG, KVM, or — if you have it — physical hardware, which this project has not tested on).

There is no bug bounty. Reports are read and triaged on a best-effort basis; this is not a funded security team.

## What is out of scope

Reports that restate a limitation already documented in the README or in [`vault/roadmap/open-questions.md`](vault/roadmap/open-questions.md) — missing DMA isolation, no speculative-execution mitigations, no physical-hardware boot evidence, no power-loss durability — are not new findings. If you have a way to *close* one of those gaps, that's a contribution (see [`CONTRIBUTING.md`](CONTRIBUTING.md)), not a security report.
