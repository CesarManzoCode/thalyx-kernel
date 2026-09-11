/* Time, from the one clock this system has.
 *
 * `CLOCK_QUERY` is monotonic nanoseconds since the kernel established a clock,
 * and the limits query carries the boot epoch the kernel was given. There is no
 * synchronised wall clock here: `time()` returns the epoch plus the monotonic
 * reading, and a caller that needs a real date has to be given one.
 *
 * Processor time is the kernel's accounting and not an estimate: `clock()` and
 * `CLOCK_PROCESS_CPUTIME_ID` read what the program's own scope has been
 * charged, which includes every thread of the domain and nothing else. There
 * is no per-thread query, so `CLOCK_THREAD_CPUTIME_ID` is refused rather than
 * answered with the scope's figure.
 */

#include "thalyx/nrt.h"
#include <time.h>
#include <sys/time.h>
#include <string.h>
#include <errno.h>

static uint64_t boot_epoch(void)
{
    static uint64_t cached;
    static int asked;
    if (!asked) {
        thalyx_limits_t limits;
        if (th_limits(&limits) == THALYX_STATUS_OK) { cached = limits.boot_epoch; }
        asked = 1;
    }
    return cached;
}

static int scope_cpu_ns(uint64_t *out)
{
    th_desc d;
    th_desc_begin(&d, THALYX_OP_SCOPE_QUERY);
    th_result r = th_op(thalyx_boot_handle_of(TH_SLOT_SELF_SCOPE), THALYX_OP_SCOPE_QUERY, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return -1; }
    thalyx_scope_info_t info;
    memcpy(&info, d.bytes + TH_BODY, sizeof(info));
    *out = info.cpu_total_ns;
    return 0;
}

time_t time(time_t *out)
{
    time_t seconds = (time_t)(boot_epoch() + th_now_ns() / 1000000000ull);
    if (out) { *out = seconds; }
    return seconds;
}

clock_t clock(void)
{
    uint64_t ns;
    if (scope_cpu_ns(&ns) != 0) { return (clock_t)-1; }
    return (clock_t)(ns / 1000ull);
}

int clock_gettime(clockid_t which, struct timespec *out)
{
    uint64_t ns;
    switch (which) {
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_MONOTONIC_COARSE:
    case CLOCK_BOOTTIME:
        ns = th_now_ns();
        break;
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_COARSE:
        ns = th_now_ns() + boot_epoch() * 1000000000ull;
        break;
    case CLOCK_PROCESS_CPUTIME_ID:
        if (scope_cpu_ns(&ns) != 0) { errno = EINVAL; return -1; }
        break;
    default:
        errno = EINVAL;
        return -1;
    }
    out->tv_sec = (time_t)(ns / 1000000000ull);
    out->tv_nsec = (long)(ns % 1000000000ull);
    return 0;
}

int clock_getres(clockid_t which, struct timespec *out)
{
    if (which == CLOCK_THREAD_CPUTIME_ID || which < 0 || which > CLOCK_BOOTTIME) {
        errno = EINVAL;
        return -1;
    }
    if (out) { out->tv_sec = 0; out->tv_nsec = 1; }
    return 0;
}

int timespec_get(struct timespec *out, int base)
{
    if (base != TIME_UTC || clock_gettime(CLOCK_REALTIME, out) != 0) { return 0; }
    return base;
}

int gettimeofday(struct timeval *out, void *timezone_unused)
{
    (void)timezone_unused;
    uint64_t ns = th_now_ns() + boot_epoch() * 1000000000ull;
    out->tv_sec = (time_t)(ns / 1000000000ull);
    out->tv_usec = (suseconds_t)((ns % 1000000000ull) / 1000);
    return 0;
}
