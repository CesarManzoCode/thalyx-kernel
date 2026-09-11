/* Threads, locks and conditions over built threads and signal waits.
 *
 * See `pthread.h` for what this is and is not. The mechanics are these.
 *
 * A thread is found by its stack (`th_thread_index`), and each has one wake
 * bit on the domain's work signal. Anything a thread can block on -- a mutex,
 * a condition, a once-guard in progress -- keeps a mask of the threads waiting
 * on it, and whoever releases it raises the bits of every thread in the mask.
 * A woken thread re-checks what it was waiting for, so a wake that turns out
 * to be for something else costs one more wait and nothing else. Every wait
 * also carries a deadline a few milliseconds away, which bounds the cost of a
 * wake that went missing through a mistake in this file rather than hiding it.
 *
 * `pthread_create` hands a built thread its start routine; built threads
 * announce themselves when they first run, so a creator that arrives before
 * they do waits briefly for one instead of failing.
 */

#include "thalyx/nrt.h"
#include <pthread.h>
#include <errno.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <setjmp.h>

#define TH_NOTE_PTHREAD_REFUSED 0x500Eull

#define WAIT_SLICE_NS 5000000ull   /* the longest a missed wake could cost */

extern volatile uint32_t th_threads_alive;

static uint64_t realtime_offset_ns(void)
{
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    uint64_t realtime = (uint64_t)now.tv_sec * 1000000000ull + (uint64_t)now.tv_nsec;
    return realtime - th_now_ns();
}

/* An absolute time on `clock`, as a monotonic deadline. Zero means none. */
static uint64_t deadline_of(clockid_t clock, const struct timespec *until)
{
    if (until == NULL) { return 0; }
    uint64_t at = (uint64_t)until->tv_sec * 1000000000ull + (uint64_t)until->tv_nsec;
    if (clock == CLOCK_REALTIME) {
        uint64_t offset = realtime_offset_ns();
        at = at > offset ? at - offset : 1;
    }
    return at ? at : 1;
}

static void wait_slice(uint64_t deadline)
{
    uint64_t now = th_now_ns();
    uint64_t until = now + WAIT_SLICE_NS;
    if (deadline && deadline < until) { until = deadline; }
    th_wait_wake(until);
}

static void wake_all(uint32_t mask)
{
    while (mask) {
        unsigned index = (unsigned)__builtin_ctz(mask);
        mask &= mask - 1;
        th_wake(index);
    }
}

/* ---------------------------------------------------------------- mutex */

typedef struct {
    int lock;              /*  0: 0 free, 1 held                           */
    unsigned int count;    /*  4: depth, for a recursive mutex             */
    int owner;             /*  8: index + 1 of the thread that holds it    */
    unsigned int waiters;  /* 12: one bit per waiting thread               */
    int kind;              /* 16: glibc's offset for the kind              */
} th_mutex;

_Static_assert(sizeof(th_mutex) <= sizeof(pthread_mutex_t), "mutex fits");
_Static_assert(__builtin_offsetof(th_mutex, kind) == 16, "kind where glibc keeps it");

static int mutex_acquire(pthread_mutex_t *handle, uint64_t deadline, int only_try)
{
    th_mutex *m = (th_mutex *)handle;
    int self = (int)th_thread_index() + 1;
    if (__atomic_load_n(&m->owner, __ATOMIC_RELAXED) == self) {
        if (m->kind == PTHREAD_MUTEX_RECURSIVE) { m->count++; return 0; }
        if (m->kind == PTHREAD_MUTEX_ERRORCHECK) { return EDEADLK; }
    }
    uint32_t bit = 1u << (self - 1);
    for (unsigned spin = 0;; spin++) {
        int expected = 0;
        if (__atomic_compare_exchange_n(&m->lock, &expected, 1, 0, __ATOMIC_SEQ_CST,
                                        __ATOMIC_RELAXED)) {
            __atomic_store_n(&m->owner, self, __ATOMIC_RELAXED);
            m->count = 1;
            return 0;
        }
        if (only_try) { return EBUSY; }
        if (spin < 64) { __builtin_ia32_pause(); continue; }
        if (deadline && th_now_ns() >= deadline) { return ETIMEDOUT; }
        __atomic_fetch_or(&m->waiters, bit, __ATOMIC_SEQ_CST);
        expected = 0;
        if (__atomic_compare_exchange_n(&m->lock, &expected, 1, 0, __ATOMIC_SEQ_CST,
                                        __ATOMIC_RELAXED)) {
            __atomic_fetch_and(&m->waiters, ~bit, __ATOMIC_SEQ_CST);
            __atomic_store_n(&m->owner, self, __ATOMIC_RELAXED);
            m->count = 1;
            return 0;
        }
        wait_slice(deadline);
        __atomic_fetch_and(&m->waiters, ~bit, __ATOMIC_SEQ_CST);
    }
}

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attributes)
{
    memset(mutex, 0, sizeof(*mutex));
    if (attributes) { ((th_mutex *)mutex)->kind = attributes->__align; }
    return 0;
}

