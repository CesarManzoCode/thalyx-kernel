/* Threads, as the language runtime asks for them, and what this system answers.
 *
 * QuickJS includes `pthread.h` on every platform that is not Windows and uses
 * it for four things: a once-guard, a mutex, a condition variable, and starting
 * a worker thread. Only the first three are reachable from an embedding that
 * does not use `Atomics` or workers, and a work domain of this port is one
 * thread by construction -- its threads are built by its supervisor before it
 * is activated, and it never asks for another.
 *
 * So this is not an emulation of pthreads and does not pretend to be one. The
 * mutex and the condition variable are the correct implementations *for one
 * thread*: a lock nobody contends is a counter, and a wait nobody can be woken
 * from is a mistake, which is why `pthread_cond_wait` refuses rather than
 * returning as if it had waited. `pthread_create` refuses too: a program that
 * needed a second thread here would get an error at the point it asked for one
 * instead of a thread that silently never ran.
 *
 * If a workload ever needs real worker threads inside a language runtime, the
 * runtime beneath this header already has them (`thalyx/nrt.h`), and this file
 * is where they would be joined up. Nothing here should be read as a claim that
 * they are.
 */
#ifndef _PTHREAD_H
#define _PTHREAD_H

#include <stddef.h>
#include <errno.h>
#include <time.h>

typedef struct { int done; } pthread_once_t;
#define PTHREAD_ONCE_INIT { 0 }

typedef struct { int held; } pthread_mutex_t;
#define PTHREAD_MUTEX_INITIALIZER { 0 }

typedef struct { int unused; } pthread_cond_t;
typedef struct { int detached; size_t stack; } pthread_attr_t;
typedef struct { int clock; } pthread_condattr_t;
typedef unsigned long pthread_t;

#define PTHREAD_CREATE_DETACHED 1

static inline int pthread_once(pthread_once_t *guard, void (*callback)(void))
{
    if (!guard->done) { guard->done = 1; callback(); }
    return 0;
}

static inline int pthread_mutex_init(pthread_mutex_t *mutex, void *attributes)
{
    (void)attributes;
    mutex->held = 0;
    return 0;
}
static inline int pthread_mutex_destroy(pthread_mutex_t *mutex) { (void)mutex; return 0; }
static inline int pthread_mutex_lock(pthread_mutex_t *mutex) { mutex->held++; return 0; }
static inline int pthread_mutex_unlock(pthread_mutex_t *mutex) { mutex->held--; return 0; }

static inline int pthread_cond_init(pthread_cond_t *cond, void *attributes)
{
    (void)attributes;
    cond->unused = 0;
    return 0;
}
static inline int pthread_cond_destroy(pthread_cond_t *cond) { (void)cond; return 0; }
static inline int pthread_cond_signal(pthread_cond_t *cond) { (void)cond; return 0; }
static inline int pthread_cond_broadcast(pthread_cond_t *cond) { (void)cond; return 0; }

/* A wait with nobody who could signal it is a deadlock, and answering as if it
 * had waited would hide one. */
static inline int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex)
{
    (void)cond; (void)mutex;
    return ENOSYS;
}
static inline int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex,
                                         const struct timespec *until)
{
    (void)cond; (void)mutex; (void)until;
    return ENOSYS;
}

static inline int pthread_condattr_init(pthread_condattr_t *attributes)
{
    attributes->clock = 0;
    return 0;
}
static inline int pthread_condattr_destroy(pthread_condattr_t *attributes)
{
    (void)attributes;
    return 0;
}
static inline int pthread_condattr_setclock(pthread_condattr_t *attributes, int clock)
{
    attributes->clock = clock;
    return 0;
}

static inline int pthread_attr_init(pthread_attr_t *attributes)
{
    attributes->detached = 0;
    attributes->stack = 0;
    return 0;
}
static inline int pthread_attr_destroy(pthread_attr_t *attributes) { (void)attributes; return 0; }
static inline int pthread_attr_setdetachstate(pthread_attr_t *attributes, int state)
{
    attributes->detached = state;
    return 0;
}
static inline int pthread_attr_setstacksize(pthread_attr_t *attributes, size_t bytes)
{
    attributes->stack = bytes;
    return 0;
}

/* Refused, and said so. A domain's threads are built before it is activated. */
static inline int pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                                 void *(*start)(void *), void *argument)
{
    (void)thread; (void)attributes; (void)start; (void)argument;
    return ENOSYS;
}
static inline int pthread_join(pthread_t thread, void **result)
{
    (void)thread; (void)result;
    return ENOSYS;
}

#endif
