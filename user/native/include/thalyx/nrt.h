/* The native user runtime: what a C program on this system stands on.
 *
 * `vault/integration/thalyx.md` asks for a real port rather than a trait: a
 * program needs allocation, threads, time, mappings, a transport and a way to
 * be launched, and each of those is a system contract rather than a language
 * feature. This header is that surface for C. There is no POSIX behind it and
 * nothing here is a compatibility shim: `malloc` is memory objects the program
 * created against its own scope, a thread is a kernel thread added to its own
 * domain, and a message is an invocation the kernel stamped with an origin.
 *
 * The address plan below is part of the target contract, not a program's
 * choice: a supervisor building a native domain maps the same regions at the
 * same addresses for every one of them, so `_start` can stand on a stack
 * before it has read anything.
 */
#ifndef THALYX_NRT_H
#define THALYX_NRT_H

#include <stddef.h>
#include <stdint.h>
#include "thalyx/sys.h"

__TH_BEGIN_DECLS

/* ------------------------------------------------------------------ layout */

#define TH_IMAGE_BASE      0x00400000ull /* where the linker script puts the image */
#define TH_CONFIG_VADDR    0x10000000ull /* one read-only page: the boot record   */
#define TH_SHARED_VADDR    0x10010000ull /* role-specific shared region           */
#define TH_SHARED_MAX      0x00100000ull
#define TH_STACK_BASE      0x20000000ull /* main stack region, grows down from top */
#define TH_STACK_TOP       0x20200000ull
#define TH_HEAP_BASE       0x30000000ull /* heap arenas, one memory object each    */
#define TH_HEAP_LIMIT      0x38000000ull
#define TH_THREAD_STACKS   0x40000000ull /* one aligned slot per thread            */
#define TH_THREAD_SLOT     0x00040000ull /* 256 KiB per thread, power of two       */
#define TH_THREAD_MAX      3u            /* besides the initial thread             */
#define TH_BULK_VADDR      0x50000000ull /* large read-only data, weights included */
#define TH_XFER_VADDR      0x60000000ull /* bounded transfer buffers               */

/* ------------------------------------------------------- the boot record */

#define TH_CONFIG_MAGIC 0x354B584C4148545Aull /* "ZTHALXK5" little-endian */

/* Capability slots a native domain is built with. A slot names nothing until
 * the supervisor installs a capability in it; the numbers are the agreement
 * between the supervisor and the program, not authority by themselves. */
enum {
    TH_SLOT_SELF_SCOPE   = 0,  /* own work scope: create memory, query budget   */
    TH_SLOT_SELF_DOMAIN  = 1,  /* own domain: map what it created               */
    TH_SLOT_SERVICE      = 2,  /* the endpoint facet this program calls         */
    TH_SLOT_INBOUND      = 3,  /* the endpoint this program receives on         */
    TH_SLOT_LOG          = 4,  /* control log, when the role may append         */
    TH_SLOT_SIGNAL_WORK  = 5,  /* one bit per worker: there is work for you     */
    TH_SLOT_SIGNAL_DONE  = 6,  /* one bit per worker: I have finished           */
    TH_SLOT_BULK         = 7,  /* sealed bulk object, when the role has one     */
    TH_SLOT_XFER         = 8,  /* transfer buffer the program lends             */
    TH_SLOT_AUX0         = 9,
    TH_SLOT_AUX1         = 10,
    TH_SLOT_AUX2         = 11,
    TH_SLOT_AUX3         = 12,
    TH_SLOT_AUX4         = 13,
    TH_SLOT_AUX5         = 14,
    TH_SLOT_FIRST_FREE   = 15
};

/* Handle of a capability a supervisor installed into a slot that had never been
 * used: generations start at one, so the handle is the slot with a one above
 * it. A slot with nothing in it answers `NO_CAPABILITY`, which is how a program
 * discovers what it was not given. */
uint64_t thalyx_boot_handle_of(uint32_t slot);
void th_runtime_start(void) __TH_NORETURN;
void th_thread_body(uint64_t index);
int  th_thread_join(unsigned index);
int64_t th_signal_raise(uint32_t slot, uint64_t mask);
int64_t th_signal_wait(uint32_t slot, uint64_t mask, uint64_t deadline_ns);

/* Bit the runtime raises on TH_SLOT_SIGNAL_DONE when the program returns, so a
 * launcher can wait for it instead of watching the domain state. */
#define TH_BIT_PROGRAM_DONE 1ull

