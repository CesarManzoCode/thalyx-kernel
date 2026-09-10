#ifndef _ERRNO_H
#define _ERRNO_H
extern int th_errno_storage;
#define errno th_errno_storage
#define EPERM 1
#define ENOENT 2
#define EINTR 4
#define EIO 5
#define EBADF 9
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define EINVAL 22
#define ENOSPC 28
#define EPIPE 32
#define ERANGE 34
#define ENOSYS 38
#define EOVERFLOW 75
#define ETIMEDOUT 110
#define ECANCELED 125
#endif
