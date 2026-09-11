/* Civil time as text, in the C locale and in UTC.
 *
 * `strftime` is what the C++ library's time facets and ported programs' log
 * lines reach. It covers the conversions the C locale defines; one it does not
 * know is copied through as written rather than guessed at.
 */

#include <time.h>
#include <stdio.h>
#include <string.h>

static const char *const day_names[7] = {
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
};
static const char *const month_names[12] = {
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
};

struct tm *gmtime(const time_t *when)
{
    static struct tm shared;
    return gmtime_r(when, &shared);
}

struct tm *localtime(const time_t *when)
{
    static struct tm shared;
    return localtime_r(when, &shared);
}

char *asctime(const struct tm *t)
{
    static char text[32];
    snprintf(text, sizeof(text), "%.3s %.3s%3d %.2d:%.2d:%.2d %d\n",
             day_names[(unsigned)t->tm_wday % 7], month_names[(unsigned)t->tm_mon % 12],
             t->tm_mday, t->tm_hour, t->tm_min, t->tm_sec, 1900 + t->tm_year);
    return text;
}

char *ctime(const time_t *when) { return asctime(localtime(when)); }

size_t strftime(char *out, size_t n, const char *format, const struct tm *t)
{
    size_t at = 0;
    char piece[64];
    for (const char *f = format; *f; f++) {
        const char *text = piece;
        piece[0] = 0;
        if (*f != '%') {
            piece[0] = *f;
            piece[1] = 0;
        } else {
            f++;
            switch (*f) {
            case 'a': snprintf(piece, sizeof(piece), "%.3s", day_names[(unsigned)t->tm_wday % 7]); break;
            case 'A': text = day_names[(unsigned)t->tm_wday % 7]; break;
            case 'b': case 'h':
                snprintf(piece, sizeof(piece), "%.3s", month_names[(unsigned)t->tm_mon % 12]);
                break;
            case 'B': text = month_names[(unsigned)t->tm_mon % 12]; break;
            case 'c':
                snprintf(piece, sizeof(piece), "%.3s %.3s %2d %.2d:%.2d:%.2d %d",
                         day_names[(unsigned)t->tm_wday % 7], month_names[(unsigned)t->tm_mon % 12],
                         t->tm_mday, t->tm_hour, t->tm_min, t->tm_sec, 1900 + t->tm_year);
                break;
            case 'C': snprintf(piece, sizeof(piece), "%02d", (1900 + t->tm_year) / 100); break;
            case 'd': snprintf(piece, sizeof(piece), "%02d", t->tm_mday); break;
            case 'D':
                snprintf(piece, sizeof(piece), "%02d/%02d/%02d", t->tm_mon + 1, t->tm_mday,
                         (1900 + t->tm_year) % 100);
                break;
            case 'e': snprintf(piece, sizeof(piece), "%2d", t->tm_mday); break;
            case 'F':
                snprintf(piece, sizeof(piece), "%d-%02d-%02d", 1900 + t->tm_year, t->tm_mon + 1,
                         t->tm_mday);
                break;
            case 'H': snprintf(piece, sizeof(piece), "%02d", t->tm_hour); break;
            case 'I': snprintf(piece, sizeof(piece), "%02d", t->tm_hour % 12 ? t->tm_hour % 12 : 12); break;
            case 'j': snprintf(piece, sizeof(piece), "%03d", t->tm_yday + 1); break;
            case 'm': snprintf(piece, sizeof(piece), "%02d", t->tm_mon + 1); break;
            case 'M': snprintf(piece, sizeof(piece), "%02d", t->tm_min); break;
            case 'n': text = "\n"; break;
            case 'p': text = t->tm_hour < 12 ? "AM" : "PM"; break;
            case 'R': snprintf(piece, sizeof(piece), "%02d:%02d", t->tm_hour, t->tm_min); break;
            case 'S': snprintf(piece, sizeof(piece), "%02d", t->tm_sec); break;
            case 't': text = "\t"; break;
            case 'T':
                snprintf(piece, sizeof(piece), "%02d:%02d:%02d", t->tm_hour, t->tm_min, t->tm_sec);
                break;
            case 'u': snprintf(piece, sizeof(piece), "%d", t->tm_wday ? t->tm_wday : 7); break;
            case 'w': snprintf(piece, sizeof(piece), "%d", t->tm_wday); break;
            case 'x':
                snprintf(piece, sizeof(piece), "%02d/%02d/%02d", t->tm_mon + 1, t->tm_mday,
                         (1900 + t->tm_year) % 100);
                break;
            case 'X':
                snprintf(piece, sizeof(piece), "%02d:%02d:%02d", t->tm_hour, t->tm_min, t->tm_sec);
                break;
            case 'y': snprintf(piece, sizeof(piece), "%02d", (1900 + t->tm_year) % 100); break;
            case 'Y': snprintf(piece, sizeof(piece), "%d", 1900 + t->tm_year); break;
            case 'z': text = "+0000"; break;
            case 'Z': text = "UTC"; break;
            case '%': text = "%"; break;
            case 0: f--; text = "%"; break;
            default: snprintf(piece, sizeof(piece), "%%%c", *f); break;
            }
        }
        size_t len = strlen(text);
        if (at + len >= n) { return 0; }
        memcpy(out + at, text, len);
        at += len;
    }
    if (at >= n) { return 0; }
    out[at] = 0;
    return at;
}