typedef struct {
    uint64_t magic;
    uint32_t version;
    uint32_t role;            /* what this domain is; the program does not choose */
    uint64_t instance;        /* which one of that role                           */
    uint64_t heap_pages;      /* pages the program may hold across all arenas     */
    uint64_t arena_pages;     /* pages in one arena; the growth step              */
    uint64_t shared_bytes;    /* bytes mapped at TH_SHARED_VADDR, zero when none  */
    uint64_t bulk_bytes;      /* bytes mapped at TH_BULK_VADDR, zero when none    */
    uint64_t xfer_bytes;      /* bytes mapped at TH_XFER_VADDR, zero when none    */
    uint64_t seed;            /* run seed, so a program cannot precompute a run   */
    uint64_t arg0;
    uint64_t arg1;
    uint64_t arg2;
    uint64_t arg3;
} th_config;

const th_config *th_boot(void);

/* ---------------------------------------------------------------- the heap */

void th_heap_init(void);
uint64_t th_heap_pages_held(void);
uint64_t th_heap_bytes_in_use(void);
int th_heap_last_error(void);

/* ------------------------------------------------------------------ memory */

/* Creates a memory object in the program's own scope and maps it into the
 * program's own domain at `vaddr`. Both authorities are capabilities the
 * supervisor installed; a program without them cannot grow. */
int th_map_new(uint64_t vaddr, uint64_t pages, uint32_t rights, const char *label);

/* ----------------------------------------------------------------- threads */

typedef void (*th_thread_fn)(void *argument);
int  th_thread_start(unsigned index, th_thread_fn body, void *argument);
void th_thread_exit(void);
unsigned th_thread_index(void);
void th_yield_ns(uint64_t nanoseconds);

/* --------------------------------------------------------------- transport */

/* A bounded message: what fits inline, plus one lent capability. Anything
 * larger travels through a memory object whose capability the message carries,
 * which is the only way past MAX_INLINE_PAYLOAD and is deliberately visible. */
typedef struct {
    uint8_t  bytes[THALYX_MAX_INLINE_PAYLOAD];
    uint32_t len;
} th_payload;

typedef struct {
    thalyx_message_header_t header;
    uint64_t invocation;      /* handle of the invocation, for a received call */
    th_payload payload;
    uint64_t lent[THALYX_MAX_CAPS_PER_MESSAGE];
    uint32_t lent_count;
} th_message;

int64_t th_call(uint64_t facet, const th_payload *out, th_payload *in,
                uint64_t lend, uint32_t lend_rights, uint64_t deadline_ns);

/* Bytes through a capability rather than through a mapping.
 *
 * This is how a service reaches a buffer a caller lent it: bounded pieces, one
 * operation each, with the kernel checking the rights of every one. A service
 * that mapped the buffer instead would be holding authority over it after the
 * call, which is the thing per-request lending exists to avoid. */
int64_t th_memory_read(uint64_t memory, uint64_t offset, void *into, uint64_t len);
int64_t th_memory_write(uint64_t memory, uint64_t offset, const void *from, uint64_t len);

/* Charges this worker's execution to the scope the invocation came from.
 *
 * The client does not choose who pays: the scope comes from the invocation the
 * kernel stamped. This is what makes an inference cost the work that asked for
 * it while the weights stay charged to the service that holds them. */
int64_t th_bind_worker(uint64_t invocation);
int64_t th_unbind_worker(uint64_t invocation);
int64_t th_close(uint64_t handle);
int64_t th_receive(uint64_t endpoint, th_message *out, uint64_t deadline_ns);
int64_t th_reply(uint64_t invocation, uint64_t result, const th_payload *body);

/* -------------------------------------------------------------- reporting */

void th_note(uint64_t check, uint64_t value);
void th_log(const char *text);

/* Every program's entry point after the runtime is standing. */
int th_main(const th_config *config);

/* ------------------------------------------------- the C++ runtime closure */

/* Thread-local storage for thread `index`, installed through the kernel's
 * `THREAD_POINTER_SET`. The initial thread is index zero. Answers zero, or the
 * note value that says why the image's TLS does not fit. */
int th_tls_install(unsigned index);

/* Constructors the image carries, `.preinit_array` then `.init_array`, run once
 * on the initial thread before `th_main`; the handlers `atexit` and
 * `__cxa_atexit` registered, run in reverse by `exit`. */
void th_run_constructors(void);
void th_run_exit_handlers(void);

/* The read-only files this domain has: see `stdio.h`. */
void th_files_init(const th_config *config);

/* One wake bit per thread, raised by whoever releases what it waits for. */
void th_wake(unsigned index);
int  th_wait_wake(uint64_t deadline_ns);

/* Lets the built threads, parked since activation, start: called once by the
 * initial thread when `.bss` is zeroed and the runtime is standing. */
void th_threads_release(void);

__TH_END_DECLS

#endif /* THALYX_NRT_H */
