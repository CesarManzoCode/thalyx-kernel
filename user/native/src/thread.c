/* Threads, on a kernel where a domain's threads are built and not spawned.
 *
 * `DOMAIN_ADD_THREAD` only answers for a domain that is still `Building`, which
 * is the kernel's construction discipline and not an omission: a running domain
 * that could add threads to itself would be a second, unaccounted path into the
 * same scope's parallelism budget. So a native program does not create threads.
 * Its supervisor creates them before activation, all entering the same
 * trampoline with an index, and this file is what they do next: block on a
 * signal until the program posts work for that index.
 *
 * Blocking is a real `SIGNAL_WAIT`, not a spin. A parked worker that spun would
 * spend its scope's CPU budget doing nothing, and the engine's whole claim is
 * that residency costs what residency costs.
 *
 * A thread finds its index by its stack: the slots are `TH_THREAD_SLOT` bytes,
 * aligned to their own size, and masking the stack pointer gives the index
 * back. It costs one `and`, and it works before thread-local storage exists --
 * which is when it is needed, because installing that storage needs the index.
 * Once installed, each thread also has its own FS base (see `cxxrt.c`).
 *
 * Every thread has one more bit on the work signal: its wake bit, which the
 * locks in `pthread.c` raise to end a wait. Work bits are 1 to 3 and wake bits
 * 16 to 19, so the two never answer each other's waits.
 */

#include "thalyx/nrt.h"
#include <string.h>

#define TH_NOTE_THREAD_UP   0x5007ull
#define TH_NOTE_THREAD_WORK 0x5008ull

/* Bit 63 is raised by nobody, which is what makes a wait on it a sleep. */
#define TH_BIT_NEVER (1ull << 63)

typedef struct {
    th_thread_fn body;
    void *argument;
} Worker;

static Worker workers[TH_THREAD_MAX + 1];

int64_t th_signal_wait(uint32_t slot, uint64_t mask, uint64_t deadline_ns)
{
    thalyx_signal_bits_t bits;
    memset(&bits, 0, sizeof(bits));
    bits.bits = mask;
    th_desc d;
    th_desc_begin(&d, THALYX_OP_SIGNAL_WAIT);
    th_desc_put(&d, TH_BODY, &bits, sizeof(bits));
    th_result r = th_op(thalyx_boot_handle_of(slot), THALYX_OP_SIGNAL_WAIT, &d, deadline_ns);
    return r.status;
}

int64_t th_signal_raise(uint32_t slot, uint64_t mask)
{
    thalyx_signal_bits_t bits;
    memset(&bits, 0, sizeof(bits));
    bits.bits = mask;
    th_desc d;
    th_desc_begin(&d, THALYX_OP_SIGNAL_RAISE);
    th_desc_put(&d, TH_BODY, &bits, sizeof(bits));
    th_result r = th_op(thalyx_boot_handle_of(slot), THALYX_OP_SIGNAL_RAISE, &d, 0);
    return r.status;
}

unsigned th_thread_index(void)
{
    uintptr_t here = (uintptr_t)__builtin_frame_address(0);
    if (here < TH_THREAD_STACKS || here >= TH_THREAD_STACKS + (TH_THREAD_MAX + 1) * TH_THREAD_SLOT) {
        return 0;                                    /* the initial thread */
    }
    return (unsigned)((here - TH_THREAD_STACKS) / TH_THREAD_SLOT) + 1;
}

void th_yield_ns(uint64_t nanoseconds)
{
    th_signal_wait(TH_SLOT_SIGNAL_DONE, TH_BIT_NEVER, th_now_ns() + nanoseconds);
}

#define TH_WAKE_BIT(index) (1ull << (16 + (index)))
/* Raised once by the initial thread when the runtime is standing. A built
 * thread starts running the moment its domain is activated, which is before
 * `_start` has zeroed `.bss`; it waits for this bit before it touches any of
 * the runtime's state, so nothing it writes is erased under it. */
#define TH_START_BIT(index) (1ull << (24 + (index)))

void th_threads_release(void)
{
    uint64_t bits = 0;
    for (unsigned index = 1; index <= TH_THREAD_MAX; index++) { bits |= TH_START_BIT(index); }
    th_signal_raise(TH_SLOT_SIGNAL_WORK, bits);
}

void th_wake(unsigned index)
{
    if (index <= TH_THREAD_MAX) { th_signal_raise(TH_SLOT_SIGNAL_WORK, TH_WAKE_BIT(index)); }
}

int th_wait_wake(uint64_t deadline_ns)
{
    return (int)th_signal_wait(TH_SLOT_SIGNAL_WORK, TH_WAKE_BIT(th_thread_index()), deadline_ns);
}

/* One bit per built thread that has started and is parked waiting for work.
 * `pthread_create` hands work only to a thread that said it is there. */
volatile uint32_t th_threads_alive;

/* The body a built thread runs. Its index comes from the argument the
 * supervisor gave `DOMAIN_ADD_THREAD`, which is the same discipline the K3
 * workers use: what a thread is, is not the program's choice. */
void th_thread_body(uint64_t index)
{
    if (index == 0 || index > TH_THREAD_MAX) {
        for (;;) { th_yield_ns(1000000000ull); }
    }
    /* Nothing but this wait touches memory the runtime owns until the initial
     * thread has zeroed `.bss` and says so; the wait itself uses only this
     * stack and a capability installed before activation. */
    while (th_signal_wait(TH_SLOT_SIGNAL_WORK, TH_START_BIT(index), 0) != THALYX_STATUS_OK) { }
    /* Before anything else runs on this thread: code compiled with the stack
     * protector reads FS on its first instruction. A thread whose storage
     * could not be installed parks rather than run code that would fault. */
    if (th_tls_install((unsigned)index) != 0) {
        for (;;) { th_yield_ns(1000000000ull); }
    }
    th_note(TH_NOTE_THREAD_UP, index);
    __atomic_fetch_or(&th_threads_alive, 1u << index, __ATOMIC_RELEASE);
    for (;;) {
        if (th_signal_wait(TH_SLOT_SIGNAL_WORK, 1ull << index, 0) != THALYX_STATUS_OK) {
            continue;
        }
        th_thread_fn body = __atomic_load_n(&workers[index].body, __ATOMIC_ACQUIRE);
        if (body != NULL) { body(workers[index].argument); }
        th_note(TH_NOTE_THREAD_WORK, index);
        th_signal_raise(TH_SLOT_SIGNAL_DONE, 1ull << index);
    }
}

int th_thread_start(unsigned index, th_thread_fn body, void *argument)
{
    if (index == 0 || index > TH_THREAD_MAX) { return -1; }
    workers[index].argument = argument;
    __atomic_store_n(&workers[index].body, body, __ATOMIC_RELEASE);
    return (int)th_signal_raise(TH_SLOT_SIGNAL_WORK, 1ull << index);
}

int th_thread_join(unsigned index)
{
    if (index == 0 || index > TH_THREAD_MAX) { return -1; }
    return (int)th_signal_wait(TH_SLOT_SIGNAL_DONE, 1ull << index, 0);
}

/* Nothing ends one thread of a domain on this kernel: `EXIT` terminates the
 * whole domain, which is what makes it wrong here. A thread with nothing left
 * to do sleeps until its domain is stopped. */
void th_thread_exit(void)
{
    for (;;) { th_yield_ns(1000000000ull); }
}