int pthread_mutex_destroy(pthread_mutex_t *mutex)
{
    return ((th_mutex *)mutex)->lock ? EBUSY : 0;
}

int pthread_mutex_lock(pthread_mutex_t *mutex) { return mutex_acquire(mutex, 0, 0); }
int pthread_mutex_trylock(pthread_mutex_t *mutex) { return mutex_acquire(mutex, 0, 1); }

int pthread_mutex_timedlock(pthread_mutex_t *mutex, const struct timespec *until)
{
    return mutex_acquire(mutex, deadline_of(CLOCK_REALTIME, until), 0);
}

int pthread_mutex_clocklock(pthread_mutex_t *mutex, clockid_t clock, const struct timespec *until)
{
    return mutex_acquire(mutex, deadline_of(clock, until), 0);
}

int pthread_mutex_unlock(pthread_mutex_t *handle)
{
    th_mutex *m = (th_mutex *)handle;
    int self = (int)th_thread_index() + 1;
    if (m->kind == PTHREAD_MUTEX_ERRORCHECK && m->owner != self) { return EPERM; }
    if (m->kind == PTHREAD_MUTEX_RECURSIVE && m->count > 1) {
        m->count--;
        return 0;
    }
    m->count = 0;
    __atomic_store_n(&m->owner, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&m->lock, 0, __ATOMIC_SEQ_CST);
    wake_all(__atomic_load_n(&m->waiters, __ATOMIC_SEQ_CST));
    return 0;
}

int pthread_mutexattr_init(pthread_mutexattr_t *attributes)
{
    attributes->__align = PTHREAD_MUTEX_NORMAL;
    return 0;
}
int pthread_mutexattr_destroy(pthread_mutexattr_t *attributes) { (void)attributes; return 0; }
int pthread_mutexattr_settype(pthread_mutexattr_t *attributes, int kind)
{
    if (kind < PTHREAD_MUTEX_NORMAL || kind > PTHREAD_MUTEX_ADAPTIVE_NP) { return EINVAL; }
    attributes->__align = kind == PTHREAD_MUTEX_ADAPTIVE_NP ? PTHREAD_MUTEX_NORMAL : kind;
    return 0;
}
int pthread_mutexattr_gettype(const pthread_mutexattr_t *attributes, int *kind)
{
    *kind = attributes->__align;
    return 0;
}

/* ------------------------------------------------------------ condition */

typedef struct {
    unsigned int sequence; /* 0: advanced by every signal */
    int clock;             /* 4: what an absolute wait is measured on */
    unsigned int waiters;  /* 8 */
} th_cond;

_Static_assert(sizeof(th_cond) <= sizeof(pthread_cond_t), "condition fits");

static int cond_wait(pthread_cond_t *handle, pthread_mutex_t *mutex, uint64_t deadline)
{
    th_cond *c = (th_cond *)handle;
    uint32_t bit = 1u << th_thread_index();
    unsigned seen = __atomic_load_n(&c->sequence, __ATOMIC_SEQ_CST);
    __atomic_fetch_or(&c->waiters, bit, __ATOMIC_SEQ_CST);
    pthread_mutex_unlock(mutex);
    int timed_out = 0;
    while (__atomic_load_n(&c->sequence, __ATOMIC_SEQ_CST) == seen) {
        if (deadline && th_now_ns() >= deadline) { timed_out = 1; break; }
        wait_slice(deadline);
    }
    __atomic_fetch_and(&c->waiters, ~bit, __ATOMIC_SEQ_CST);
    pthread_mutex_lock(mutex);
    return timed_out ? ETIMEDOUT : 0;
}

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attributes)
{
    memset(cond, 0, sizeof(*cond));
    if (attributes) { ((th_cond *)cond)->clock = attributes->__align; }
    return 0;
}
int pthread_cond_destroy(pthread_cond_t *cond) { (void)cond; return 0; }

