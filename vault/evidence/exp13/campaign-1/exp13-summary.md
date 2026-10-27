# EXP-13 campaign results

Report: `/home/cesarmanzocode/thalyx-kernel/build/exp13/campaign-1` — arms present: L0, L1, K1 — paired rounds: 6, bootstrap resamples: 10000.

All figures are milliseconds of task end-to-end latency (`whole_ns`), median of paired per-round differences with a 95% bootstrap CI. A negative K1−L1 means the kernel arm was faster.

## Aggregate, all cases

| effect | median (ms) | 95% CI (ms) | paired rounds×cases |
|---|---|---|---|
| K1-L0 | -8.176 | [-12.223, -6.100] | 96 |
| L1-L0 | -36.859 | [-38.079, -35.235] | 96 |
| K1-L1 | +27.684 | [+24.581, +31.777] | 96 |

## Per case

| case | L0 | L1 | K1 | K1−L0 | L1−L0 | K1−L1 |
|---|---|---|---|---|---|---|
| a-confined-check-that-may-not-run | +39.705 | +3.940 | +27.080 | -13.978 | -35.716 | +23.103 |
| a-failing-check-puts-it-back | +33.371 | +3.783 | +26.450 | -7.194 | -29.404 | +22.585 |
| a-failure-kept-on-request | +26.153 | +4.076 | +46.344 | +17.226 | -22.020 | +42.146 |
| a-loop-runs-out-of-calls | +38.501 | +5.933 | +31.498 | -8.237 | -32.501 | +25.399 |
| a-program-cannot-settle-its-own-boundary | +33.236 | +6.111 | +44.038 | +7.700 | -27.120 | +38.009 |
| a-program-that-looks-before-it-changes | +48.291 | +9.985 | +60.140 | +11.849 | -38.463 | +49.581 |
| a-rust-check | +91.684 | +26.069 | +48.851 | -42.895 | -65.508 | +22.782 |
| a-step-outside-the-workspace-is-refused | +40.953 | +3.497 | +22.847 | -16.810 | -37.305 | +19.312 |
| a-verb-that-is-not-exposed-is-not-reachable | +33.445 | +3.021 | +23.619 | -9.247 | -30.439 | +20.612 |
| a-verdict-about-an-older-tree | +42.108 | +6.751 | +26.166 | -14.916 | -35.356 | +19.212 |
| an-assertion-stops-the-program | +44.067 | +5.292 | +30.539 | -12.035 | -38.685 | +25.163 |
| context-decides-a-rename | +45.714 | +8.696 | +44.445 | -1.248 | -36.978 | +35.926 |
| it-worked-and-was-put-back | +43.218 | +4.020 | +27.481 | -14.073 | -39.122 | +23.555 |
| steps-edit-and-check | +34.409 | +4.446 | +40.652 | +5.035 | -29.851 | +35.924 |
| the-program-asks-for-the-model | +156.489 | +18.102 | +40.338 | -115.515 | -138.141 | +21.960 |
| two-transactions-in-a-row | +74.920 | +9.003 | +68.726 | -6.153 | -65.855 | +59.715 |

## Transport calls (managed arms), aggregate paired difference

| effect | median calls | 95% CI |
|---|---|---|
| K1-L1 | None | [None, None] |

