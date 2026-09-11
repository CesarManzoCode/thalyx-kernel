/* Wide characters, in a system whose only locale is "C".
 *
 * The C++ standard library's `wchar_t` facets and `std::char_traits<wchar_t>`
 * are compiled against this interface, so it is declared whole. What
 * implements it is small: in the C locale a character is one byte, a byte
 * below 0x80 converts to the wide character of the same value, and a byte
 * above it is not a character at all -- `EILSEQ`, which is glibc's answer in
 * the same locale.
 *
 * `mbstate_t` has glibc's size and alignment. `std::streampos` carries one, and
 * the prebuilt library passes stream positions by value.
 */
#ifndef _WCHAR_H
#define _WCHAR_H

#include <stddef.h>
#include <stdarg.h>
#include "thalyx/cdefs.h"

#ifndef __wint_t_defined
#define __wint_t_defined 1
typedef unsigned int wint_t;
#endif

typedef struct {
    int __count;
    union {
        unsigned int __wch;
        char __wchb[4];
    } __value;
} __mbstate_t;
typedef __mbstate_t mbstate_t;

typedef struct th_file FILE;
struct tm;

#define WEOF (0xffffffffu)
#ifndef WCHAR_MIN
#define WCHAR_MIN __WCHAR_MIN__
#define WCHAR_MAX __WCHAR_MAX__
#endif

__TH_BEGIN_DECLS
wint_t btowc(int c) __TH_NOTHROW;
int    wctob(wint_t c) __TH_NOTHROW;
int    mbsinit(const mbstate_t *state) __TH_NOTHROW;
size_t mbrlen(const char *s, size_t n, mbstate_t *state) __TH_NOTHROW;
size_t mbrtowc(wchar_t *out, const char *s, size_t n, mbstate_t *state) __TH_NOTHROW;
size_t wcrtomb(char *out, wchar_t wc, mbstate_t *state) __TH_NOTHROW;
size_t mbsrtowcs(wchar_t *out, const char **src, size_t n, mbstate_t *state) __TH_NOTHROW;
size_t wcsrtombs(char *out, const wchar_t **src, size_t n, mbstate_t *state) __TH_NOTHROW;
size_t mbsnrtowcs(wchar_t *out, const char **src, size_t nms, size_t n, mbstate_t *state) __TH_NOTHROW;
size_t wcsnrtombs(char *out, const wchar_t **src, size_t nwc, size_t n, mbstate_t *state) __TH_NOTHROW;

wint_t fgetwc(FILE *stream);
wchar_t *fgetws(wchar_t *out, int n, FILE *stream);
wint_t fputwc(wchar_t c, FILE *stream);
int    fputws(const wchar_t *s, FILE *stream);
int    fwide(FILE *stream, int mode) __TH_NOTHROW;
int    fwprintf(FILE *stream, const wchar_t *format, ...);
int    fwscanf(FILE *stream, const wchar_t *format, ...);
wint_t getwc(FILE *stream);
wint_t getwchar(void);
wint_t putwc(wchar_t c, FILE *stream);
wint_t putwchar(wchar_t c);
int    swprintf(wchar_t *out, size_t n, const wchar_t *format, ...) __TH_NOTHROW;
int    swscanf(const wchar_t *s, const wchar_t *format, ...) __TH_NOTHROW;
wint_t ungetwc(wint_t c, FILE *stream);
int    vfwprintf(FILE *stream, const wchar_t *format, va_list args);
int    vfwscanf(FILE *stream, const wchar_t *format, va_list args);
int    vswprintf(wchar_t *out, size_t n, const wchar_t *format, va_list args) __TH_NOTHROW;
int    vswscanf(const wchar_t *s, const wchar_t *format, va_list args) __TH_NOTHROW;
int    vwprintf(const wchar_t *format, va_list args);
int    vwscanf(const wchar_t *format, va_list args);
int    wprintf(const wchar_t *format, ...);
int    wscanf(const wchar_t *format, ...);

wchar_t *wcscat(wchar_t *dst, const wchar_t *src) __TH_NOTHROW;
wchar_t *wcschr(const wchar_t *s, wchar_t c) __TH_NOTHROW;
int      wcscmp(const wchar_t *a, const wchar_t *b) __TH_NOTHROW;
int      wcscoll(const wchar_t *a, const wchar_t *b) __TH_NOTHROW;
wchar_t *wcscpy(wchar_t *dst, const wchar_t *src) __TH_NOTHROW;
size_t   wcscspn(const wchar_t *s, const wchar_t *reject) __TH_NOTHROW;
size_t   wcsftime(wchar_t *out, size_t n, const wchar_t *format, const struct tm *when) __TH_NOTHROW;
size_t   wcslen(const wchar_t *s) __TH_NOTHROW;
wchar_t *wcsncat(wchar_t *dst, const wchar_t *src, size_t n) __TH_NOTHROW;
int      wcsncmp(const wchar_t *a, const wchar_t *b, size_t n) __TH_NOTHROW;
wchar_t *wcsncpy(wchar_t *dst, const wchar_t *src, size_t n) __TH_NOTHROW;
wchar_t *wcspbrk(const wchar_t *s, const wchar_t *accept) __TH_NOTHROW;
wchar_t *wcsrchr(const wchar_t *s, wchar_t c) __TH_NOTHROW;
size_t   wcsspn(const wchar_t *s, const wchar_t *accept) __TH_NOTHROW;
wchar_t *wcsstr(const wchar_t *haystack, const wchar_t *needle) __TH_NOTHROW;
double   wcstod(const wchar_t *s, wchar_t **end) __TH_NOTHROW;
float    wcstof(const wchar_t *s, wchar_t **end) __TH_NOTHROW;
long double wcstold(const wchar_t *s, wchar_t **end) __TH_NOTHROW;
wchar_t *wcstok(wchar_t *s, const wchar_t *delim, wchar_t **save) __TH_NOTHROW;
long     wcstol(const wchar_t *s, wchar_t **end, int base) __TH_NOTHROW;
unsigned long wcstoul(const wchar_t *s, wchar_t **end, int base) __TH_NOTHROW;
long long wcstoll(const wchar_t *s, wchar_t **end, int base) __TH_NOTHROW;
unsigned long long wcstoull(const wchar_t *s, wchar_t **end, int base) __TH_NOTHROW;
size_t   wcsxfrm(wchar_t *out, const wchar_t *s, size_t n) __TH_NOTHROW;
wchar_t *wmemchr(const wchar_t *s, wchar_t c, size_t n) __TH_NOTHROW;
int      wmemcmp(const wchar_t *a, const wchar_t *b, size_t n) __TH_NOTHROW;
wchar_t *wmemcpy(wchar_t *dst, const wchar_t *src, size_t n) __TH_NOTHROW;
wchar_t *wmemmove(wchar_t *dst, const wchar_t *src, size_t n) __TH_NOTHROW;
wchar_t *wmemset(wchar_t *dst, wchar_t c, size_t n) __TH_NOTHROW;
__TH_END_DECLS

#endif
