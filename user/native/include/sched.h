/* Scheduling, as far as a program on this kernel can ask about it.
 *
 * A program does not choose its policy or its priority: the kernel schedules
 * threads by the budgets of the scopes they are charged to. So the policy
 * queries answer one policy with one priority, and `sched_yield` gives the
 * processor up for as short a wait as the kernel can express.
 */
#ifndef _SCHED_H
#define _SCHED_H

#include "thalyx/cdefs.h"

#define SCHED_OTHER 0
#define SCHED_FIFO  1
#define SCHED_RR    2

struct sched_param {
    int sched_priority;
};

__TH_BEGIN_DECLS
int sched_yield(void) __TH_NOTHROW;
int sched_get_priority_max(int policy) __TH_NOTHROW;
int sched_get_priority_min(int policy) __TH_NOTHROW;
__TH_END_DECLS

#endif
