/* The paired benchmarks of K6: the loop, the counter and the reporting, one
 * text compiled for both backends. See bench.h.
 *
 * Nothing here knows which backend it is on. What it knows is the plan the
 * host wrote -- which entries, in which order, how many samples of each -- and
 * the counter. A sample is the counter's difference around one operation, or
 * a count over a fixed slice of the counter; the conversion to time is the
 * host's, from the calibration this file reports, so that a backend whose
 * clock disagreed with the counter would show the disagreement rather than
 * fold it into its results.
 */

#include "bench.h"

static uint32_t samples[K6_SAMPLES_MAX];
static uint64_t tsc_hz;

uint64_t bench_tsc_hz(void) { return tsc_hz; }

uint64_t bench_calibrate(void)
{
    uint64_t t0 = plat_now_ns();
    uint64_t c0 = bench_cycles();
    uint64_t t1 = t0;
    while (t1 - t0 < K6_CAL_WINDOW_NS) { t1 = plat_now_ns(); }
    uint64_t c1 = bench_cycles();
    uint64_t elapsed = t1 - t0;
    tsc_hz = (uint64_t)(((unsigned __int128)(c1 - c0) * 1000000000u) / elapsed);
    return tsc_hz;
}

uint64_t bench_fnv1a(const void *bytes, uint64_t length)
{
    const uint8_t *p = bytes;
    uint64_t hash = 0xcbf29ce484222325ull;
    for (uint64_t i = 0; i < length; i++) {
        hash = (hash ^ p[i]) * 0x100000001b3ull;
    }
    return hash;
}

static uint32_t clamp32(uint64_t value)
{
    return value >= K6_LOST_SAMPLE ? (uint32_t)(K6_LOST_SAMPLE - 1) : (uint32_t)value;
}

void bench_emit_samples(const uint32_t *values, uint32_t count)
{
    for (uint32_t i = 0; i < count; i += 2) {
        uint64_t low = values[i];
        uint64_t high = i + 1 < count ? values[i + 1] : K6_LOST_SAMPLE;
        plat_emit(K6_NOTE_SAMPLE, low | (high << 32));
    }
}

static int is_latency(uint32_t bench)
{
    switch (bench) {
    case K6_BENCH_ENTRY_VERSION:
    case K6_BENCH_ENTRY_CLOCK:
    case K6_BENCH_IPC_CALL:
    case K6_BENCH_IPC_CAPS:
    case K6_BENCH_MEM_MAP:
    case K6_BENCH_MEM_SEAL:
    case K6_BENCH_CAP_DERIVE:
    case K6_BENCH_IPC_LINEAGE:
        return 1;
    default:
        return 0;
    }
}

static void error_note(uint32_t bench, int64_t status)
{
    plat_emit(K6_NOTE_ERROR, bench | ((uint64_t)(uint32_t)status << 32));
}

/* One latency entry: unrecorded warm-up, then one sample per operation. A
 * failed operation is a lost sample and an error, never a fast one. */
static uint32_t run_latency(uint32_t bench, uint32_t param, uint32_t n, uint32_t warmup,
                            uint32_t *errors)
{
    for (uint32_t i = 0; i < warmup; i++) { (void)plat_op(bench, param); }
    for (uint32_t i = 0; i < n; i++) {
        uint64_t c0 = bench_cycles();
        int64_t status = plat_op(bench, param);
        uint64_t c1 = bench_cycles();
        if (status < 0) {
            if (*errors == 0) { error_note(bench, status); }
            *errors += 1;
            samples[i] = (uint32_t)K6_LOST_SAMPLE;
        } else {
            samples[i] = clamp32(c1 - c0);
        }
    }
    return n;
}

void bench_run_plan(const k6_plan *plan)
{
    if (plan->magic != K6_PLAN_MAGIC || plan->version != K6_PLAN_VERSION
        || plan->count > K6_PLAN_ENTRIES) {
        error_note(0, -2);
        plat_emit(K6_NOTE_DONE, 0);
        return;
    }
    plat_emit(K6_NOTE_PLAN, plan->seed);
    plat_emit(K6_NOTE_TSC_HZ, bench_calibrate());

    for (uint32_t e = 0; e < plan->count; e++) {
        uint32_t bench = plan->bench[e];
        uint32_t param = plan->param[e];
        uint32_t n = plan->samples[e] < K6_SAMPLES_MAX ? plan->samples[e] : (uint32_t)K6_SAMPLES_MAX;
        uint32_t errors = 0;
        uint32_t got = 0;
        plat_emit(K6_NOTE_BEGIN, bench | ((uint64_t)param << 16) | ((uint64_t)n << 32));
        int64_t prepared = plat_prepare(bench, param);
        if (prepared < 0) {
            error_note(bench, prepared);
            errors = 1;
        } else if (is_latency(bench)) {
            got = run_latency(bench, param, n, plan->warmup[e], &errors);
        } else {
            int64_t special = plat_special(bench, param, samples, n);
            if (special < 0) {
                error_note(bench, special);
                errors = 1;
            } else {
                got = (uint32_t)special;
            }
        }
        bench_emit_samples(samples, got);
        if (prepared >= 0) { plat_release(bench, param); }
        plat_emit(K6_NOTE_END, bench | ((uint64_t)errors << 32));
    }
    plat_emit(K6_NOTE_DONE, plan->count);
}

