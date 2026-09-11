# Contributing to Thalyx-Kernel

This is a low-level systems project (a bare-metal microkernel and its build/validation tooling), developed against a set of gates that decide correctness independently of the code under test. Read [`AGENTS.md`](AGENTS.md) and [`vault/constitution.md`](vault/constitution.md) before proposing a change to the kernel or its packages — most architectural questions already have a decision recorded in [`vault/decisions/`](vault/decisions/README.md).

## Workflow

1. **Branch from current `main`.** Short-lived, one branch per coherent piece of work: `git checkout -b fix/short-description main` after pulling the latest `main`.
2. **Commit in coherent, reviewable steps.** Each commit should be something that could stand on its own: a fix, a test, a doc update tied to the behavior it documents. Commit messages and all identifiers/code are English (see [`AGENTS.md`](AGENTS.md)); prose in `vault/` is Spanish.
3. **Validate before opening a PR.** At minimum:
   ```sh
   cargo fmt --check
   cargo build --workspace
   python3 tools/check_abi.py
   python3 tools/check_vault.py
   ```
   If your change touches kernel behavior, run the relevant gate(s) locally (`tools/check_k1.py` … `tools/check_k6.py`, and `tools/run_gates.py` for the full chain) — these need a QEMU/OVMF/mtools toolchain and are not run by hosted CI. See the README's [Validate](README.md#validate-reproducing-each-gates-verdict) section for exact commands and what CI covers instead.
4. **Open a pull request against `main`.** Fill in the PR template; link the issue it closes, if any.
5. **Squash merge.** One PR becomes one commit on `main`. Delete the branch after merge (this is not automatic in this repository's settings — do it from the PR page or `git push origin --delete <branch>`).

Direct pushes to `main` are not the normal path for any change, including small ones — a PR gives CI and review a chance to run even on a one-line fix.

## What counts as a good change here

- **A fix is minimal and regression-covered.** If a defect only appears at runtime (most of them do, in this codebase — see how many each phase's evidence note lists), add or extend a gate criterion or a self-test damage case that would have caught it, not just a comment.
- **Evidence and contracts move together.** If you change what a mechanism does, update its architecture contract, its decision record (if the change reverses a decision), and the phase evidence note that measured it — in the same PR. A behavior change without an updated contract is treated as a bug in the PR, not the code.
- **Distinguish claims.** Follow the vault's own discipline: an observed fact is a specific execution, cited; a decision binds implementation but is revisable; a contract is required behavior, not yet necessarily built; a hypothesis needs an experiment before it becomes anything else. Don't upgrade a hypothesis to a fact in prose because the code compiles.
- **No new scope disguised as a prerequisite.** K0–K6 are functionally frozen as of the repository-polish work recorded in the changelog: new kernel architecture, new features, and attempts at the IPC lock bottleneck are out of scope for a routine PR. Open an issue and get a decision recorded first (see below).

## Post-K6 bug workflow

K0–K6 are complete in their stated scope. A bug found in that scope (a gate that should have failed and didn't, a criterion that's wrong, a defect a self-test damage case doesn't cover) is fixed the same way as any other change: branch, fix, extend the gate to cover it, PR, squash merge. It does not reopen the phase or require redoing that phase's evidence note wholesale — add a note of the fix where the phase's evidence document already lists what execution corrected, if the document has that section.

A change that would alter what a phase's gate is allowed to conclude (loosening a criterion, removing a regression check, changing what "passing" means) needs a decision record update first, not just a code change — open an issue describing the proposed change and why, and reference [`vault/governance.md`](vault/governance.md).

## Reporting issues

Use the issue templates for a bug or for a feature/performance proposal. For anything security-relevant, see [`SECURITY.md`](SECURITY.md) instead of a public issue.
