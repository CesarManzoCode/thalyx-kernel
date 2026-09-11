/* The rest of the system interface a POSIX program reaches for.
 *
 * Each answer comes from the kernel or says there is none. `sysconf` reads the
 * processors the kernel brought up and the page size it runs on; sleeping is a
 * real wait on a signal; `getpid` is the kernel's own identifier for the
 * domain, because a domain is what a process is here. Signals, a shell and a
 * source of entropy do not exist, and asking for one is refused.
 *
 * `syscall` answers exactly one number: the futex operations the prebuilt C++
 * library issues directly when two threads race to initialise the same static
 * object. A futex wait parks the caller on its wake bit until a wake for the
 * same address raises it, or the word changes, or its deadline passes.
 */

#include "thalyx/nrt.h"
#include <unistd.h>
#include <errno.h>
#include <signal.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sched.h>
#include <sys/resource.h>

long sysconf(int name)
{
    thalyx_limits_t limits;
    switch (name) {
    case _SC_NPROCESSORS_ONLN:
    case _SC_NPROCESSORS_CONF:
        if (th_limits(&limits) != THALYX_STATUS_OK) { errno = EINVAL; return -1; }
        return (long)limits.cpus_online;
    case _SC_PAGESIZE:
        if (th_limits(&limits) != THALYX_STATUS_OK) { errno = EINVAL; return -1; }
        return (long)limits.page_size;
    case _SC_CLK_TCK:
        return 100;
    case _SC_OPEN_MAX:
        return 11;
    case _SC_PHYS_PAGES:
        /* The most this program may hold: its heap ceiling. */
        return (long)th_boot()->heap_pages;
    default:
        errno = EINVAL;
        return -1;
    }
}

int get_nprocs(void) { return (int)sysconf(_SC_NPROCESSORS_ONLN); }
int get_nprocs_conf(void) { return (int)sysconf(_SC_NPROCESSORS_CONF); }

int usleep(useconds_t microseconds)
{
    th_yield_ns((uint64_t)microseconds * 1000ull);
    return 0;
}

unsigned sleep(unsigned seconds)
{
    th_yield_ns((uint64_t)seconds * 1000000000ull);
    return 0;
}

int nanosleep(const struct timespec *wanted, struct timespec *left)
{
    if (wanted == NULL || wanted->tv_nsec < 0 || wanted->tv_nsec >= 1000000000L) {
        errno = EINVAL;
        return -1;
    }
    th_yield_ns((uint64_t)wanted->tv_sec * 1000000000ull + (uint64_t)wanted->tv_nsec);
    if (left) { left->tv_sec = 0; left->tv_nsec = 0; }
    return 0;
}

pid_t getpid(void)
{
    static pid_t cached;
    if (cached) { return cached; }
    th_desc d;
    th_desc_begin(&d, THALYX_OP_DOMAIN_QUERY);
    th_result r = th_op(thalyx_boot_handle_of(TH_SLOT_SELF_DOMAIN), THALYX_OP_DOMAIN_QUERY, &d, 0);
    if (r.status == THALYX_STATUS_OK) {
        thalyx_domain_info_t info;
        memcpy(&info, d.bytes + TH_BODY, sizeof(info));
        cached = (pid_t)info.domain_id;
    }
    if (!cached) { cached = 1; }
    return cached;
}

int getpriority(int which, id_t who)
{
    (void)which;
    (void)who;
    errno = 0;
    return 0;
}

int setpriority(int which, id_t who, int priority)
{
    (void)which;
    (void)who;
    if (priority == 0) { return 0; }
    errno = EPERM;
    return -1;
}

int getrlimit(int resource, struct rlimit *out)
{
    (void)resource;
    (void)out;
    errno = ENOSYS;
    return -1;
}

int system(const char *command)
{
    if (command == NULL) { return 0; }   /* no command processor exists */
    errno = ENOSYS;
    return -1;
}

sighandler_t signal(int sig, sighandler_t handler)
{
    (void)sig;
    (void)handler;
    errno = ENOSYS;
    return SIG_ERR;
}

int raise(int sig)
{
    if (sig == SIGABRT) { abort(); }
    errno = ENOSYS;
    return -1;
}

int getentropy(void *into, size_t n)
{
    (void)into;
    (void)n;
    errno = ENOSYS;
    return -1;
}

/* This system has no entropy source a program can reach, and `arc4random`
 * has no way to say it failed. Answering with something predictable would be
 * the one wrong answer that looks right, so a program that asks stops. The
 * prebuilt C++ library reaches it only through `std::random_device`, which
 * nothing on the engine's path constructs with a default seed. */
/* The runtime's own range, 0x5001 to 0x500F, is full; its closure notes that
 * do not fit there start at 0x5700, clear of the supervisor's 0x5010 onward. */
#define TH_NOTE_NO_ENTROPY 0x5700ull

