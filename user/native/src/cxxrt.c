/* What the C++ runtime asks of the C one.
 *
 * A C program on this target never needed any of this. The prebuilt C++
 * standard library, GCC's unwinder and every C++ program do, and a real
 * inference engine is a C++ program:
 *
 *   - Thread-local storage, reached through FS. libstdc++ keeps its exception
 *     state there, and every function it compiled with the stack protector
 *     compares against a guard at FS:0x28. A thread with no FS base cannot run
 *     either -- it faults on the first `mov %fs:0x28`.
 *   - Constructors that run before `main`, and handlers that run at `exit`.
 *   - A way for the unwinder to find the exception tables of the code a throw
 *     is unwinding through.
 *   - `errno` that belongs to the thread that set it.
 *   - Somewhere to report a smashed stack.
 *
 * Thread-local storage is laid out the way the x86-64 ELF TLS ABI lays out a
 * static executable's, variant II: the thread pointer points at a control
 * block, the image's TLS segment sits immediately below it, and a variable is
 * found at a negative offset the linker computed. The block's first word is
 * the thread pointer itself, which is how code reads `%fs:0` to learn where it
 * is. Whether this layout is the one the linker assumed is not taken on trust:
 * every thread reads a thread-local variable with a known initial value, and
 * its own stack guard, back through FS before anything else runs on it.
 */

#include "thalyx/nrt.h"
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include <stdlib.h>

#define TH_NOTE_TLS_UP        0x5009ull
#define TH_NOTE_TLS_BAD       0x500Aull
#define TH_NOTE_CONSTRUCTORS  0x500Bull
#define TH_NOTE_STACK_SMASHED 0x500Cull
#define TH_NOTE_EXIT_HANDLERS 0x500Dull

/* ------------------------------------------------ thread-local storage */

extern const uint8_t __tdata_start[];
extern const uint8_t __tdata_end[];
extern const uint8_t __tbss_end[];
extern const uint8_t __tls_align[];   /* absolute: the TLS segment's alignment */

#define TH_TLS_AREA 8192u   /* one thread's TLS block and control block      */
#define TH_TCB_SIZE 256u    /* zeroed except the words named below           */

typedef struct {
    uint64_t self;          /* 0x00  the thread pointer itself               */
    uint64_t dtv;           /* 0x08  no dynamic modules: zero               */
    uint64_t self_again;    /* 0x10  glibc keeps the pointer here as well    */
    uint64_t reserved[2];   /* 0x18                                          */
    uint64_t stack_guard;   /* 0x28  what stack-protected code compares with */
    uint64_t pointer_guard; /* 0x30                                          */
} th_tcb;

_Static_assert(__builtin_offsetof(th_tcb, stack_guard) == 0x28, "stack guard at FS:0x28");

static uint8_t tls_space[TH_THREAD_MAX + 1][TH_TLS_AREA] __attribute__((aligned(4096)));

/* Non-zero on purpose: it puts this runtime in `.tdata`, and it is the value a
 * thread reads back through FS to prove the linker and this file agree.
 * `volatile`, because a thread-local nothing writes is otherwise a constant the
 * compiler folds -- and the check would compile to nothing. */
static volatile __thread uint64_t tls_probe = 0x544C5350524F4245ull;

static uint64_t mix(uint64_t x)
{
    x ^= x >> 30; x *= 0xbf58476d1ce4e5b9ull;
    x ^= x >> 27; x *= 0x94d049bb133111ebull;
    x ^= x >> 31;
    return x;
}

int th_tls_install(unsigned index)
{
    if (index > TH_THREAD_MAX) { return 1; }
    uint64_t filesz = (uint64_t)(__tdata_end - __tdata_start);
    uint64_t memsz = (uint64_t)(__tbss_end - __tdata_start);
    uint64_t align = (uint64_t)(uintptr_t)__tls_align;
    if (align == 0) { align = 1; }
    if (align > 4096 || (align & (align - 1)) != 0) {
        th_note(TH_NOTE_TLS_BAD, 2);
        return 2;
    }
    /* The ABI's offset of the block below the thread pointer: its size,
     * rounded up to its alignment. */
    uint64_t block = (memsz + align - 1) & ~(align - 1);
    uint64_t lead = align > 64 ? align : 64;
    uint64_t below = (block + lead - 1) & ~(lead - 1);
    if (below + TH_TCB_SIZE > TH_TLS_AREA) {
        th_note(TH_NOTE_TLS_BAD, 3);
        return 3;
    }

    uint8_t *area = tls_space[index];
    memset(area, 0, TH_TLS_AREA);
    uint8_t *tp = area + below;
    memcpy(tp - block, __tdata_start, (size_t)filesz);

    th_tcb *tcb = (th_tcb *)tp;
    uint64_t guard = mix(th_boot()->seed ^ th_now_ns() ^ ((uint64_t)index << 56)) & ~0xFFull;
    tcb->self = (uint64_t)(uintptr_t)tp;
    tcb->self_again = (uint64_t)(uintptr_t)tp;
    tcb->stack_guard = guard;
    tcb->pointer_guard = mix(guard ^ 0x7074725f67756172ull);

    int status = th_set_thread_pointer((uint64_t)(uintptr_t)tp);
    if (status != THALYX_STATUS_OK) {
        th_note(TH_NOTE_TLS_BAD, 4);
        return 4;
    }

    /* The two reads that decide whether the layout is right. */
    uint64_t seen_guard;
    __asm__ __volatile__("movq %%fs:0x28, %0" : "=r"(seen_guard));
    if (seen_guard != guard || tls_probe != 0x544C5350524F4245ull) {
        th_note(TH_NOTE_TLS_BAD, 5);
        return 5;
    }
    if (!th_quiet_startup()) {
        th_note(TH_NOTE_TLS_UP, ((uint64_t)index << 32) | memsz);
    }
    return 0;
}

