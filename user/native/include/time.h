/* Time, with the monotonic clock the kernel publishes underneath it.
 *
 * The wall clock is not a thing this system has: nothing here is synchronised
 * to anything, and pretending otherwise would put a fabricated date into
 * durable evidence. `time()` therefore returns the boot epoch the kernel
 * reports plus monotonic nanoseconds, and says so.
 */
#ifndef _TIME_H
#define _TIME_H
#include <stddef.h>
#include <stdint.h>

typedef long time_t;
typedef long suseconds_t;

struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
    long tm_gmtoff;
    const char *tm_zone;
};

struct timespec { time_t tv_sec; long tv_nsec; };

time_t time(time_t *out);
int    clock_gettime(int which, struct timespec *out);
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#endif