uint32_t arc4random(void)
{
    th_note(TH_NOTE_NO_ENTROPY, (uint64_t)(uintptr_t)__builtin_return_address(0));
    abort();
}

/* No environment exists, so the secure answer and the ordinary one agree. */
char *secure_getenv(const char *name)
{
    (void)name;
    return NULL;
}

/* ---------------------------------------------------------------- futex */

#define SYS_FUTEX            202
#define FUTEX_WAIT             0
#define FUTEX_WAKE             1
#define FUTEX_WAIT_BITSET      9
#define FUTEX_WAKE_BITSET     10
#define FUTEX_CMD_MASK       127
#define FUTEX_CLOCK_REALTIME 256

#define TH_FUTEX_WAITERS 16

static struct {
    int *address;
    uint32_t mask;
} futex_waiters[TH_FUTEX_WAITERS];
static int futex_lock;

static void futex_enter(void)
{
    while (__atomic_exchange_n(&futex_lock, 1, __ATOMIC_ACQUIRE)) { __builtin_ia32_pause(); }
}

static void futex_leave(void) { __atomic_store_n(&futex_lock, 0, __ATOMIC_RELEASE); }

static int futex_register(int *address, uint32_t bit)
{
    futex_enter();
    for (unsigned i = 0; i < TH_FUTEX_WAITERS; i++) {
        if (futex_waiters[i].address == address || futex_waiters[i].address == NULL) {
            futex_waiters[i].address = address;
            futex_waiters[i].mask |= bit;
            futex_leave();
            return 0;
        }
    }
    futex_leave();
    return -1;
}

static void futex_unregister(int *address, uint32_t bit)
{
    futex_enter();
    for (unsigned i = 0; i < TH_FUTEX_WAITERS; i++) {
        if (futex_waiters[i].address == address) {
            futex_waiters[i].mask &= ~bit;
            if (futex_waiters[i].mask == 0) { futex_waiters[i].address = NULL; }
        }
    }
    futex_leave();
}

static long futex_wake(int *address, int count)
{
    uint32_t mask = 0;
    futex_enter();
    for (unsigned i = 0; i < TH_FUTEX_WAITERS; i++) {
        if (futex_waiters[i].address == address) { mask |= futex_waiters[i].mask; }
    }
    futex_leave();
    long woken = 0;
    while (mask && woken < count) {
        unsigned index = (unsigned)__builtin_ctz(mask);
        mask &= mask - 1;
        th_wake(index);
        woken++;
    }
    return woken;
}

static long futex_wait(int *address, int expected, const struct timespec *timeout, int absolute,
                       int realtime)
{
    uint64_t deadline = 0;
    if (timeout) {
        uint64_t span = (uint64_t)timeout->tv_sec * 1000000000ull + (uint64_t)timeout->tv_nsec;
        if (!absolute) {
            deadline = th_now_ns() + span;
        } else if (realtime) {
            struct timespec now;
            clock_gettime(CLOCK_REALTIME, &now);
            uint64_t wall = (uint64_t)now.tv_sec * 1000000000ull + (uint64_t)now.tv_nsec;
            deadline = th_now_ns() + (span > wall ? span - wall : 0);
        } else {
            deadline = span;
        }
    }
    uint32_t bit = 1u << th_thread_index();
    if (futex_register(address, bit) != 0) { errno = ENOMEM; return -1; }
    long result = 0;
    while (__atomic_load_n(address, __ATOMIC_SEQ_CST) == expected) {
        uint64_t now = th_now_ns();
        if (deadline && now >= deadline) { errno = ETIMEDOUT; result = -1; break; }
        uint64_t until = now + 5000000ull;
        if (deadline && deadline < until) { until = deadline; }
        th_wait_wake(until);
    }
    futex_unregister(address, bit);
    if (result == 0 && __atomic_load_n(address, __ATOMIC_SEQ_CST) == expected) {
        errno = EAGAIN;
        return -1;
    }
    return result;
}

long syscall(long number, ...)
{
    va_list args;
    va_start(args, number);
    if (number != SYS_FUTEX) {
        va_end(args);
        errno = ENOSYS;
        return -1;
    }
    int *address = va_arg(args, int *);
    int operation = va_arg(args, int);
    int value = va_arg(args, int);
    const struct timespec *timeout = va_arg(args, const struct timespec *);
    va_end(args);

    int command = operation & FUTEX_CMD_MASK;
    if (command == FUTEX_WAIT || command == FUTEX_WAIT_BITSET) {
        if (__atomic_load_n(address, __ATOMIC_SEQ_CST) != value) {
            errno = EAGAIN;
            return -1;
        }
        return futex_wait(address, value, timeout, command == FUTEX_WAIT_BITSET,
                          (operation & FUTEX_CLOCK_REALTIME) != 0);
    }
    if (command == FUTEX_WAKE || command == FUTEX_WAKE_BITSET) {
        return futex_wake(address, value);
    }
    errno = ENOSYS;
    return -1;
}
