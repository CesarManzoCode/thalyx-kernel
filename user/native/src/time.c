/* Time, from the one clock this system has.
 *
 * `CLOCK_QUERY` is monotonic nanoseconds since the kernel established a clock,
 * and the limits query carries the boot epoch the kernel was given. There is no
 * synchronised wall clock here: `time()` returns the epoch plus the monotonic
 * reading, and a caller that needs a real date has to be given one.
 */

#include "thalyx/nrt.h"
#include <time.h>
#include <sys/time.h>

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

time_t time(time_t *out)
{
    time_t seconds = (time_t)(boot_epoch() + th_now_ns() / 1000000000ull);
    if (out) { *out = seconds; }
    return seconds;
}

int clock_gettime(int which, struct timespec *out)
{
    uint64_t ns = th_now_ns();
    if (which == CLOCK_REALTIME) { ns += boot_epoch() * 1000000000ull; }
    out->tv_sec = (time_t)(ns / 1000000000ull);
    out->tv_nsec = (long)(ns % 1000000000ull);
    return 0;
}

int gettimeofday(struct timeval *out, void *timezone_unused)
{
    (void)timezone_unused;
    uint64_t ns = th_now_ns() + boot_epoch() * 1000000000ull;
    out->tv_sec = (time_t)(ns / 1000000000ull);
    out->tv_usec = (suseconds_t)((ns % 1000000000ull) / 1000);
    return 0;
}
