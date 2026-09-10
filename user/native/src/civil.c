/* Civil time, in UTC.
 *
 * The conversions are the well-known days-from-civil pair -- proleptic
 * Gregorian, valid across the whole range a 64-bit `time_t` can hold, with no
 * table and no leap seconds. There is no timezone here: nothing on this system
 * is configured with one and nothing is synchronised to anything, so the local
 * offset is zero and a caller is told zero rather than a plausible number.
 */

#include <time.h>
#include <unistd.h>

static long days_from_civil(long year, unsigned month, unsigned day)
{
    year -= month <= 2;
    const long era = (year >= 0 ? year : year - 399) / 400;
    const unsigned year_of_era = (unsigned)(year - era * 400);
    const unsigned day_of_year =
        (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1;
    const unsigned day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    return era * 146097 + (long)day_of_era - 719468;
}

static void civil_from_days(long days, long *year, unsigned *month, unsigned *day)
{
    days += 719468;
    const long era = (days >= 0 ? days : days - 146096) / 146097;
    const unsigned day_of_era = (unsigned)(days - era * 146097);
    const unsigned year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    const long y = (long)year_of_era + era * 400;
    const unsigned day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    const unsigned mp = (5 * day_of_year + 2) / 153;
    *day = day_of_year - (153 * mp + 2) / 5 + 1;
    *month = mp + (mp < 10 ? 3 : -9);
    *year = y + (*month <= 2);
}

struct tm *gmtime_r(const time_t *when, struct tm *into)
{
    time_t seconds = *when;
    long days = (long)(seconds / 86400);
    long rest = (long)(seconds % 86400);
    if (rest < 0) { rest += 86400; days -= 1; }

    long year;
    unsigned month, day;
    civil_from_days(days, &year, &month, &day);

    into->tm_sec = (int)(rest % 60);
    into->tm_min = (int)((rest / 60) % 60);
    into->tm_hour = (int)(rest / 3600);
    into->tm_mday = (int)day;
    into->tm_mon = (int)month - 1;
    into->tm_year = (int)(year - 1900);
    /* 1970-01-01 was a Thursday. */
    into->tm_wday = (int)(((days % 7) + 11) % 7);
    into->tm_yday = (int)(days - days_from_civil(year, 1, 1));
    into->tm_isdst = 0;
    into->tm_gmtoff = 0;
    into->tm_zone = "UTC";
    return into;
}

struct tm *localtime_r(const time_t *when, struct tm *into)
{
    return gmtime_r(when, into);
}

time_t timegm(struct tm *broken)
{
    long days = days_from_civil((long)broken->tm_year + 1900,
                                (unsigned)(broken->tm_mon + 1),
                                (unsigned)broken->tm_mday);
    return (time_t)days * 86400 + broken->tm_hour * 3600 + broken->tm_min * 60 + broken->tm_sec;
}

time_t mktime(struct tm *broken)
{
    time_t seconds = timegm(broken);
    gmtime_r(&seconds, broken);            /* normalised, as the standard says */
    return seconds;
}

double difftime(time_t later, time_t earlier)
{
    return (double)(later - earlier);
}

/* There are no links and no paths here. It always refuses, which is what the
 * one caller in the ported runtime is written to handle. */
ssize_t readlink(const char *path, char *into, size_t size)
{
    (void)path; (void)into; (void)size;
    return -1;
}
