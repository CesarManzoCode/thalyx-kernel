/* Time, with the monotonic clock the kernel publishes underneath it.
 *
 * The wall clock is not a thing this system has: nothing here is synchronised
 * to anything, and pretending otherwise would put a fabricated date into
 * durable evidence. `time()` therefore returns the boot epoch the kernel
 * reports plus monotonic nanoseconds, and says so.
 *
 * Clock identifiers have glibc's values, because code compiled against glibc
 * -- the prebuilt C++ library's `steady_clock` among it -- passes those.
 */
#ifndef _TIME_H
#define _TIME_H

#include <stddef.h>
#include <stdint.h>
#include "thalyx/cdefs.h"

typedef long time_t;
typedef long suseconds_t;
typedef long clock_t;
typedef int clockid_t;

struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
    long tm_gmtoff;
    const char *tm_zone;
};

struct timespec { time_t tv_sec; long tv_nsec; };

#define CLOCKS_PER_SEC ((clock_t) 1000000)
#define TIME_UTC 1

#define CLOCK_REALTIME           0
#define CLOCK_MONOTONIC          1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID  3
#define CLOCK_MONOTONIC_RAW      4
#define CLOCK_REALTIME_COARSE    5
#define CLOCK_MONOTONIC_COARSE   6
#define CLOCK_BOOTTIME           7

__TH_BEGIN_DECLS

time_t  time(time_t *out) __TH_NOTHROW;
clock_t clock(void) __TH_NOTHROW;
double  difftime(time_t later, time_t earlier) __TH_NOTHROW;

/* Civil time, in UTC and only UTC. There is no timezone database on this
 * system and no configured offset, so `localtime_r` is `gmtime_r`: a runtime
 * asking what the local offset is gets zero, which is true here, rather than a
 * number somebody guessed. */
struct tm *gmtime_r(const time_t *when, struct tm *into) __TH_NOTHROW;
struct tm *localtime_r(const time_t *when, struct tm *into) __TH_NOTHROW;
struct tm *gmtime(const time_t *when) __TH_NOTHROW;
struct tm *localtime(const time_t *when) __TH_NOTHROW;
time_t     mktime(struct tm *broken) __TH_NOTHROW;
time_t     timegm(struct tm *broken) __TH_NOTHROW;
char      *asctime(const struct tm *broken) __TH_NOTHROW;
char      *ctime(const time_t *when) __TH_NOTHROW;
size_t     strftime(char *out, size_t n, const char *format, const struct tm *broken) __TH_NOTHROW;

int clock_gettime(clockid_t which, struct timespec *out) __TH_NOTHROW;
int clock_getres(clockid_t which, struct timespec *out) __TH_NOTHROW;
int nanosleep(const struct timespec *wanted, struct timespec *left);
int timespec_get(struct timespec *out, int base) __TH_NOTHROW;

__TH_END_DECLS

#endif