/* ------------------------------------------------------------ sched.wake */

static volatile uint32_t wake_armed;
static volatile uint32_t wake_stop;
static volatile uint64_t wake_seen;

static void wakee(void *argument)
{
    (void)argument;
    for (;;) {
        __atomic_store_n(&wake_armed, 1u, __ATOMIC_RELEASE);
        plat_block();
        uint64_t now = bench_cycles();
        if (__atomic_load_n(&wake_stop, __ATOMIC_ACQUIRE)) { return; }
        __atomic_store_n(&wake_seen, now, __ATOMIC_RELEASE);
    }
}

static int wait_armed(uint64_t limit)
{
    uint64_t start = bench_cycles();
    while (!__atomic_load_n(&wake_armed, __ATOMIC_ACQUIRE)) {
        if (bench_cycles() - start > limit) { return 0; }
    }
    return 1;
}

/* The waker keeps running after the wake, so the woken thread has to run
 * somewhere else: what is measured is how long a blocked thread waits to run
 * once another running thread has made it runnable. Before each wake the waker
 * gives the wakee 50 microseconds to be inside its wait; a wake that still
 * arrives first is kept by both backends and shows as a very short sample. */
int64_t bench_wake(uint32_t *out, uint32_t n)
{
    if (plat_helper_threads() < 1) { return -20; }
    __atomic_store_n(&wake_armed, 0u, __ATOMIC_RELEASE);
    __atomic_store_n(&wake_stop, 0u, __ATOMIC_RELEASE);
    __atomic_store_n(&wake_seen, 0ull, __ATOMIC_RELEASE);
    if (plat_thread_start(1, wakee, 0) != 0) { return -18; }
    uint64_t settle = tsc_hz / 20000;
    uint64_t give_up = tsc_hz / 10;
    for (uint32_t i = 0; i < n; i++) {
        if (!wait_armed(give_up * 10)) {
            out[i] = (uint32_t)K6_LOST_SAMPLE;
            continue;
        }
        __atomic_store_n(&wake_armed, 0u, __ATOMIC_RELEASE);
        uint64_t s = bench_cycles();
        while (bench_cycles() - s < settle) { }
        __atomic_store_n(&wake_seen, 0ull, __ATOMIC_RELEASE);
        uint64_t c0 = bench_cycles();
        plat_wake();
        uint64_t seen = 0;
        while ((seen = __atomic_load_n(&wake_seen, __ATOMIC_ACQUIRE)) == 0) {
            if (bench_cycles() - c0 > give_up) { break; }
        }
        out[i] = seen ? clamp32(seen - c0) : (uint32_t)K6_LOST_SAMPLE;
    }
    __atomic_store_n(&wake_stop, 1u, __ATOMIC_RELEASE);
    (void)wait_armed(give_up * 10);
    plat_wake();
    plat_thread_join(1);
    return (int64_t)n;
}

/* ------------------------------------------------- scale.compute, .ipc */

static volatile uint32_t scale_go;
static uint64_t scale_start;
static uint64_t scale_slice;
static uint32_t scale_counts[4][BENCH_SCALE_SLICES];
static uint32_t scale_refused[4];
static volatile int64_t scale_first_error;
static volatile uint64_t scale_sink[4];

/* Consecutive failures after which a worker concludes its peer is gone rather
 * than busy. A refused admission answers in a microsecond, so this is well
 * under a slice of them. */
#define SCALE_GIVE_UP 100000u

typedef int64_t (*scale_work)(unsigned index, uint64_t *state);

static int64_t compute_chunk(unsigned index, uint64_t *state)
{
    (void)index;
    uint64_t x = *state;
    for (int k = 0; k < 256; k++) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    *state = x;
    return 0;
}

static int64_t ipc_chunk(unsigned index, uint64_t *state)
{
    (void)state;
    return plat_ipc_pair_call(index);
}

static scale_work scale_body;

/* Counts completed chunks of work in each slice of the counter. A thread that
 * was not running in a slice counts nothing in it, which is the point. A chunk
 * the backend refused -- an admission the audited profile would not take, a
 * socket that would block -- is counted as refused and tried again: the slice
 * it cost is charged to the throughput, and the refusals are reported beside
 * it rather than ending the measurement. */