/* The prebuilt library asks whether the process is single threaded to skip
 * atomics on reference counts. It is not -- a domain may have built threads --
 * and answering no costs a locked instruction where yes could cost a race. */
char __libc_single_threaded = 0;

/* ----------------------------------------------------------------- errno */

static int errno_slots[TH_THREAD_MAX + 1];

int *__errno_location(void)
{
    return &errno_slots[th_thread_index()];
}

/* ------------------------------------------ constructors and exit handlers */

typedef void (*th_init_fn)(int, char **, char **);
typedef void (*th_fini_fn)(void);

extern th_init_fn __preinit_array_start[];
extern th_init_fn __preinit_array_end[];
extern th_init_fn __init_array_start[];
extern th_init_fn __init_array_end[];
extern th_fini_fn __fini_array_start[];
extern th_fini_fn __fini_array_end[];

static char *no_arguments[1] = { NULL };

void th_run_constructors(void)
{
    uint64_t ran = 0;
    for (th_init_fn *fn = __preinit_array_start; fn < __preinit_array_end; fn++) {
        (*fn)(0, no_arguments, no_arguments);
        ran++;
    }
    for (th_init_fn *fn = __init_array_start; fn < __init_array_end; fn++) {
        (*fn)(0, no_arguments, no_arguments);
        ran++;
    }
    if (!th_quiet_startup()) { th_note(TH_NOTE_CONSTRUCTORS, ran); }
}

/* A fixed table, refused when full rather than grown: an `atexit` that could
 * allocate could fail in a way its caller has no way to see. */
#define TH_EXIT_HANDLERS 64

typedef struct {
    void (*with_argument)(void *);
    void (*plain)(void);
    void *argument;
} th_handler;

static th_handler handlers[TH_EXIT_HANDLERS];
static unsigned handler_count;
static th_handler quick_handlers[TH_EXIT_HANDLERS];
static unsigned quick_count;

void *__dso_handle = &__dso_handle;

static int take(th_handler *table, unsigned *count)
{
    unsigned slot = __atomic_fetch_add(count, 1, __ATOMIC_ACQ_REL);
    if (slot >= TH_EXIT_HANDLERS) {
        __atomic_fetch_sub(count, 1, __ATOMIC_ACQ_REL);
        return -1;
    }
    (void)table;
    return (int)slot;
}

int __cxa_atexit(void (*fn)(void *), void *argument, void *dso)
{
    (void)dso;
    int slot = take(handlers, &handler_count);
    if (slot < 0) { return -1; }
    handlers[slot].argument = argument;
    __atomic_store_n(&handlers[slot].with_argument, fn, __ATOMIC_RELEASE);
    return 0;
}

int atexit(void (*fn)(void))
{
    int slot = take(handlers, &handler_count);
    if (slot < 0) { return -1; }
    __atomic_store_n(&handlers[slot].plain, fn, __ATOMIC_RELEASE);
    return 0;
}

int at_quick_exit(void (*fn)(void))
{
    int slot = take(quick_handlers, &quick_count);
    if (slot < 0) { return -1; }
    __atomic_store_n(&quick_handlers[slot].plain, fn, __ATOMIC_RELEASE);
    return 0;
}

static void run_table(th_handler *table, unsigned *count)
{
    for (;;) {
        unsigned n = __atomic_load_n(count, __ATOMIC_ACQUIRE);
        if (n == 0) { break; }
        th_handler handler = table[n - 1];
        __atomic_store_n(count, n - 1, __ATOMIC_RELEASE);
        if (handler.with_argument) { handler.with_argument(handler.argument); }
        else if (handler.plain) { handler.plain(); }
    }
}

void th_run_exit_handlers(void)
{
    uint64_t pending = __atomic_load_n(&handler_count, __ATOMIC_ACQUIRE);
    run_table(handlers, &handler_count);
    for (th_fini_fn *fn = __fini_array_end; fn > __fini_array_start; fn--) {
        (*(fn - 1))();
    }
    th_note(TH_NOTE_EXIT_HANDLERS, pending);
}

_Noreturn void quick_exit(int code)
{
    run_table(quick_handlers, &quick_count);
    _Exit(code);
}

/* ---------------------------------------------------- the stack protector */

_Noreturn void __stack_chk_fail(void)
{
    th_note(TH_NOTE_STACK_SMASHED, (uint64_t)(uintptr_t)__builtin_return_address(0));
    th_exit((uint64_t)-5);
}

/* ------------------------------------------ exception tables for unwinding */

/* What `_dl_find_object` answers, with glibc's x86-64 layout: no `eh_dbase`
 * and no `eh_count` on this architecture. */
struct dl_find_object {
    unsigned long long dlfo_flags;
    void *dlfo_map_start;
    void *dlfo_map_end;
    void *dlfo_link_map;
    void *dlfo_eh_frame;
    unsigned long long reserved[7];
};

extern const uint8_t __image_start[];
extern const uint8_t __image_end[];
extern const uint8_t __eh_frame_hdr_start[];

/* GCC's unwinder asks this for the object a program counter belongs to. There
 * is one object: this image, which the kernel mapped whole and nothing ever
 * loads beside. */
int _dl_find_object(void *pc, struct dl_find_object *out)
{
    const uint8_t *at = (const uint8_t *)pc;
    if (at < __image_start || at >= __image_end) { return -1; }
    memset(out, 0, sizeof(*out));
    out->dlfo_map_start = (void *)__image_start;
    out->dlfo_map_end = (void *)__image_end;
    out->dlfo_eh_frame = (void *)__eh_frame_hdr_start;
    return 0;
}
