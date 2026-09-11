/* Threads, on a kernel where a domain's threads are built and not spawned.
 *
 * `DOMAIN_ADD_THREAD` answers only for a domain that is still being built, so
 * no running program creates a thread here. Its supervisor builds them before
 * activation, parked on a signal, and `pthread_create` hands one of those its
 * work. When every built thread is busy, or none was built, `pthread_create`
 * answers `EAGAIN` at the point it was asked -- not a thread that silently
 * never runs.
 *
 * Mutexes and condition variables block on a real `SIGNAL_WAIT`, one bit per
 * thread, so a waiting thread spends nothing of its scope's budget while it
 * waits. A wait on a bit that was raised before the waiter got there returns
 * at once, because signal bits latch: a wake-up cannot be lost.
 *
 * Every type has glibc's x86-64 size. The prebuilt C++ standard library was
 * compiled against those sizes and holds these objects inside its own, so a
 * smaller `pthread_mutex_t` here would be a `std::mutex` two libraries
 * disagree about the size of. All-zero is a valid unlocked mutex, condition,
 * lock and once-guard, which is what glibc's static initialisers are, and a
 * recursive mutex keeps its kind at the offset glibc's initialiser writes it.
 */
#ifndef _PTHREAD_H
#define _PTHREAD_H

#include <stddef.h>
#include <time.h>
#include <sched.h>
#include "thalyx/cdefs.h"

typedef unsigned long pthread_t;
typedef union { char __size[56]; long __align; } pthread_attr_t;
typedef union { char __size[40]; long __align; } pthread_mutex_t;
typedef union { char __size[4]; int __align; } pthread_mutexattr_t;
typedef union { char __size[48]; long long __align; } pthread_cond_t;
typedef union { char __size[4]; int __align; } pthread_condattr_t;
typedef unsigned int pthread_key_t;
typedef int pthread_once_t;
typedef union { char __size[56]; long __align; } pthread_rwlock_t;
typedef union { char __size[8]; long __align; } pthread_rwlockattr_t;
typedef volatile int pthread_spinlock_t;

#define PTHREAD_MUTEX_INITIALIZER  { { 0 } }
#define PTHREAD_COND_INITIALIZER   { { 0 } }
#define PTHREAD_RWLOCK_INITIALIZER { { 0 } }
#define PTHREAD_ONCE_INIT 0
#define PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP \
    { { 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1 } }

enum {
    PTHREAD_MUTEX_TIMED_NP = 0,
    PTHREAD_MUTEX_RECURSIVE_NP = 1,
    PTHREAD_MUTEX_ERRORCHECK_NP = 2,
    PTHREAD_MUTEX_ADAPTIVE_NP = 3,
    PTHREAD_MUTEX_NORMAL = PTHREAD_MUTEX_TIMED_NP,
    PTHREAD_MUTEX_RECURSIVE = PTHREAD_MUTEX_RECURSIVE_NP,
    PTHREAD_MUTEX_ERRORCHECK = PTHREAD_MUTEX_ERRORCHECK_NP,
    PTHREAD_MUTEX_DEFAULT = PTHREAD_MUTEX_NORMAL
};

#define PTHREAD_CREATE_JOINABLE 0
#define PTHREAD_CREATE_DETACHED 1
#define PTHREAD_CANCEL_ENABLE   0
#define PTHREAD_CANCEL_DISABLE  1

__TH_BEGIN_DECLS

int  pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                    void *(*start)(void *), void *argument) __TH_NOTHROW;
int  pthread_join(pthread_t thread, void **result);
int  pthread_detach(pthread_t thread) __TH_NOTHROW;
void pthread_exit(void *result) __TH_NORETURN;
int  pthread_cancel(pthread_t thread);
pthread_t pthread_self(void) __TH_NOTHROW __attribute__((__const__));
int  pthread_equal(pthread_t a, pthread_t b) __TH_NOTHROW;
int  pthread_once(pthread_once_t *guard, void (*callback)(void));
int  pthread_setcancelstate(int state, int *old);
int  pthread_getschedparam(pthread_t thread, int *policy, struct sched_param *param) __TH_NOTHROW;
int  pthread_setschedparam(pthread_t thread, int policy, const struct sched_param *param) __TH_NOTHROW;

