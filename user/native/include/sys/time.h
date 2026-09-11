#ifndef _SYS_TIME_H
#define _SYS_TIME_H
#include <time.h>
#include "thalyx/cdefs.h"
struct timeval { time_t tv_sec; suseconds_t tv_usec; };
__TH_BEGIN_DECLS
int gettimeofday(struct timeval *out, void *timezone_unused) __TH_NOTHROW;
__TH_END_DECLS
#endif
