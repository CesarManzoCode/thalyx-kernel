/* Process priority and resource limits, as a program here can see them.
 *
 * A program does not set its own priority: the kernel schedules by the budget
 * of the scope a thread is charged to, and that scope belongs to whoever built
 * the domain. So the default priority is accepted and anything else is refused
 * with `EPERM`, which is what an unprivileged process asking to raise itself is
 * told elsewhere too. Its real limits are its scope's, and a supervisor reads
 * them; `getrlimit` has nothing truthful to add and refuses.
 */
#ifndef _SYS_RESOURCE_H
#define _SYS_RESOURCE_H

#include <sys/types.h>
#include "thalyx/cdefs.h"

#define PRIO_PROCESS 0
#define PRIO_PGRP    1
#define PRIO_USER    2

/* Resource identifiers. Named because ported code refers to them; `getrlimit`
 * refuses whichever one is asked for, because a program's real limits are its
 * scope's and a supervisor is who reads those. */
#define RLIMIT_CPU        0
#define RLIMIT_FSIZE      1
#define RLIMIT_DATA       2
#define RLIMIT_STACK      3
#define RLIMIT_CORE       4
#define RLIMIT_NOFILE     7
#define RLIMIT_AS         9
#define RLIMIT_MEMLOCK    8
#define RLIM_INFINITY     (~0UL)

typedef unsigned long rlim_t;
struct rlimit {
    rlim_t rlim_cur;
    rlim_t rlim_max;
};

__TH_BEGIN_DECLS
int getpriority(int which, id_t who) __TH_NOTHROW;
int setpriority(int which, id_t who, int priority) __TH_NOTHROW;
int getrlimit(int resource, struct rlimit *out) __TH_NOTHROW;
__TH_END_DECLS

#endif