/* A signal wakes every waiter, which POSIX allows: each re-checks its own
 * predicate, and with at most four threads the difference is nothing. */
int pthread_cond_signal(pthread_cond_t *handle)
{
    th_cond *c = (th_cond *)handle;
    __atomic_fetch_add(&c->sequence, 1, __ATOMIC_SEQ_CST);
    wake_all(__atomic_load_n(&c->waiters, __ATOMIC_SEQ_CST));
    return 0;
}
int pthread_cond_broadcast(pthread_cond_t *handle) { return pthread_cond_signal(handle); }

int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex)
{
    return cond_wait(cond, mutex, 0);
}

int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex,
                           const struct timespec *until)
{
    return cond_wait(cond, mutex, deadline_of(((th_cond *)cond)->clock, until));
}

int pthread_cond_clockwait(pthread_cond_t *cond, pthread_mutex_t *mutex, clockid_t clock,
                           const struct timespec *until)
{
    return cond_wait(cond, mutex, deadline_of(clock, until));
}

int pthread_condattr_init(pthread_condattr_t *attributes)
{
    attributes->__align = CLOCK_REALTIME;
    return 0;
}
int pthread_condattr_destroy(pthread_condattr_t *attributes) { (void)attributes; return 0; }
int pthread_condattr_setclock(pthread_condattr_t *attributes, clockid_t clock)
{
    if (clock != CLOCK_REALTIME && clock != CLOCK_MONOTONIC) { return EINVAL; }
    attributes->__align = clock;
    return 0;
}

/* ------------------------------------------------------ reader and writer */

/* A mutex and a condition, kept inside the lock's own bytes: `readers` is
 * the number holding it shared, `writer` whether one holds it alone. */
typedef struct {
    pthread_mutex_t guard;  /*  0: 40 bytes */
    int readers;            /* 40 */
    int writer;             /* 44 */
    unsigned int sequence;  /* 48 */
    unsigned int waiters;   /* 52 */
} th_rwlock;

_Static_assert(sizeof(th_rwlock) <= sizeof(pthread_rwlock_t), "rwlock fits");

static int rw_acquire(pthread_rwlock_t *handle, int exclusive, uint64_t deadline, int only_try)
{
    th_rwlock *l = (th_rwlock *)handle;
    uint32_t bit = 1u << th_thread_index();
    for (;;) {
        pthread_mutex_lock(&l->guard);
        int free_now = exclusive ? (l->writer == 0 && l->readers == 0) : l->writer == 0;
        if (free_now) {
            if (exclusive) { l->writer = 1; } else { l->readers++; }
            pthread_mutex_unlock(&l->guard);
            return 0;
        }
        unsigned seen = l->sequence;
        __atomic_fetch_or(&l->waiters, bit, __ATOMIC_SEQ_CST);
        pthread_mutex_unlock(&l->guard);
        if (only_try) {
            __atomic_fetch_and(&l->waiters, ~bit, __ATOMIC_SEQ_CST);
            return EBUSY;
        }
        while (__atomic_load_n(&l->sequence, __ATOMIC_SEQ_CST) == seen) {
            if (deadline && th_now_ns() >= deadline) {
                __atomic_fetch_and(&l->waiters, ~bit, __ATOMIC_SEQ_CST);
                return ETIMEDOUT;
            }
            wait_slice(deadline);
        }
        __atomic_fetch_and(&l->waiters, ~bit, __ATOMIC_SEQ_CST);
    }
}

int pthread_rwlock_init(pthread_rwlock_t *lock, const pthread_rwlockattr_t *attributes)
{
    (void)attributes;
    memset(lock, 0, sizeof(*lock));
    return 0;
}
int pthread_rwlock_destroy(pthread_rwlock_t *lock) { (void)lock; return 0; }
int pthread_rwlock_rdlock(pthread_rwlock_t *lock) { return rw_acquire(lock, 0, 0, 0); }
int pthread_rwlock_tryrdlock(pthread_rwlock_t *lock) { return rw_acquire(lock, 0, 0, 1); }
int pthread_rwlock_wrlock(pthread_rwlock_t *lock) { return rw_acquire(lock, 1, 0, 0); }
int pthread_rwlock_trywrlock(pthread_rwlock_t *lock) { return rw_acquire(lock, 1, 0, 1); }
int pthread_rwlock_timedrdlock(pthread_rwlock_t *lock, const struct timespec *until)
{
    return rw_acquire(lock, 0, deadline_of(CLOCK_REALTIME, until), 0);
}
int pthread_rwlock_timedwrlock(pthread_rwlock_t *lock, const struct timespec *until)
{
    return rw_acquire(lock, 1, deadline_of(CLOCK_REALTIME, until), 0);
}
int pthread_rwlock_clockrdlock(pthread_rwlock_t *lock, clockid_t clock, const struct timespec *until)
{
    return rw_acquire(lock, 0, deadline_of(clock, until), 0);
}
int pthread_rwlock_clockwrlock(pthread_rwlock_t *lock, clockid_t clock, const struct timespec *until)
{
    return rw_acquire(lock, 1, deadline_of(clock, until), 0);
}