static void scale_worker(void *argument)
{
    unsigned index = (unsigned)(uintptr_t)argument;
    uint64_t state = 0x9E3779B97F4A7C15ull ^ ((uint64_t)index << 32);
    uint32_t consecutive = 0;
    while (!__atomic_load_n(&scale_go, __ATOMIC_ACQUIRE)) { }
    for (;;) {
        int64_t status = scale_body(index, &state);
        uint64_t slice = (bench_cycles() - scale_start) / scale_slice;
        if (slice >= BENCH_SCALE_SLICES) { break; }
        if (status < 0) {
            scale_refused[index]++;
            if (__atomic_load_n(&scale_first_error, __ATOMIC_RELAXED) == 0) {
                __atomic_store_n(&scale_first_error, status, __ATOMIC_RELAXED);
            }
            if (++consecutive > SCALE_GIVE_UP) { break; }
            continue;
        }
        consecutive = 0;
        scale_counts[index][slice]++;
    }
    scale_sink[index] = state;
}

static int64_t scale_run(uint32_t bench, uint32_t threads, scale_work body, uint32_t *out,
                         uint32_t max)
{
    if (threads == 0 || threads > 4 || threads > plat_helper_threads() + 1) { return -20; }
    if (max < BENCH_SCALE_SLICES) { return -2; }
    for (unsigned t = 0; t < 4; t++) {
        for (unsigned s = 0; s < BENCH_SCALE_SLICES; s++) { scale_counts[t][s] = 0; }
        scale_refused[t] = 0;
    }
    scale_first_error = 0;
    scale_body = body;
    scale_slice = tsc_hz / (1000000000ull / K6_SLICE_NS);
    __atomic_store_n(&scale_go, 0u, __ATOMIC_RELEASE);
    for (unsigned t = 1; t < threads; t++) {
        if (plat_thread_start(t, scale_worker, (void *)(uintptr_t)t) != 0) { return -18; }
    }
    /* A short head start so every helper is spinning on `scale_go` when the
     * first slice begins. */
    uint64_t s = bench_cycles();
    while (bench_cycles() - s < tsc_hz / 100) { }
    scale_start = bench_cycles();
    __atomic_store_n(&scale_go, 1u, __ATOMIC_RELEASE);
    scale_worker((void *)(uintptr_t)0);
    for (unsigned t = 1; t < threads; t++) { plat_thread_join(t); }
    uint64_t refused = 0;
    for (unsigned s2 = 0; s2 < BENCH_SCALE_SLICES; s2++) {
        uint64_t total = 0;
        for (unsigned t = 0; t < threads; t++) { total += scale_counts[t][s2]; }
        out[s2] = clamp32(total);
    }
    for (unsigned t = 0; t < threads; t++) { refused += scale_refused[t]; }
    if (refused) {
        /* Quantity 1: chunks the backend refused over the whole run, and the
         * first status it refused with. */
        plat_emit(K6_NOTE_AUX, bench | (1ull << 16) | (refused << 32));
        error_note(bench, scale_first_error);
    }
    return BENCH_SCALE_SLICES;
}

int64_t bench_scale_compute(uint32_t threads, uint32_t *out, uint32_t max)
{
    return scale_run(K6_BENCH_SCALE_COMPUTE, threads, compute_chunk, out, max);
}

int64_t bench_scale_ipc(uint32_t pairs, uint32_t *out, uint32_t max)
{
    return scale_run(K6_BENCH_SCALE_IPC, pairs, ipc_chunk, out, max);
}

/* ---------------------------------------------------------- quota.share */

static uint64_t share_cycles[1024];

/* A thread that never blocks, reading the counter as fast as it can. Two
 * readings closer together than five microseconds mean it ran in between;
 * farther apart, it was not running. The sample is the execution one slice of
 * SLICE_NS received, in nanoseconds. */
int64_t bench_spin_share(uint64_t duration_ns, uint32_t *out, uint32_t max)
{
    uint64_t slices = duration_ns / K6_SLICE_NS;
    if (slices > max) { slices = max; }
    if (slices > 1024) { slices = 1024; }
    uint64_t slice = tsc_hz / (1000000000ull / K6_SLICE_NS);
    uint64_t gap = tsc_hz / 200000;
    for (uint64_t i = 0; i < slices; i++) { share_cycles[i] = 0; }
    uint64_t start = bench_cycles();
    uint64_t last = start;
    for (;;) {
        uint64_t now = bench_cycles();
        uint64_t index = (last - start) / slice;
        if (index >= slices) { break; }
        uint64_t step = now - last;
        if (step < gap) { share_cycles[index] += step; }
        last = now;
    }
    for (uint64_t i = 0; i < slices; i++) {
        out[i] = clamp32((uint64_t)(((unsigned __int128)share_cycles[i] * 1000000000u) / tsc_hz));
    }
    return (int64_t)slices;
}
