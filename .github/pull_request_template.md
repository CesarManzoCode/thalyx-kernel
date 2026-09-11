<!--
Thanks for contributing. See CONTRIBUTING.md for the workflow this project uses
(branch from main, coherent commits, validate, PR, squash merge).
-->

## What this changes and why

## Scope

- [ ] This does not add new kernel architecture, new features, or attempt the IPC lock bottleneck (K0–K6 are functionally frozen; open an issue first if it does)
- [ ] If this changes kernel behavior, the relevant contract/decision record/evidence note is updated in this PR
- [ ] If this fixes a runtime defect, a gate criterion or self-test damage case now covers it

## Validation performed

<!-- Exact commands run and their result. At minimum: -->
```
cargo fmt --check
cargo build --workspace
python3 tools/check_abi.py
python3 tools/check_vault.py
```
<!-- If applicable, which gate(s) you ran locally (tools/check_k*.py, tools/run_gates.py) and the result. -->

## Related issue(s)

Closes #
