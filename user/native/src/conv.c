/* Number parsing and the two searches, which every ported runtime asks for.
 *
 * `strtod` here is a plain accumulate-then-scale conversion, correct to within
 * an ulp or two rather than correctly rounded. The language runtime does not
 * use it -- quickjs carries its own `dtoa`/`strtod` pair, which is exactly why
 * it does -- so what remains is configuration and diagnostics.
 */

#include <stdlib.h>
#include <ctype.h>
#include <string.h>
#include <math.h>

static unsigned long long parse_unsigned(const char *s, char **end, int base, int *any)
{
    unsigned long long value = 0;
    *any = 0;
    if (base == 0) {
        if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
        else if (s[0] == '0') { base = 8; }
        else { base = 10; }
    } else if (base == 16 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }
    for (;; s++) {
        int digit;
        if (*s >= '0' && *s <= '9') { digit = *s - '0'; }
        else if (*s >= 'a' && *s <= 'z') { digit = *s - 'a' + 10; }
        else if (*s >= 'A' && *s <= 'Z') { digit = *s - 'A' + 10; }
        else { break; }
        if (digit >= base) { break; }
        value = value * (unsigned)base + (unsigned)digit;
        *any = 1;
    }
    if (end) { *end = (char *)s; }
    return value;
}

long long strtoll(const char *s, char **end, int base)
{
    while (isspace((unsigned char)*s)) { s++; }
    int negative = 0;
    if (*s == '-') { negative = 1; s++; } else if (*s == '+') { s++; }
    int any = 0;
    unsigned long long value = parse_unsigned(s, end, base, &any);
    if (!any && end) { *end = (char *)s; }
    return negative ? -(long long)value : (long long)value;
}

long strtol(const char *s, char **end, int base) { return (long)strtoll(s, end, base); }

unsigned long long strtoull(const char *s, char **end, int base)
{
    while (isspace((unsigned char)*s)) { s++; }
    if (*s == '+') { s++; }
    int any = 0;
    unsigned long long value = parse_unsigned(s, end, base, &any);
    if (!any && end) { *end = (char *)s; }
    return value;
}

unsigned long strtoul(const char *s, char **end, int base)
{
    return (unsigned long)strtoull(s, end, base);
}

int atoi(const char *s) { return (int)strtoll(s, NULL, 10); }
int abs(int value) { return value < 0 ? -value : value; }
long labs(long value) { return value < 0 ? -value : value; }
long long llabs(long long value) { return value < 0 ? -value : value; }

double strtod(const char *s, char **end)
{
    const char *start = s;
    while (isspace((unsigned char)*s)) { s++; }
    int negative = 0;
    if (*s == '-') { negative = 1; s++; } else if (*s == '+') { s++; }

    if (strncmp(s, "Infinity", 8) == 0) { if (end) { *end = (char *)(s + 8); } return negative ? -HUGE_VAL : HUGE_VAL; }
    if (strncmp(s, "NaN", 3) == 0) { if (end) { *end = (char *)(s + 3); } return NAN; }

    double mantissa = 0.0;
    int digits = 0, exponent = 0;
    while (isdigit((unsigned char)*s)) { mantissa = mantissa * 10.0 + (*s++ - '0'); digits++; }
    if (*s == '.') {
        s++;
        while (isdigit((unsigned char)*s)) { mantissa = mantissa * 10.0 + (*s++ - '0'); digits++; exponent--; }
    }
    if (digits == 0) { if (end) { *end = (char *)start; } return 0.0; }
    if (*s == 'e' || *s == 'E') {
        const char *mark = s;
        s++;
        int sign = 1;
        if (*s == '-') { sign = -1; s++; } else if (*s == '+') { s++; }
        if (!isdigit((unsigned char)*s)) { s = mark; }
        else {
            int typed = 0;
            while (isdigit((unsigned char)*s)) { typed = typed * 10 + (*s++ - '0'); if (typed > 100000) { typed = 100000; } }
            exponent += sign * typed;
        }
    }
    if (end) { *end = (char *)s; }
    double scaled = mantissa * pow(10.0, (double)exponent);
    return negative ? -scaled : scaled;
}

float strtof(const char *s, char **end) { return (float)strtod(s, end); }

static void swap_bytes(unsigned char *a, unsigned char *b, size_t size)
{
    for (size_t i = 0; i < size; i++) { unsigned char t = a[i]; a[i] = b[i]; b[i] = t; }
}

/* Insertion sort for short runs, quicksort with a middle pivot above it. The
 * language runtime sorts property tables with this and nothing here sorts a
 * large array, so the simple version is the honest one. */
void qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *))
{
    unsigned char *items = base;
    if (count < 2) { return; }
    if (count < 12) {
        for (size_t i = 1; i < count; i++) {
            for (size_t j = i; j > 0 && compare(items + (j - 1) * size, items + j * size) > 0; j--) {
                swap_bytes(items + (j - 1) * size, items + j * size, size);
            }
        }
        return;
    }
    size_t middle = count / 2;
    swap_bytes(items, items + middle * size, size);
    size_t split = 0;
    for (size_t i = 1; i < count; i++) {
        if (compare(items + i * size, items) < 0) {
            split++;
            swap_bytes(items + split * size, items + i * size, size);
        }
    }
    swap_bytes(items, items + split * size, size);
    qsort(items, split, size, compare);
    qsort(items + (split + 1) * size, count - split - 1, size, compare);
}

void *bsearch(const void *key, const void *base, size_t count, size_t size,
              int (*compare)(const void *, const void *))
{
    const unsigned char *items = base;
    size_t low = 0, high = count;
    while (low < high) {
        size_t middle = (low + high) / 2;
        int order = compare(key, items + middle * size);
        if (order == 0) { return (void *)(items + middle * size); }
        if (order < 0) { high = middle; } else { low = middle + 1; }
    }
    return NULL;
}
