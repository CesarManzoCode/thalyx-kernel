/* Wide characters in the C locale.
 *
 * One byte is one character. A byte below 0x80 converts to the wide character
 * of the same value and back; anything else is not a character in this locale
 * and conversion stops with `EILSEQ`, as glibc's does in "C". The `_l` forms
 * are what the prebuilt C++ library calls, and the locale they take is the
 * only one there is.
 */

#include <wchar.h>
#include <wctype.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>
#include <locale.h>
#include <stdio.h>
#include <time.h>

/* ----------------------------------------------------------- conversion */

wint_t btowc(int c) { return (c >= 0 && c < 0x80) ? (wint_t)c : WEOF; }
int wctob(wint_t c) { return c < 0x80 ? (int)c : EOF; }
int mbsinit(const mbstate_t *state) { return state == NULL || state->__count == 0; }

size_t mbrtowc(wchar_t *out, const char *s, size_t n, mbstate_t *state)
{
    (void)state;
    if (s == NULL) { return 0; }
    if (n == 0) { return (size_t)-2; }
    unsigned char c = (unsigned char)*s;
    if (c >= 0x80) { errno = EILSEQ; return (size_t)-1; }
    if (out) { *out = (wchar_t)c; }
    return c ? 1 : 0;
}

size_t mbrlen(const char *s, size_t n, mbstate_t *state) { return mbrtowc(NULL, s, n, state); }

size_t wcrtomb(char *out, wchar_t wc, mbstate_t *state)
{
    (void)state;
    if (out == NULL) { return 1; }
    if ((unsigned)wc >= 0x80) { errno = EILSEQ; return (size_t)-1; }
    *out = (char)wc;
    return 1;
}

size_t mbsnrtowcs(wchar_t *out, const char **src, size_t nms, size_t n, mbstate_t *state)
{
    (void)state;
    const char *s = *src;
    size_t done = 0;
    while (nms > 0 && (out == NULL || done < n)) {
        unsigned char c = (unsigned char)*s;
        if (c >= 0x80) { errno = EILSEQ; *src = s; return (size_t)-1; }
        if (out) { out[done] = (wchar_t)c; }
        if (c == 0) {
            if (out) { *src = NULL; }
            return done;
        }
        done++;
        s++;
        nms--;
    }
    if (out) { *src = s; }
    return done;
}

size_t mbsrtowcs(wchar_t *out, const char **src, size_t n, mbstate_t *state)
{
    return mbsnrtowcs(out, src, (size_t)-1, n, state);
}

size_t wcsnrtombs(char *out, const wchar_t **src, size_t nwc, size_t n, mbstate_t *state)
{
    (void)state;
    const wchar_t *s = *src;
    size_t done = 0;
    while (nwc > 0 && (out == NULL || done < n)) {
        wchar_t c = *s;
        if ((unsigned)c >= 0x80) { errno = EILSEQ; *src = s; return (size_t)-1; }
        if (out) { out[done] = (char)c; }
        if (c == 0) {
            if (out) { *src = NULL; }
            return done;
        }
        done++;
        s++;
        nwc--;
    }
    if (out) { *src = s; }
    return done;
}

size_t wcsrtombs(char *out, const wchar_t **src, size_t n, mbstate_t *state)
{
    return wcsnrtombs(out, src, (size_t)-1, n, state);
}

int mblen(const char *s, size_t n) { return s == NULL ? 0 : (int)mbrlen(s, n, NULL); }
int mbtowc(wchar_t *out, const char *s, size_t n) { return s == NULL ? 0 : (int)mbrtowc(out, s, n, NULL); }
int wctomb(char *out, wchar_t wc) { return out == NULL ? 0 : (int)wcrtomb(out, wc, NULL); }

size_t mbstowcs(wchar_t *out, const char *s, size_t n)
{
    const char *from = s;
    return mbsrtowcs(out, &from, n, NULL);
}

size_t wcstombs(char *out, const wchar_t *s, size_t n)
{
    const wchar_t *from = s;
    return wcsrtombs(out, &from, n, NULL);
}

/* ---------------------------------------------------------------- streams */

/* A wide character on a stream is the one byte the C locale gives it. */
wint_t fgetwc(FILE *stream)
{
    int c = fgetc(stream);
    if (c == EOF) { return WEOF; }
    wint_t wide = btowc(c);
    if (wide == WEOF) { errno = EILSEQ; }
    return wide;
}

wint_t getwc(FILE *stream) { return fgetwc(stream); }

wint_t fputwc(wchar_t c, FILE *stream)
{
    int narrow = wctob((wint_t)c);
    if (narrow == EOF) { errno = EILSEQ; return WEOF; }
    return fputc(narrow, stream) == EOF ? WEOF : (wint_t)c;
}

wint_t putwc(wchar_t c, FILE *stream) { return fputwc(c, stream); }

wint_t ungetwc(wint_t c, FILE *stream)
{
    int narrow = wctob(c);
    if (c == WEOF || narrow == EOF) { return WEOF; }
    return ungetc(narrow, stream) == EOF ? WEOF : c;
}

/* ---------------------------------------------------------------- strings */

size_t wcslen(const wchar_t *s)
{
    size_t n = 0;
    while (s[n]) { n++; }
    return n;
}

wchar_t *wmemcpy(wchar_t *dst, const wchar_t *src, size_t n)
{
    return memcpy(dst, src, n * sizeof(wchar_t));
}

wchar_t *wmemmove(wchar_t *dst, const wchar_t *src, size_t n)
{
    return memmove(dst, src, n * sizeof(wchar_t));
}

wchar_t *wmemset(wchar_t *dst, wchar_t c, size_t n)
{
    for (size_t i = 0; i < n; i++) { dst[i] = c; }
    return dst;
}

