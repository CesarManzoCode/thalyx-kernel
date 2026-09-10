/* The memory and string primitives, written out.
 *
 * GCC emits calls to `memcpy`, `memset`, `memmove` and `memcmp` from ordinary
 * code whatever the source says, so these four are not optional even for a
 * program that never mentions them. The rest are what the ported runtimes use.
 */

#include <string.h>
#include <stdlib.h>
#include <stdint.h>

void *memcpy(void *dst, const void *src, size_t n)
{
    unsigned char *d = dst;
    const unsigned char *s = src;
    while (n >= 8) { __builtin_memcpy(d, s, 8); d += 8; s += 8; n -= 8; }
    while (n--) { *d++ = *s++; }
    return dst;
}

void *memmove(void *dst, const void *src, size_t n)
{
    unsigned char *d = dst;
    const unsigned char *s = src;
    if (d == s || n == 0) { return dst; }
    if (d < s) { return memcpy(dst, src, n); }
    d += n; s += n;
    while (n--) { *--d = *--s; }
    return dst;
}

void *memset(void *dst, int c, size_t n)
{
    unsigned char *d = dst;
    unsigned char v = (unsigned char)c;
    uint64_t wide = 0x0101010101010101ull * v;
    while (n >= 8) { __builtin_memcpy(d, &wide, 8); d += 8; n -= 8; }
    while (n--) { *d++ = v; }
    return dst;
}

int memcmp(const void *a, const void *b, size_t n)
{
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; i++) {
        if (x[i] != y[i]) { return (int)x[i] - (int)y[i]; }
    }
    return 0;
}

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    for (size_t i = 0; i < n; i++) {
        if (p[i] == (unsigned char)c) { return (void *)(p + i); }
    }
    return NULL;
}

size_t strlen(const char *s)
{
    const char *p = s;
    while (*p) { p++; }
    return (size_t)(p - s);
}

size_t strnlen(const char *s, size_t n)
{
    size_t i = 0;
    while (i < n && s[i]) { i++; }
    return i;
}

char *strcpy(char *dst, const char *src)
{
    char *out = dst;
    while ((*out++ = *src++) != 0) { }
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n)
{
    size_t i = 0;
    for (; i < n && src[i]; i++) { dst[i] = src[i]; }
    for (; i < n; i++) { dst[i] = 0; }
    return dst;
}

char *strcat(char *dst, const char *src)
{
    strcpy(dst + strlen(dst), src);
    return dst;
}

int strcmp(const char *a, const char *b)
{
    while (*a && *a == *b) { a++; b++; }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        unsigned char x = (unsigned char)a[i], y = (unsigned char)b[i];
        if (x != y) { return (int)x - (int)y; }
        if (x == 0) { return 0; }
    }
    return 0;
}

char *strchr(const char *s, int c)
{
    for (;; s++) {
        if (*s == (char)c) { return (char *)s; }
        if (*s == 0) { return NULL; }
    }
}

char *strrchr(const char *s, int c)
{
    const char *found = NULL;
    for (;; s++) {
        if (*s == (char)c) { found = s; }
        if (*s == 0) { return (char *)found; }
    }
}

char *strstr(const char *haystack, const char *needle)
{
    size_t n = strlen(needle);
    if (n == 0) { return (char *)haystack; }
    for (; *haystack; haystack++) {
        if (strncmp(haystack, needle, n) == 0) { return (char *)haystack; }
    }
    return NULL;
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *out = malloc(n);
    if (out) { memcpy(out, s, n); }
    return out;
}

int th_errno_storage = 0;

char *strerror(int errnum)
{
    (void)errnum;
    return (char *)"error";
}
