/* Opening files by descriptor, for the files this system has.
 *
 * Flag values are the x86-64 Linux ones. Only reading is ever granted: a file
 * here is a region a supervisor mapped read-only, and asking to open one for
 * writing is refused with `EROFS` rather than accepted and dropped.
 */
#ifndef _FCNTL_H
#define _FCNTL_H

#include <sys/types.h>
#include "thalyx/cdefs.h"

#define O_RDONLY    00
#define O_WRONLY    01
#define O_RDWR      02
#define O_ACCMODE   03
#define O_CREAT     0100
#define O_EXCL      0200
#define O_NOCTTY    0400
#define O_TRUNC     01000
#define O_APPEND    02000
#define O_NONBLOCK  04000
#define O_DIRECT    040000
#define O_DIRECTORY 0200000
#define O_CLOEXEC   02000000

#define F_GETFD 1
#define F_SETFD 2
#define F_GETFL 3
#define F_SETFL 4
#define FD_CLOEXEC 1

__TH_BEGIN_DECLS
int open(const char *path, int flags, ...);
int fcntl(int fd, int command, ...);
__TH_END_DECLS

#endif
