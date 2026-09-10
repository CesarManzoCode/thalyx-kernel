/* Formatted output onto the diagnostic plane, and into memory.
 *
 * The plane takes two integers a call, so text is emitted eight bytes at a time
 * with a marker note around it. That is deliberately awkward: it is a debugging
 * channel, it coalesces, and nothing durable may be derived from it. Anything a
 * program wants remembered goes into managed state instead.
 *
 * The float conversions are not correctly rounded and say so here rather than
 * in a footnote: they split the value at the decimal point and expand each half
 * with integer arithmetic, which is right to about fifteen significant digits
 * and wrong in the last place for values that need more. Nothing on this system
 * derives a number from them -- the language runtime carries its own `dtoa` and
 * the inference engine never prints one -- so the shortcoming is in diagnostics
 * only. A program that needed exact text would be wrong to ask `printf` for it.
 */

#include "thalyx/nrt.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <math.h>

#define TH_NOTE_TEXT 0x5006ull

struct th_file { int kind; char *out; size_t size; size_t at; };

static struct th_file diag_stream = { 0, NULL, 0, 0 };
FILE *th_stdout = &diag_stream;
FILE *th_stderr = &diag_stream;

static void emit_text(const char *text, size_t len)
{
    size_t at = 0;
    while (at < len) {
        uint64_t packed = 0;
        size_t take = len - at < 8 ? len - at : 8;
        memcpy(&packed, text + at, take);
        th_note(TH_NOTE_TEXT | (take << 32), packed);
        at += take;
    }
}

static void put(struct th_file *stream, char c)
{
    if (stream == NULL) { return; }
    if (stream->kind == 1) {
        if (stream->at + 1 < stream->size) { stream->out[stream->at] = c; }
        stream->at++;
        return;
    }
    char one = c;
    emit_text(&one, 1);
}

static void put_bytes(struct th_file *stream, const char *text, size_t len)
{
    if (stream == NULL) { return; }
    if (stream->kind == 1) {
        for (size_t i = 0; i < len; i++) {
            if (stream->at + 1 < stream->size) { stream->out[stream->at] = text[i]; }
            stream->at++;
        }
        return;
    }
    emit_text(text, len);
}

enum { FLAG_LEFT = 1, FLAG_ZERO = 2, FLAG_PLUS = 4, FLAG_SPACE = 8, FLAG_ALT = 16 };

static void pad(struct th_file *stream, char with, int count)
{
    for (int i = 0; i < count; i++) { put(stream, with); }
}

static int emit_field(struct th_file *stream, const char *body, int len,
                      int width, int flags, const char *prefix)
{
    int prefix_len = prefix ? (int)strlen(prefix) : 0;
    int total = len + prefix_len;
    int fill = width > total ? width - total : 0;
    if (!(flags & FLAG_LEFT) && !(flags & FLAG_ZERO)) { pad(stream, ' ', fill); }
    if (prefix_len) { put_bytes(stream, prefix, (size_t)prefix_len); }
    if (!(flags & FLAG_LEFT) && (flags & FLAG_ZERO)) { pad(stream, '0', fill); }
    put_bytes(stream, body, (size_t)len);
    if (flags & FLAG_LEFT) { pad(stream, ' ', fill); }
    return total + fill;
}

static int unsigned_to(char *out, unsigned long long value, unsigned base, int upper)
{
    static const char lower_digits[] = "0123456789abcdef";
    static const char upper_digits[] = "0123456789ABCDEF";
    const char *digits = upper ? upper_digits : lower_digits;
    char scratch[24];
    int len = 0;
    do { scratch[len++] = digits[value % base]; value /= base; } while (value);
    for (int i = 0; i < len; i++) { out[i] = scratch[len - 1 - i]; }
    return len;
}

/* Fixed-point expansion. See the file header for what this does not promise. */
static int double_to(char *out, double value, int precision, int *sign)
{
    *sign = 0;
    if (value < 0 || (value == 0 && signbit(value))) { *sign = 1; value = -value; }
    if (isnan(value)) { memcpy(out, "nan", 3); return 3; }
    if (isinf(value)) { memcpy(out, "inf", 3); return 3; }
    if (precision < 0) { precision = 6; }
    if (precision > 17) { precision = 17; }

    double rounder = 0.5;
    for (int i = 0; i < precision; i++) { rounder /= 10.0; }
    value += rounder;

    unsigned long long whole = (unsigned long long)value;
    double fraction = value - (double)whole;
    int len = unsigned_to(out, whole, 10, 0);
    if (precision > 0) {
        out[len++] = '.';
        for (int i = 0; i < precision; i++) {
            fraction *= 10.0;
            int digit = (int)fraction;
            if (digit < 0) { digit = 0; }
            if (digit > 9) { digit = 9; }
            out[len++] = (char)('0' + digit);
            fraction -= digit;
        }
    }
    return len;
}