int pthread_rwlock_unlock(pthread_rwlock_t *handle)
{
    th_rwlock *l = (th_rwlock *)handle;
    pthread_mutex_lock(&l->guard);
    if (l->writer) { l->writer = 0; } else if (l->readers > 0) { l->readers--; }
    __atomic_fetch_add(&l->sequence, 1, __ATOMIC_SEQ_CST);
    uint32_t waiting = __atomic_load_n(&l->waiters, __ATOMIC_SEQ_CST);
    pthread_mutex_unlock(&l->guard);
    wake_all(waiting);
    return 0;
}

/* ----------------------------------------------------------------- once */

int pthread_once(pthread_once_t *guard, void (*callback)(void))
{
    int state = __atomic_load_n(guard, __ATOMIC_ACQUIRE);
    if (state == 2) { return 0; }
    int expected = 0;
    if (__atomic_compare_exchange_n(guard, &expected, 1, 0, __ATOMIC_ACQ_REL,
                                    __ATOMIC_ACQUIRE)) {
        callback();
        __atomic_store_n(guard, 2, __ATOMIC_RELEASE);
        return 0;
    }
    while (__atomic_load_n(guard, __ATOMIC_ACQUIRE) != 2) { th_yield_ns(100000); }
    return 0;
}

/* ----------------------------------------------------------------- keys */

#define TH_KEYS 64

static unsigned char key_used[TH_KEYS];
static void (*key_destructor[TH_KEYS])(void *);
static const void *key_value[TH_THREAD_MAX + 1][TH_KEYS];

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *))
{
    for (unsigned i = 0; i < TH_KEYS; i++) {
        unsigned char expected = 0;
        if (__atomic_compare_exchange_n(&key_used[i], &expected, 1, 0, __ATOMIC_ACQ_REL,
                                        __ATOMIC_RELAXED)) {
            key_destructor[i] = destructor;
            for (unsigned t = 0; t <= TH_THREAD_MAX; t++) { key_value[t][i] = NULL; }
            *key = i;
            return 0;
        }
    }
    return EAGAIN;
}

int pthread_key_delete(pthread_key_t key)
{
    if (key >= TH_KEYS || !key_used[key]) { return EINVAL; }
    __atomic_store_n(&key_used[key], 0, __ATOMIC_RELEASE);
    return 0;
}

void *pthread_getspecific(pthread_key_t key)
{
    if (key >= TH_KEYS) { return NULL; }
    return (void *)key_value[th_thread_index()][key];
}

int pthread_setspecific(pthread_key_t key, const void *value)
{
    if (key >= TH_KEYS || !key_used[key]) { return EINVAL; }
    key_value[th_thread_index()][key] = value;
    return 0;
}

/* -------------------------------------------------------------- threads */

typedef struct {
    int busy;
    int detached;
    void *(*start)(void *);
    void *argument;
    void *result;
    jmp_buf leave;
} th_pthread;

static th_pthread threads[TH_THREAD_MAX + 1];

static void run_start(void *argument)
{
    th_pthread *slot = argument;
    if (setjmp(slot->leave) == 0) {
        slot->result = slot->start(slot->argument);
    }
    if (slot->detached) {
        th_signal_raise(TH_SLOT_SIGNAL_DONE, 0);   /* nothing joins a detached one */
        __atomic_store_n(&slot->busy, 0, __ATOMIC_RELEASE);
    }
}

