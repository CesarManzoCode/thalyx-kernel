/* The system interface a POSIX program expects, as far as this system has one.
 *
 * Descriptors 0, 1 and 2 exist: input always at its end, output and error on
 * the diagnostic plane. Others are the read-only files `stdio.h` describes.
 * `sysconf` answers from the kernel's own records -- the processors the kernel
 * brought up, the page size it runs on -- rather than from constants.
 *
 * `_POSIX_MAPPED_FILES` is deliberately not defined. There is no `mmap` here:
 * a domain maps memory objects through capabilities, and a program that would
 * have mapped a file reads it instead. llama.cpp keys its loader on exactly
 * this macro and takes its own read path when it is absent.
 */
#ifndef _UNISTD_H
#define _UNISTD_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
#include "thalyx/cdefs.h"

#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4

/* glibc's numbers for the names asked of `sysconf`. */
#define _SC_CLK_TCK           2
#define _SC_OPEN_MAX          4
#define _SC_PAGESIZE         30
#define _SC_PAGE_SIZE        _SC_PAGESIZE
#define _SC_NPROCESSORS_CONF 83
#define _SC_NPROCESSORS_ONLN 84
#define _SC_PHYS_PAGES       85

__TH_BEGIN_DECLS
long    sysconf(int name) __TH_NOTHROW;
ssize_t read(int fd, void *into, size_t n);
ssize_t write(int fd, const void *from, size_t n);
int     close(int fd);
off_t   lseek(int fd, off_t offset, int whence) __TH_NOTHROW;
int     isatty(int fd) __TH_NOTHROW;
int     dup(int fd) __TH_NOTHROW;
int     dup2(int fd, int to) __TH_NOTHROW;
int     access(const char *path, int mode) __TH_NOTHROW;
int     unlink(const char *path) __TH_NOTHROW;
char   *getcwd(char *into, size_t n) __TH_NOTHROW;
int     usleep(useconds_t microseconds);
unsigned sleep(unsigned seconds);
pid_t   getpid(void) __TH_NOTHROW;
/* There are no links here and no paths. Declared because ported code refers to
 * it on the platform it was written for; it always refuses. */
ssize_t readlink(const char *path, char *into, size_t size) __TH_NOTHROW;
__TH_END_DECLS

#endif