int pthread_attr_init(pthread_attr_t *attributes) __TH_NOTHROW;
int pthread_attr_destroy(pthread_attr_t *attributes) __TH_NOTHROW;
int pthread_attr_setdetachstate(pthread_attr_t *attributes, int state) __TH_NOTHROW;
int pthread_attr_getdetachstate(const pthread_attr_t *attributes, int *state) __TH_NOTHROW;
int pthread_attr_setstacksize(pthread_attr_t *attributes, size_t bytes) __TH_NOTHROW;
int pthread_attr_getstacksize(const pthread_attr_t *attributes, size_t *bytes) __TH_NOTHROW;

int pthread_mutexattr_init(pthread_mutexattr_t *attributes) __TH_NOTHROW;
int pthread_mutexattr_destroy(pthread_mutexattr_t *attributes) __TH_NOTHROW;
int pthread_mutexattr_settype(pthread_mutexattr_t *attributes, int kind) __TH_NOTHROW;
int pthread_mutexattr_gettype(const pthread_mutexattr_t *attributes, int *kind) __TH_NOTHROW;

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attributes) __TH_NOTHROW;
int pthread_mutex_destroy(pthread_mutex_t *mutex) __TH_NOTHROW;
int pthread_mutex_lock(pthread_mutex_t *mutex) __TH_NOTHROW;
int pthread_mutex_trylock(pthread_mutex_t *mutex) __TH_NOTHROW;
int pthread_mutex_unlock(pthread_mutex_t *mutex) __TH_NOTHROW;
int pthread_mutex_timedlock(pthread_mutex_t *mutex, const struct timespec *until) __TH_NOTHROW;
int pthread_mutex_clocklock(pthread_mutex_t *mutex, clockid_t clock,
                            const struct timespec *until) __TH_NOTHROW;

int pthread_condattr_init(pthread_condattr_t *attributes) __TH_NOTHROW;
int pthread_condattr_destroy(pthread_condattr_t *attributes) __TH_NOTHROW;
int pthread_condattr_setclock(pthread_condattr_t *attributes, clockid_t clock) __TH_NOTHROW;

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attributes) __TH_NOTHROW;
int pthread_cond_destroy(pthread_cond_t *cond) __TH_NOTHROW;
int pthread_cond_signal(pthread_cond_t *cond) __TH_NOTHROW;
int pthread_cond_broadcast(pthread_cond_t *cond) __TH_NOTHROW;
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex);
int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex,
                           const struct timespec *until);
int pthread_cond_clockwait(pthread_cond_t *cond, pthread_mutex_t *mutex, clockid_t clock,
                           const struct timespec *until);

int pthread_rwlock_init(pthread_rwlock_t *lock, const pthread_rwlockattr_t *attributes) __TH_NOTHROW;
int pthread_rwlock_destroy(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_rdlock(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_tryrdlock(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_wrlock(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_trywrlock(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_unlock(pthread_rwlock_t *lock) __TH_NOTHROW;
int pthread_rwlock_timedrdlock(pthread_rwlock_t *lock, const struct timespec *until) __TH_NOTHROW;
int pthread_rwlock_timedwrlock(pthread_rwlock_t *lock, const struct timespec *until) __TH_NOTHROW;
int pthread_rwlock_clockrdlock(pthread_rwlock_t *lock, clockid_t clock,
                               const struct timespec *until) __TH_NOTHROW;
int pthread_rwlock_clockwrlock(pthread_rwlock_t *lock, clockid_t clock,
                               const struct timespec *until) __TH_NOTHROW;

int   pthread_key_create(pthread_key_t *key, void (*destructor)(void *)) __TH_NOTHROW;
int   pthread_key_delete(pthread_key_t key) __TH_NOTHROW;
void *pthread_getspecific(pthread_key_t key) __TH_NOTHROW;
int   pthread_setspecific(pthread_key_t key, const void *value) __TH_NOTHROW;

__TH_END_DECLS

#endif