int pthread_create(pthread_t *thread, const pthread_attr_t *attributes,
                   void *(*start)(void *), void *argument)
{
    int detached = attributes && attributes->__size[0] == PTHREAD_CREATE_DETACHED;
    uint64_t give_up = th_now_ns() + 200000000ull;
    for (;;) {
        uint32_t alive = __atomic_load_n(&th_threads_alive, __ATOMIC_ACQUIRE);
        for (unsigned index = 1; index <= TH_THREAD_MAX; index++) {
            if (!(alive & (1u << index))) { continue; }
            int expected = 0;
            if (!__atomic_compare_exchange_n(&threads[index].busy, &expected, 1, 0,
                                             __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
                continue;
            }
            threads[index].detached = detached;
            threads[index].start = start;
            threads[index].argument = argument;
            threads[index].result = NULL;
            if (th_thread_start(index, run_start, &threads[index]) != THALYX_STATUS_OK) {
                __atomic_store_n(&threads[index].busy, 0, __ATOMIC_RELEASE);
                return EAGAIN;
            }
            *thread = (pthread_t)(index + 1);
            return 0;
        }
        if (th_now_ns() >= give_up) {
            th_note(TH_NOTE_PTHREAD_REFUSED, alive);
            return EAGAIN;
        }
        th_yield_ns(1000000ull);
    }
}

int pthread_join(pthread_t thread, void **result)
{
    if (thread < 2 || thread > TH_THREAD_MAX + 1) { return ESRCH; }
    unsigned index = (unsigned)(thread - 1);
    if (!threads[index].busy || threads[index].detached) { return EINVAL; }
    if (th_thread_join(index) != THALYX_STATUS_OK) { return EINVAL; }
    if (result) { *result = threads[index].result; }
    __atomic_store_n(&threads[index].busy, 0, __ATOMIC_RELEASE);
    return 0;
}

int pthread_detach(pthread_t thread)
{
    if (thread < 2 || thread > TH_THREAD_MAX + 1) { return ESRCH; }
    threads[thread - 1].detached = 1;
    return 0;
}

_Noreturn void pthread_exit(void *result)
{
    unsigned index = th_thread_index();
    if (index == 0) { exit(0); }
    threads[index].result = result;
    longjmp(threads[index].leave, 1);
}

/* Nothing on this kernel stops one thread of a running domain. */
int pthread_cancel(pthread_t thread) { (void)thread; return ENOSYS; }

int pthread_setcancelstate(int state, int *old)
{
    if (old) { *old = PTHREAD_CANCEL_ENABLE; }
    return state == PTHREAD_CANCEL_ENABLE || state == PTHREAD_CANCEL_DISABLE ? 0 : EINVAL;
}

pthread_t pthread_self(void) { return (pthread_t)(th_thread_index() + 1); }
int pthread_equal(pthread_t a, pthread_t b) { return a == b; }

/* Scheduling is the kernel's, by scope budget; there is one policy to report. */
int pthread_getschedparam(pthread_t thread, int *policy, struct sched_param *param)
{
    (void)thread;
    *policy = SCHED_OTHER;
    param->sched_priority = 0;
    return 0;
}

int pthread_setschedparam(pthread_t thread, int policy, const struct sched_param *param)
{
    (void)thread;
    return policy == SCHED_OTHER && param->sched_priority == 0 ? 0 : EPERM;
}

int sched_get_priority_max(int policy) { (void)policy; return 0; }
int sched_get_priority_min(int policy) { (void)policy; return 0; }

int sched_yield(void)
{
    th_yield_ns(1000);
    return 0;
}

/* --------------------------------------------------------- attributes */

int pthread_attr_init(pthread_attr_t *attributes)
{
    memset(attributes, 0, sizeof(*attributes));
    return 0;
}
int pthread_attr_destroy(pthread_attr_t *attributes) { (void)attributes; return 0; }
int pthread_attr_setdetachstate(pthread_attr_t *attributes, int state)
{
    attributes->__size[0] = (char)state;
    return 0;
}
int pthread_attr_getdetachstate(const pthread_attr_t *attributes, int *state)
{
    *state = attributes->__size[0];
    return 0;
}
/* A built thread's stack is the one its supervisor mapped; a size asked for
 * here is recorded and has no effect, and reading it back says what the
 * thread really has. */
int pthread_attr_setstacksize(pthread_attr_t *attributes, size_t bytes)
{
    (void)attributes;
    return bytes < PTHREAD_STACK_MIN ? EINVAL : 0;
}
int pthread_attr_getstacksize(const pthread_attr_t *attributes, size_t *bytes)
{
    (void)attributes;
    *bytes = 16 * 4096;
    return 0;
}
