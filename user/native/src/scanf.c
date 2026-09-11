/* Formatted input from a string.
 *
 * The conversions ported programs use -- integers in every base and width,
 * floating point, strings, single characters, scan sets and `%n` -- with field
 * widths and assignment suppression. Numbers are converted by the same
 * `strtoll`, `strtoull` and `strtod` everything else here uses, so a number
 * scanned and a number parsed are the same number.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <stdarg.h>

static const char *skip_space(const char *s)
{
    while (isspace((unsigned char)*s)) { s++; }
    return s;
}

int vsscanf(const char *input, const char *format, va_list args)
{
    const char *s = input;
    int assigned = 0;
    int consumed_any = 0;

    for (const char *f = format; *f; f++) {
        if (isspace((unsigned char)*f)) { s = skip_space(s); continue; }
        if (*f != '%') {
            if (*s != *f) { break; }
            s++;
            continue;
        }
        f++;
        if (*f == '%') {
            s = skip_space(s);
            if (*s != '%') { break; }
            s++;
            continue;
        }
        int suppress = 0;
        if (*f == '*') { suppress = 1; f++; }
        int width = 0;
        while (*f >= '0' && *f <= '9') { width = width * 10 + (*f++ - '0'); }
        int longs = 0, shorts = 0, sizes = 0;
        for (;;) {
            if (*f == 'l') { longs++; f++; }
            else if (*f == 'h') { shorts++; f++; }
            else if (*f == 'z' || *f == 'j' || *f == 't') { sizes = 1; f++; }
            else if (*f == 'L') { longs = 2; f++; }
            else { break; }
        }
        char conversion = *f;
        if (conversion == 0) { break; }

        if (conversion == 'n') {
            if (!suppress) { *va_arg(args, int *) = (int)(s - input); }
            continue;
        }
        if (conversion != 'c' && conversion != '[') { s = skip_space(s); }
        if (*s == 0) { break; }

        char field[512];
        size_t limit = width > 0 && (size_t)width < sizeof(field) ? (size_t)width : sizeof(field) - 1;

        switch (conversion) {
        case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
            size_t n = strnlen(s, limit);
            memcpy(field, s, n);
            field[n] = 0;
            char *end;
            int base = conversion == 'd' || conversion == 'u' ? 10
                     : conversion == 'o' ? 8 : conversion == 'i' ? 0 : 16;
            unsigned long long value = conversion == 'd' || conversion == 'i'
                ? (unsigned long long)strtoll(field, &end, base)
                : strtoull(field, &end, base);
            if (end == field) { return assigned ? assigned : (consumed_any ? 0 : EOF); }
            s += end - field;
            consumed_any = 1;
            if (!suppress) {
                if (sizes || longs >= 1) { *va_arg(args, unsigned long *) = (unsigned long)value; }
                else if (shorts == 1) { *va_arg(args, unsigned short *) = (unsigned short)value; }
                else if (shorts >= 2) { *va_arg(args, unsigned char *) = (unsigned char)value; }
                else { *va_arg(args, unsigned *) = (unsigned)value; }
                assigned++;
            }
            break;
        }
        case 'f': case 'F': case 'g': case 'G': case 'e': case 'E': case 'a': {
            size_t n = strnlen(s, limit);
            memcpy(field, s, n);
            field[n] = 0;
            char *end;
            double value = strtod(field, &end);
            if (end == field) { return assigned ? assigned : (consumed_any ? 0 : EOF); }
            s += end - field;
            consumed_any = 1;
            if (!suppress) {
                if (longs == 2) { *va_arg(args, long double *) = (long double)value; }
                else if (longs == 1) { *va_arg(args, double *) = value; }
                else { *va_arg(args, float *) = (float)value; }
                assigned++;
            }
            break;
        }
        case 's': {
            char *out = suppress ? NULL : va_arg(args, char *);
            size_t n = 0;
            while (s[n] && !isspace((unsigned char)s[n]) && (width == 0 || n < (size_t)width)) {
                if (out) { out[n] = s[n]; }
                n++;
            }
            if (out) { out[n] = 0; assigned++; }
            s += n;
            consumed_any = 1;
            break;
        }
        case 'c': {
            size_t n = width > 0 ? (size_t)width : 1;
            if (strnlen(s, n) < n) { return assigned; }
            if (!suppress) { memcpy(va_arg(args, char *), s, n); assigned++; }
            s += n;
            consumed_any = 1;
            break;
        }
        case '[': {
            f++;
            int negate = 0;
            if (*f == '^') { negate = 1; f++; }
            const char *set = f;
            if (*f == ']') { f++; }
            while (*f && *f != ']') { f++; }
            size_t set_len = (size_t)(f - set);
            char *out = suppress ? NULL : va_arg(args, char *);
            size_t n = 0;
            while (s[n] && (width == 0 || n < (size_t)width)) {
                int member = memchr(set, s[n], set_len) != NULL;
                if (member == negate) { break; }
                if (out) { out[n] = s[n]; }
                n++;
            }
            if (n == 0) { return assigned; }
            if (out) { out[n] = 0; assigned++; }
            s += n;
            consumed_any = 1;
            break;
        }
        default:
            return assigned;
        }
    }
    return assigned;
}

int sscanf(const char *s, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int n = vsscanf(s, format, args);
    va_end(args);
    return n;
}

/* Input from a stream is input from the only stream there is to read
 * formatted text from, and there is none: stdin is always at its end. */
int vfscanf(FILE *stream, const char *format, va_list args)
{
    (void)stream;
    (void)format;
    (void)args;
    return EOF;
}

int fscanf(FILE *stream, const char *format, ...)
{
    (void)stream;
    (void)format;
    return EOF;
}

int vscanf(const char *format, va_list args) { return vfscanf(stdin, format, args); }
int scanf(const char *format, ...) { (void)format; return EOF; }
