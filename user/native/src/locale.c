/* The one locale this system has, behind glibc's locale interface.
 *
 * `std::locale::classic()` builds every facet the C++ library has, and each is
 * written against glibc's interface: `__newlocale`, `__uselocale`,
 * `__nl_langinfo_l` and the `_l` family. So that interface is here, and it
 * answers for "C". A request for any other locale by name fails with `ENOENT`,
 * which is what a system without that locale installed answers too.
 *
 * Collation is byte order and conversion is one byte to one character, which
 * is what "C" means.
 */

#include "thalyx/nrt.h"
#include <locale.h>
#include <libintl.h>
#include <string.h>
#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <time.h>

struct __locale_struct {
    const char *name;
};

static struct __locale_struct c_locale = { "C" };
static locale_t current[TH_THREAD_MAX + 1];

static struct lconv c_lconv = {
    (char *)".", (char *)"", (char *)"", (char *)"", (char *)"",
    (char *)"", (char *)"", (char *)"", (char *)"", (char *)"",
    CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX,
    CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX, CHAR_MAX,
};

/* "C.UTF-8" is not among them: it is a different locale, with multibyte
 * characters, and this system does not have it. */
static int is_c(const char *name)
{
    return name[0] == 0 || strcmp(name, "C") == 0 || strcmp(name, "POSIX") == 0;
}

char *setlocale(int category, const char *name)
{
    if (category < LC_CTYPE || category > LC_IDENTIFICATION) {
        errno = EINVAL;
        return NULL;
    }
    if (name == NULL || is_c(name)) { return (char *)"C"; }
    errno = ENOENT;
    return NULL;
}

struct lconv *localeconv(void) { return &c_lconv; }

locale_t newlocale(int mask, const char *name, locale_t base)
{
    (void)mask;
    (void)base;
    if (name == NULL) {
        errno = EINVAL;
        return (locale_t)0;
    }
    if (!is_c(name)) {
        errno = ENOENT;
        return (locale_t)0;
    }
    return &c_locale;
}

locale_t duplocale(locale_t locale)
{
    return locale == LC_GLOBAL_LOCALE ? &c_locale : locale;
}

void freelocale(locale_t locale) { (void)locale; }

locale_t uselocale(locale_t locale)
{
    unsigned self = th_thread_index();
    locale_t previous = current[self] ? current[self] : LC_GLOBAL_LOCALE;
    if (locale != (locale_t)0) {
        current[self] = locale == LC_GLOBAL_LOCALE ? (locale_t)0 : locale;
    }
    return previous;
}

locale_t __newlocale(int mask, const char *name, locale_t base)
    __attribute__((alias("newlocale")));
locale_t __duplocale(locale_t locale) __attribute__((alias("duplocale")));
void __freelocale(locale_t locale) __attribute__((alias("freelocale")));
locale_t __uselocale(locale_t locale) __attribute__((alias("uselocale")));

size_t __ctype_get_mb_cur_max(void) { return 1; }

/* ------------------------------------------------------------ langinfo */

/* glibc encodes an item as its category above sixteen bits and an index
 * below, and code compiled against glibc passes those numbers. */
#define ITEM(category, index) (((category) << 16) | (index))

static const char *const day_names[7] = {
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
};
static const char *const day_short[7] = { "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat" };
static const char *const month_names[12] = {
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
};
static const char *const month_short[12] = {
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
};

char *__nl_langinfo_l(int item, locale_t locale)
{
    (void)locale;
    int category = item >> 16;
    int index = item & 0xFFFF;
    if (category == LC_CTYPE && index == 14) { return (char *)"ANSI_X3.4-1968"; }
    if (category == LC_NUMERIC) {
        if (index == 0) { return (char *)"."; }        /* RADIXCHAR / DECIMAL_POINT */
        return (char *)"";                              /* THOUSEP, GROUPING */
    }
    if (category == LC_TIME) {
        if (index <= 6) { return (char *)day_short[index]; }
        if (index <= 13) { return (char *)day_names[index - 7]; }
        if (index <= 25) { return (char *)month_short[index - 14]; }
        if (index <= 37) { return (char *)month_names[index - 26]; }
        switch (index) {
        case 38: return (char *)"AM";
        case 39: return (char *)"PM";
        case 40: return (char *)"%a %b %e %H:%M:%S %Y";
        case 41: return (char *)"%m/%d/%y";
        case 42: return (char *)"%H:%M:%S";
        case 43: return (char *)"%I:%M:%S %p";
        default: return (char *)"";
        }
    }
    if (category == LC_MONETARY) {
        /* Everything the C locale leaves undefined is empty, and every
         * character-valued item is CHAR_MAX, which glibc returns as a string
         * holding that one byte. */
        static const char unset[2] = { CHAR_MAX, 0 };
        if (index >= 7 && index <= 14) { return (char *)unset; }
        return (char *)"";
    }
    return (char *)"";
}

char *nl_langinfo(int item) { return __nl_langinfo_l(item, &c_locale); }

/* ------------------------------------------------------------ collation */

int strcoll(const char *a, const char *b) { return strcmp(a, b); }

size_t strxfrm(char *dst, const char *src, size_t n)
{
    size_t length = strlen(src);
    if (n > 0) {
        size_t copy = length < n - 1 ? length : n - 1;
        memcpy(dst, src, copy);
        dst[copy] = 0;
    }
    return length;
}

int __strcoll_l(const char *a, const char *b, locale_t locale)
{
    (void)locale;
    return strcoll(a, b);
}

size_t __strxfrm_l(char *dst, const char *src, size_t n, locale_t locale)
{
    (void)locale;
    return strxfrm(dst, src, n);
}

size_t __strftime_l(char *out, size_t n, const char *format, const struct tm *when,
                    locale_t locale)
{
    (void)locale;
    return strftime(out, n, format, when);
}

double __strtod_l(const char *s, char **end, locale_t locale)
{
    (void)locale;
    return strtod(s, end);
}

float __strtof_l(const char *s, char **end, locale_t locale)
{
    (void)locale;
    return strtof(s, end);
}

long double __strtold_l(const char *s, char **end, locale_t locale)
{
    (void)locale;
    return strtold(s, end);
}

double strtod_l(const char *s, char **end, locale_t locale) __attribute__((alias("__strtod_l")));
float strtof_l(const char *s, char **end, locale_t locale) __attribute__((alias("__strtof_l")));
long double strtold_l(const char *s, char **end, locale_t locale)
    __attribute__((alias("__strtold_l")));

/* ------------------------------------------------------------ messages */

char *gettext(const char *msgid) { return (char *)msgid; }
char *dgettext(const char *domain, const char *msgid) { (void)domain; return (char *)msgid; }
char *dcgettext(const char *domain, const char *msgid, int category)
{
    (void)domain;
    (void)category;
    return (char *)msgid;
}
char *textdomain(const char *domain) { return (char *)(domain ? domain : "messages"); }
char *bindtextdomain(const char *domain, const char *dir) { (void)domain; return (char *)dir; }
char *bind_textdomain_codeset(const char *domain, const char *codeset)
{
    (void)domain;
    return (char *)codeset;
}
