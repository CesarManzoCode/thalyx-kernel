/* The checked entry points a fortified build calls.
 *
 * The prebuilt C++ library was compiled with `_FORTIFY_SOURCE`, so where the
 * compiler knew the size of a destination it called `__memcpy_chk` instead of
 * `memcpy`, and so on, passing that size along. Each check here is the check
 * glibc makes: if the operation would write past the size the compiler knew,
 * the program stops, because the alternative is a write past an object that
 * nothing downstream will notice. Otherwise it is the plain operation.
 */

#include "thalyx/nrt.h"
#include <stdio.h>
#include <stdarg.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <wchar.h>
#include <errno.h>

#define TH_NOTE_OVERFLOW 0x500Full

_Noreturn void __chk_fail(void)
{
    th_note(TH_NOTE_OVERFLOW, (uint64_t)(uintptr_t)__builtin_return_address(0));
    th_signal_raise(TH_SLOT_SIGNAL_DONE, TH_BIT_PROGRAM_DONE);
    th_exit((uint64_t)-7);
}

void *__memcpy_chk(void *dst, const void *src, size_t len, size_t dstlen)
{
    if (len > dstlen) { __chk_fail(); }
    return memcpy(dst, src, len);
}

void *__memmove_chk(void *dst, const void *src, size_t len, size_t dstlen)
{
    if (len > dstlen) { __chk_fail(); }
    return memmove(dst, src, len);
}

void *__memset_chk(void *dst, int c, size_t len, size_t dstlen)
{
    if (len > dstlen) { __chk_fail(); }
    return memset(dst, c, len);
}

int __vsprintf_chk(char *out, int flag, size_t slen, const char *format, va_list args)
{
    (void)flag;
    int written = vsnprintf(out, slen, format, args);
    if (written >= 0 && (size_t)written >= slen) { __chk_fail(); }
    return written;
}

int __sprintf_chk(char *out, int flag, size_t slen, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = __vsprintf_chk(out, flag, slen, format, args);
    va_end(args);
    return written;
}

int __vsnprintf_chk(char *out, size_t n, int flag, size_t slen, const char *format, va_list args)
{
    (void)flag;
    if (n > slen) { __chk_fail(); }
    return vsnprintf(out, n, format, args);
}

int __snprintf_chk(char *out, size_t n, int flag, size_t slen, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = __vsnprintf_chk(out, n, flag, slen, format, args);
    va_end(args);
    return written;
}

int __vfprintf_chk(FILE *stream, int flag, const char *format, va_list args)
{
    (void)flag;
    return vfprintf(stream, format, args);
}

int __fprintf_chk(FILE *stream, int flag, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = __vfprintf_chk(stream, flag, format, args);
    va_end(args);
    return written;
}

int __printf_chk(int flag, const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int written = __vfprintf_chk(stdout, flag, format, args);
    va_end(args);
    return written;
}

ssize_t __read_chk(int fd, void *into, size_t n, size_t buflen)
{
    if (n > buflen) { __chk_fail(); }
    return read(fd, into, n);
}

wchar_t *__wmemcpy_chk(wchar_t *dst, const wchar_t *src, size_t n, size_t dstlen)
{
    if (n > dstlen) { __chk_fail(); }
    return wmemcpy(dst, src, n);
}

wchar_t *__wmemmove_chk(wchar_t *dst, const wchar_t *src, size_t n, size_t dstlen)
{
    if (n > dstlen) { __chk_fail(); }
    return wmemmove(dst, src, n);
}

wchar_t *__wmemset_chk(wchar_t *dst, wchar_t c, size_t n, size_t dstlen)
{
    if (n > dstlen) { __chk_fail(); }
    return wmemset(dst, c, n);
}

size_t __mbsrtowcs_chk(wchar_t *dst, const char **src, size_t len, mbstate_t *state,
                       size_t dstlen)
{
    if (dst != NULL && len > dstlen) { __chk_fail(); }
    return mbsrtowcs(dst, src, len, state);
}

size_t __mbsnrtowcs_chk(wchar_t *dst, const char **src, size_t nms, size_t len,
                        mbstate_t *state, size_t dstlen)
{
    if (dst != NULL && len > dstlen) { __chk_fail(); }
    return mbsnrtowcs(dst, src, nms, len, state);
}

size_t __wcsrtombs_chk(char *dst, const wchar_t **src, size_t len, mbstate_t *state,
                       size_t dstlen)
{
    if (dst != NULL && len > dstlen) { __chk_fail(); }
    return wcsrtombs(dst, src, len, state);
}

/* Relative to a directory descriptor. There are no directory descriptors
 * here, so a path is opened only when it does not need one. */
#define AT_FDCWD (-100)

int openat(int dirfd, const char *path, int flags, ...)
{
    if (path == NULL) { errno = EFAULT; return -1; }
    if (path[0] != '/' && dirfd != AT_FDCWD) { errno = EBADF; return -1; }
    return open(path, flags);
}

int __openat_2(int dirfd, const char *path, int flags)
{
    return openat(dirfd, path, flags);
}

int __open_2(const char *path, int flags) { return open(path, flags); }
