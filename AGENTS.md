# Working on Thalyx-Kernel

Read `vault/constitution.md`, `vault/roadmap/current-state.md`, and the relevant architecture contract before changing this project.

- The user has authorized architectural engineering judgment. Resolve routine technical choices through evidence and update the appropriate decision record; do not manufacture approval gates.
- This repository is independent of Thalyx. Do not modify Thalyx or remove its Linux path as an incidental part of kernel work.
- Documentation prose is Spanish; code, identifiers, file names, and commit messages are English.
- Distinguish observed facts, accepted design, hypotheses, and implemented behavior. A model result or a successful emulator boot does not prove the whole architecture.
- Preserve the invariant-to-mechanism-to-validation mapping. If behavior changes, update its contract, decision record, current state, and evidence together.
- Develop on a work branch. Commit completed coherent changes. Do not open a pull request unless requested. Never rewrite shared history as routine cleanup.
- Use primary sources and immutable source revisions for implementation claims. Record limitations and negative results.
- Check local links and run relevant model checks with `python3 tools/check_vault.py` and `python3 research/models/check_models.py`. These validate documents and finite models, not kernel correctness.
- Follow `vault/governance.md` for decision changes. A conflicting contract must be resolved before dependent implementation proceeds.
- Do not add blanket permission requirements, unsupported benchmark claims, implementation status inferred from plans, or new scope disguised as a prerequisite.
