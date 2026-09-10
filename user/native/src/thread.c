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
 * There is no thread-local storage segment either -- setting `FS` needs an
 * instruction the kernel does not enable -- so a thread finds itself by its
 * stack: the slots are `TH_THREAD_SLOT` bytes, aligned to their own size, and
 * masking the stack pointer gives the index back. It costs one `and`.
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

/* The body a built thread runs. Its index comes from the argument the
 * supervisor gave `DOMAIN_ADD_THREAD`, which is the same discipline the K3
 * workers use: what a thread is, is not the program's choice. */
void th_thread_body(uint64_t index)
{
    if (index == 0 || index > TH_THREAD_MAX) {
        for (;;) { th_yield_ns(1000000000ull); }
    }
    th_note(TH_NOTE_THREAD_UP, index);
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
