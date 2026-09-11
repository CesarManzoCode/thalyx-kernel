/* The scalar types of the system interface, with glibc's x86-64 widths.
 *
 * Several of these cross into the prebuilt C++ library by value -- file
 * offsets, sizes -- so their widths are the interface's and not a choice.
 */
#ifndef _SYS_TYPES_H
#define _SYS_TYPES_H

#include <stddef.h>
#include <stdint.h>

typedef long ssize_t;
typedef long off_t;
typedef long off64_t;
typedef int pid_t;
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef unsigned int mode_t;
typedef unsigned long dev_t;
typedef unsigned long ino_t;
typedef unsigned long nlink_t;
typedef long blksize_t;
typedef long blkcnt_t;
typedef unsigned int useconds_t;
typedef long suseconds_t;
typedef long time_t;
typedef int clockid_t;
typedef unsigned int id_t;

#endif
