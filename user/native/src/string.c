/* The memory and string primitives, written out.
 *
 * GCC emits calls to `memcpy`, `memset`, `memmove` and `memcmp` from ordinary
 * code whatever the source says, so these four are not optional even for a
 * program that never mentions them. The rest are what the ported runtimes use.
 *
 * `strerror` has glibc's message for every number this system produces, so a
 * program that reports "could not open /bulk: No such file or directory" is
 * reporting what happened in the words its authors wrote the check against.
 */

#include <string.h>
#include <strings.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdio.h>
#include <ctype.h>
#include <errno.h>

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

void *memrchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    while (n--) {
        if (p[n] == (unsigned char)c) { return (void *)(p + n); }
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

char *strncat(char *dst, const char *src, size_t n)
{
    char *end = dst + strlen(dst);
    size_t i = 0;
    for (; i < n && src[i]; i++) { end[i] = src[i]; }
    end[i] = 0;
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

int strcasecmp(const char *a, const char *b)
{
    for (;; a++, b++) {
        int x = tolower((unsigned char)*a), y = tolower((unsigned char)*b);
        if (x != y || x == 0) { return x - y; }
    }
}

int strncasecmp(const char *a, const char *b, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        int x = tolower((unsigned char)a[i]), y = tolower((unsigned char)b[i]);
        if (x != y || x == 0) { return x - y; }
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

size_t strspn(const char *s, const char *accept)
{
    size_t n = 0;
    while (s[n] && strchr(accept, s[n])) { n++; }
    return n;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t n = 0;
    while (s[n] && !strchr(reject, s[n])) { n++; }
    return n;
}

char *strpbrk(const char *s, const char *accept)
{
    s += strcspn(s, accept);
    return *s ? (char *)s : NULL;
}

char *strtok_r(char *s, const char *delim, char **save)
{
    if (s == NULL) { s = *save; }
    if (s == NULL) { return NULL; }
    s += strspn(s, delim);
    if (*s == 0) { *save = NULL; return NULL; }
    char *end = s + strcspn(s, delim);
    if (*end) { *end = 0; *save = end + 1; } else { *save = NULL; }
    return s;
}

char *strtok(char *s, const char *delim)
{
    static char *save;
    return strtok_r(s, delim, &save);
}

char *strdup(const char *s)
{
    size_t n = strlen(s) + 1;
    char *out = malloc(n);
    if (out) { memcpy(out, s, n); }
    return out;
}

char *strndup(const char *s, size_t n)
{
    size_t length = strnlen(s, n);
    char *out = malloc(length + 1);
    if (out) {
        memcpy(out, s, length);
        out[length] = 0;
    }
    return out;
}

static const char *message_of(int errnum)
{
    switch (errnum) {
    case 0:            return "Success";
    case EPERM:        return "Operation not permitted";
    case ENOENT:       return "No such file or directory";
    case ESRCH:        return "No such process";
    case EINTR:        return "Interrupted system call";
    case EIO:          return "Input/output error";
    case ENXIO:        return "No such device or address";
    case E2BIG:        return "Argument list too long";
    case ENOEXEC:      return "Exec format error";
    case EBADF:        return "Bad file descriptor";
    case ECHILD:       return "No child processes";
    case EAGAIN:       return "Resource temporarily unavailable";
    case ENOMEM:       return "Cannot allocate memory";
    case EACCES:       return "Permission denied";
    case EFAULT:       return "Bad address";
    case EBUSY:        return "Device or resource busy";
    case EEXIST:       return "File exists";
    case EXDEV:        return "Invalid cross-device link";
    case ENODEV:       return "No such device";
    case ENOTDIR:      return "Not a directory";
    case EISDIR:       return "Is a directory";
    case EINVAL:       return "Invalid argument";
    case ENFILE:       return "Too many open files in system";
    case EMFILE:       return "Too many open files";
    case ENOTTY:       return "Inappropriate ioctl for device";
    case EFBIG:        return "File too large";
    case ENOSPC:       return "No space left on device";
    case ESPIPE:       return "Illegal seek";
    case EROFS:        return "Read-only file system";
    case EMLINK:       return "Too many links";
    case EPIPE:        return "Broken pipe";
    case EDOM:         return "Numerical argument out of domain";
    case ERANGE:       return "Numerical result out of range";
    case EDEADLK:      return "Resource deadlock avoided";
    case ENAMETOOLONG: return "File name too long";
    case ENOLCK:       return "No locks available";
    case ENOSYS:       return "Function not implemented";
    case ENOTEMPTY:    return "Directory not empty";
    case ELOOP:        return "Too many levels of symbolic links";
    case EOVERFLOW:    return "Value too large for defined data type";
    case EILSEQ:       return "Invalid or incomplete multibyte or wide character";
    case EOPNOTSUPP:   return "Operation not supported";
    case ETIMEDOUT:    return "Connection timed out";
    case ECANCELED:    return "Operation canceled";
    case EOWNERDEAD:   return "Owner died";
    case ENOTRECOVERABLE: return "State not recoverable";
    default:           return NULL;
    }
}

char *strerror_r(int errnum, char *into, size_t n)
{
    const char *message = message_of(errnum);
    if (message) { return (char *)message; }
    snprintf(into, n, "Unknown error %d", errnum);
    return into;
}

char *strerror(int errnum)
{
    static char unknown[32];
    return strerror_r(errnum, unknown, sizeof(unknown));
}
