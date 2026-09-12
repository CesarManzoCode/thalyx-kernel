/* Mapping a file, where the only files are regions a supervisor already mapped.
 *
 * A domain on this system does not acquire memory by asking for an address: it
 * holds a capability over a memory object and the mapping was installed when
 * the domain was built, charged to the scope that sponsored it. So `mmap` here
 * is not an allocator and cannot be one. It answers exactly one question --
 * "where is the region behind this descriptor?" -- and it answers it with the
 * address the region is already at.
 *
 * That makes the read-only case free and every other case impossible, which is
 * what this interface offers: `PROT_READ` on a bound region at offset zero
 * returns the existing mapping, and anything else is refused rather than
 * emulated with a copy. A program that wanted an anonymous mapping wants the
 * heap, and a program that wanted to write through a mapping wants an
 * authority it was not given.
 *
 * `munmap` refuses: the mapping belongs to the domain, not to the program, and
 * the program never charged for it. Callers that treat unmapping as an
 * optimisation carry on; callers that need it to be real find out that it was
 * not.
 */
#ifndef _SYS_MMAN_H
#define _SYS_MMAN_H

#include <stddef.h>
#include <sys/types.h>
#include "thalyx/cdefs.h"

#define PROT_NONE  0x0
#define PROT_READ  0x1
#define PROT_WRITE 0x2
#define PROT_EXEC  0x4

#define MAP_FILE      0x00
#define MAP_SHARED    0x01
#define MAP_PRIVATE   0x02
#define MAP_FIXED     0x10
#define MAP_ANONYMOUS 0x20
#define MAP_ANON      MAP_ANONYMOUS
#define MAP_POPULATE  0x8000

#define MAP_FAILED ((void *)-1)

/* Advice is accepted and has no effect: the pages are resident from the moment
 * the region was mapped, so there is nothing to fault in and nothing to evict.
 * Answering zero is the truth about what the advice achieved, which is
 * nothing, and not a claim that it did something. */
#define POSIX_MADV_NORMAL     0
#define POSIX_MADV_RANDOM     1
#define POSIX_MADV_SEQUENTIAL 2
#define POSIX_MADV_WILLNEED   3
#define POSIX_MADV_DONTNEED   4

#define MADV_NORMAL     POSIX_MADV_NORMAL
#define MADV_RANDOM     POSIX_MADV_RANDOM
#define MADV_SEQUENTIAL POSIX_MADV_SEQUENTIAL
#define MADV_WILLNEED   POSIX_MADV_WILLNEED
#define MADV_DONTNEED   POSIX_MADV_DONTNEED

__TH_BEGIN_DECLS
void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset) __TH_NOTHROW;
int   munmap(void *addr, size_t length) __TH_NOTHROW;
int   posix_madvise(void *addr, size_t length, int advice) __TH_NOTHROW;
int   madvise(void *addr, size_t length, int advice) __TH_NOTHROW;
int   mlock(const void *addr, size_t length) __TH_NOTHROW;
int   munlock(const void *addr, size_t length) __TH_NOTHROW;
__TH_END_DECLS

#endif