wchar_t *wmemchr(const wchar_t *s, wchar_t c, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        if (s[i] == c) { return (wchar_t *)&s[i]; }
    }
    return NULL;
}

int wmemcmp(const wchar_t *a, const wchar_t *b, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) { return a[i] < b[i] ? -1 : 1; }
    }
    return 0;
}

int wcscmp(const wchar_t *a, const wchar_t *b)
{
    while (*a && *a == *b) { a++; b++; }
    return *a == *b ? 0 : (*a < *b ? -1 : 1);
}

int wcsncmp(const wchar_t *a, const wchar_t *b, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) { return a[i] < b[i] ? -1 : 1; }
        if (a[i] == 0) { return 0; }
    }
    return 0;
}

int wcscoll(const wchar_t *a, const wchar_t *b) { return wcscmp(a, b); }

size_t wcsxfrm(wchar_t *out, const wchar_t *s, size_t n)
{
    size_t length = wcslen(s);
    if (n > 0) {
        size_t copy = length < n - 1 ? length : n - 1;
        wmemcpy(out, s, copy);
        out[copy] = 0;
    }
    return length;
}

/* The format and the result are narrowed and widened through the C locale,
 * where every character `strftime` produces is below 0x80. */
size_t wcsftime(wchar_t *out, size_t n, const wchar_t *format, const struct tm *when)
{
    char narrow_format[256];
    size_t i = 0;
    for (; format[i] && i + 1 < sizeof(narrow_format); i++) {
        if ((unsigned)format[i] >= 0x80) { return 0; }
        narrow_format[i] = (char)format[i];
    }
    narrow_format[i] = 0;
    char narrow[512];
    size_t made = strftime(narrow, sizeof(narrow) < n ? sizeof(narrow) : n, narrow_format, when);
    for (size_t j = 0; j < made; j++) { out[j] = (wchar_t)(unsigned char)narrow[j]; }
    if (made < n) { out[made] = 0; }
    return made;
}

int __wcscoll_l(const wchar_t *a, const wchar_t *b, locale_t l) { (void)l; return wcscoll(a, b); }
size_t __wcsxfrm_l(wchar_t *out, const wchar_t *s, size_t n, locale_t l)
{
    (void)l;
    return wcsxfrm(out, s, n);
}
size_t __wcsftime_l(wchar_t *out, size_t n, const wchar_t *format, const struct tm *when,
                    locale_t l)
{
    (void)l;
    return wcsftime(out, n, format, when);
}

/* --------------------------------------------------------------- classes */

static int narrow_class(wint_t c, int (*test)(int)) { return c < 0x80 ? test((int)c) : 0; }

int iswalnum(wint_t c)  { return narrow_class(c, isalnum); }
int iswalpha(wint_t c)  { return narrow_class(c, isalpha); }
int iswblank(wint_t c)  { return narrow_class(c, isblank); }
int iswcntrl(wint_t c)  { return narrow_class(c, iscntrl); }
int iswdigit(wint_t c)  { return narrow_class(c, isdigit); }
int iswgraph(wint_t c)  { return narrow_class(c, isgraph); }
int iswlower(wint_t c)  { return narrow_class(c, islower); }
int iswprint(wint_t c)  { return narrow_class(c, isprint); }
int iswpunct(wint_t c)  { return narrow_class(c, ispunct); }
int iswspace(wint_t c)  { return narrow_class(c, isspace); }
int iswupper(wint_t c)  { return narrow_class(c, isupper); }
int iswxdigit(wint_t c) { return narrow_class(c, isxdigit); }
wint_t towlower(wint_t c) { return c < 0x80 ? (wint_t)tolower((int)c) : c; }
wint_t towupper(wint_t c) { return c < 0x80 ? (wint_t)toupper((int)c) : c; }

/* A class is named by the same mask `ctype.h` classifies with, which is what
 * lets `iswctype` answer from one table. */
wctype_t wctype(const char *name)
{
    static const struct { const char *name; unsigned short mask; } classes[] = {
        { "alnum", _ISalnum }, { "alpha", _ISalpha }, { "blank", _ISblank },
        { "cntrl", _IScntrl }, { "digit", _ISdigit }, { "graph", _ISgraph },
        { "lower", _ISlower }, { "print", _ISprint }, { "punct", _ISpunct },
        { "space", _ISspace }, { "upper", _ISupper }, { "xdigit", _ISxdigit },
    };
    for (size_t i = 0; i < sizeof(classes) / sizeof(classes[0]); i++) {
        if (strcmp(classes[i].name, name) == 0) { return classes[i].mask; }
    }
    return 0;
}

int iswctype(wint_t c, wctype_t type)
{
    if (c >= 0x80 || type == 0) { return 0; }
    return ((*__ctype_b_loc())[c] & (unsigned short)type) != 0;
}

static const int trans_lower = 1;
static const int trans_upper = 2;

wctrans_t wctrans(const char *name)
{
    if (strcmp(name, "tolower") == 0) { return &trans_lower; }
    if (strcmp(name, "toupper") == 0) { return &trans_upper; }
    return NULL;
}

wint_t towctrans(wint_t c, wctrans_t trans)
{
    if (trans == &trans_lower) { return towlower(c); }
    if (trans == &trans_upper) { return towupper(c); }
    return c;
}

wint_t __towlower_l(wint_t c, locale_t l) { (void)l; return towlower(c); }
wint_t __towupper_l(wint_t c, locale_t l) { (void)l; return towupper(c); }
wctype_t __wctype_l(const char *name, locale_t l) { (void)l; return wctype(name); }
int __iswctype_l(wint_t c, wctype_t type, locale_t l) { (void)l; return iswctype(c, type); }
