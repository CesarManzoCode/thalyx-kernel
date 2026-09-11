/* The paired benchmarks of K6: what both backends share.
 *
 * One source, two backends. What differs between this kernel and Linux is the
 * primitive under measurement and nothing else: the loop that times it, the
 * counter it reads, the order entries run in, how samples are packed and
 * reported, the compute kernel of the scaling benchmark and the spin loop of
 * the quota benchmark are all in bench.c and compiled from the same text for
 * both. A backend supplies the `plat_` functions below and nothing more.
 *
 * The plan and the notes are `thalyx/k6.h`, generated from
 * abi/schema/k6-bench-v1.json, which is also where each benchmark says what
 * each side does and what a comparison of the two is allowed to conclude.
 */
#ifndef THALYX_K6_BENCH_H
#define THALYX_K6_BENCH_H

#include <stdint.h>

#include "thalyx/k6.h"

/* ------------------------------------------------ what a backend provides */

/* Monotonic nanoseconds from this backend's own clock. */
uint64_t plat_now_ns(void);
/* One note to the host: a code from `k6.h` and a value. */
void plat_emit(uint64_t code, uint64_t value);
/* Readies an entry. Zero when this backend runs it, a negative status when it
 * cannot; nothing is measured then and the entry reports the status. */
int64_t plat_prepare(uint32_t bench, uint32_t param);
/* One measured operation of a latency entry: zero, or a negative status. */
int64_t plat_op(uint32_t bench, uint32_t param);
/* Undoes what `plat_prepare` set up. */
void plat_release(uint32_t bench, uint32_t param);
/* Entries whose samples are not one timed operation each. Writes at most `max`
 * samples and answers how many, or a negative status. */
int64_t plat_special(uint32_t bench, uint32_t param, uint32_t *out, uint32_t max);

/* Helper threads, numbered from 1, besides the one running the plan. */
typedef void (*bench_fn)(void *argument);
unsigned plat_helper_threads(void);
int plat_thread_start(unsigned index, bench_fn body, void *argument);
int plat_thread_join(unsigned index);

/* A blocking wait and the wake that ends it, for `sched.wake`. A wake that
 * arrives before the wait is kept, on both backends: the wait then returns at
 * once. */
void plat_block(void);
void plat_wake(void);

/* One round trip on IPC pair `pair`, for `scale.ipc`. */
int64_t plat_ipc_pair_call(unsigned pair);

/* ------------------------------------------------------------- shared */

/* The time-stamp counter, ordered after every earlier load. Both backends read
 * the same counter of the same virtual machine. */
static inline uint64_t bench_cycles(void)
{
    uint32_t lo, hi;
    __asm__ __volatile__("lfence\n\trdtsc" : "=a"(lo), "=d"(hi) : : "memory");
    return ((uint64_t)hi << 32) | lo;
}

/* Calibrates the counter against `plat_now_ns` over CAL_WINDOW_NS and keeps
 * the answer. */
uint64_t bench_calibrate(void);
uint64_t bench_tsc_hz(void);

/* Runs a plan from its first entry to its last and reports every sample. */
void bench_run_plan(const k6_plan *plan);

/* The shared drivers of the entries that are not one operation per sample. */
int64_t bench_wake(uint32_t *out, uint32_t n);
int64_t bench_scale_compute(uint32_t threads, uint32_t *out, uint32_t max);
int64_t bench_scale_ipc(uint32_t pairs, uint32_t *out, uint32_t max);
int64_t bench_spin_share(uint64_t duration_ns, uint32_t *out, uint32_t max);

/* Sends samples to the host, two to a note. */
void bench_emit_samples(const uint32_t *samples, uint32_t count);

/* FNV-1a, over the bytes of an answer: the digest the host recomputes. */
uint64_t bench_fnv1a(const void *bytes, uint64_t length);

/* Slices the scaling benchmarks count in, SLICE_NS each. */
#define BENCH_SCALE_SLICES 50u

#endif /* THALYX_K6_BENCH_H */