int vfprintf(FILE *stream, const char *format, va_list args)
{
    int written = 0;
    char body[512];

    for (const char *p = format; *p; p++) {
        if (*p != '%') { put(stream, *p); written++; continue; }
        p++;
        int flags = 0;
        for (;; p++) {
            if (*p == '-') { flags |= FLAG_LEFT; }
            else if (*p == '0') { flags |= FLAG_ZERO; }
            else if (*p == '+') { flags |= FLAG_PLUS; }
            else if (*p == ' ') { flags |= FLAG_SPACE; }
            else if (*p == '#') { flags |= FLAG_ALT; }
            else { break; }
        }
        int width = 0;
        if (*p == '*') { width = va_arg(args, int); p++; if (width < 0) { flags |= FLAG_LEFT; width = -width; } }
        else { while (*p >= '0' && *p <= '9') { width = width * 10 + (*p++ - '0'); } }
        int precision = -1;
        if (*p == '.') {
            p++;
            precision = 0;
            if (*p == '*') { precision = va_arg(args, int); p++; }
            else { while (*p >= '0' && *p <= '9') { precision = precision * 10 + (*p++ - '0'); } }
        }
        int longs = 0, size_t_arg = 0;
        for (;;) {
            if (*p == 'l') { longs++; p++; }
            else if (*p == 'h') { p++; }
            else if (*p == 'z' || *p == 'j' || *p == 't') { size_t_arg = 1; p++; }
            else { break; }
        }

        char conversion = *p;
        const char *prefix = NULL;
        int len = 0;

        switch (conversion) {
        case 'd': case 'i': {
            long long value = size_t_arg ? (long long)va_arg(args, size_t)
                            : longs >= 2 ? va_arg(args, long long)
                            : longs == 1 ? (long long)va_arg(args, long)
                                         : (long long)va_arg(args, int);
            unsigned long long magnitude = value < 0 ? (unsigned long long)(-(value + 1)) + 1u
                                                     : (unsigned long long)value;
            len = unsigned_to(body, magnitude, 10, 0);
            if (value < 0) { prefix = "-"; }
            else if (flags & FLAG_PLUS) { prefix = "+"; }
            else if (flags & FLAG_SPACE) { prefix = " "; }
            break;
        }
        case 'u': case 'x': case 'X': case 'o': {
            unsigned long long value = size_t_arg ? (unsigned long long)va_arg(args, size_t)
                                     : longs >= 2 ? va_arg(args, unsigned long long)
                                     : longs == 1 ? (unsigned long long)va_arg(args, unsigned long)
                                                  : (unsigned long long)va_arg(args, unsigned int);
            unsigned base = conversion == 'u' ? 10u : conversion == 'o' ? 8u : 16u;
            len = unsigned_to(body, value, base, conversion == 'X');
            if ((flags & FLAG_ALT) && conversion == 'x') { prefix = "0x"; }
            if ((flags & FLAG_ALT) && conversion == 'X') { prefix = "0X"; }
            break;
        }
        case 'p': {
            void *value = va_arg(args, void *);
            len = unsigned_to(body, (unsigned long long)(uintptr_t)value, 16, 0);
            prefix = "0x";
            break;
        }
        case 'c': {
            body[0] = (char)va_arg(args, int);
            len = 1;
            break;
        }
        case 's': {
            const char *text = va_arg(args, const char *);
            if (text == NULL) { text = "(null)"; }
            int available = precision >= 0 ? (int)strnlen(text, (size_t)precision) : (int)strlen(text);
            int fill = width > available ? width - available : 0;
            if (!(flags & FLAG_LEFT)) { pad(stream, ' ', fill); }
            put_bytes(stream, text, (size_t)available);
            if (flags & FLAG_LEFT) { pad(stream, ' ', fill); }
            written += available + fill;
            continue;
        }
        case 'f': case 'F': case 'g': case 'G': case 'e': case 'E': {
            double value = va_arg(args, double);
            int sign = 0;
            len = double_to(body, value, precision < 0 ? 6 : precision, &sign);
            if (sign) { prefix = "-"; }
            else if (flags & FLAG_PLUS) { prefix = "+"; }
            break;
        }
        case '%': body[0] = '%'; len = 1; break;
        default:
            body[0] = '%'; body[1] = conversion; len = conversion ? 2 : 1;
            break;
        }

        written += emit_field(stream, body, len, width, flags, prefix);
        if (conversion == 0) { break; }
    }
    return written;
}

int vsnprintf(char *out, size_t size, const char *format, va_list args)
{
    struct th_file sink = { 1, out, size, 0 };
    int written = vfprintf(&sink, format, args);
    if (out && size) { out[sink.at < size ? sink.at : size - 1] = 0; }
    return written;
}

int snprintf(char *out, size_t size, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = vsnprintf(out, size, format, args);
    va_end(args);
    return written;
}

int printf(const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = vfprintf(th_stdout, format, args);
    va_end(args);
    return written;
}

int fprintf(FILE *stream, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = vfprintf(stream, format, args);
    va_end(args);
    return written;
}

int fputs(const char *text, FILE *stream) { put_bytes(stream, text, strlen(text)); return 0; }
int fputc(int c, FILE *stream) { put(stream, (char)c); return c; }
int putchar(int c) { put(th_stdout, (char)c); return c; }
int puts(const char *text) { fputs(text, th_stdout); put(th_stdout, '\n'); return 0; }
size_t fwrite(const void *data, size_t size, size_t count, FILE *stream)
{
    put_bytes(stream, data, size * count);
    return count;
}
int fflush(FILE *stream) { (void)stream; return 0; }

void th_log(const char *text) { emit_text(text, strlen(text)); }
