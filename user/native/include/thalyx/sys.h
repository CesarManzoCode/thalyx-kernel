/* The kernel interface, from C.
 *
 * `abi/include/thalyx_abi.h` is generated from `abi/schema/v0.json` and carries
 * the numbers and the structures. This header adds the one thing a generated
 * header cannot: the instruction that crosses the boundary, and the thin
 * wrappers a native program calls instead of writing that instruction again.
 *
 * The register contract is `vault/architecture/abi.md`, and it is the same one
 * `thalyx_abi::invoke` states on the Rust side: RAX the entry, RDI a handle,
 * RSI an operation, RDX a descriptor pointer, R10 its length, R8 flags, R9 a
 * monotonic deadline; RAX returns the status and RDX an auxiliary value. RCX
 * and R11 are destroyed by `syscall` itself.
 */
#ifndef THALYX_SYS_H
#define THALYX_SYS_H

#include <stdint.h>
#include <stddef.h>
#include "thalyx_abi.h"
#include "thalyx/cdefs.h"

__TH_BEGIN_DECLS

typedef struct {
    int64_t  status;
    uint64_t aux;
} th_result;

static inline th_result th_invoke(uint64_t entry, uint64_t handle, uint64_t operation,
                                  const void *descriptor, uint64_t len,
                                  uint64_t flags, uint64_t deadline)
{
    int64_t  status;
    uint64_t aux;
    register uint64_t r10 __asm__("r10") = len;
    register uint64_t r8  __asm__("r8")  = flags;
    register uint64_t r9  __asm__("r9")  = deadline;
    __asm__ __volatile__(
        "syscall"
        : "=a"(status), "=d"(aux)
        : "a"(entry), "D"(handle), "S"(operation), "d"((uint64_t)(uintptr_t)descriptor),
          "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory");
    th_result out = { status, aux };
    return out;
}

/* The assigned entries. */
uint64_t th_abi_version(void);
int      th_limits(thalyx_limits_t *out);
uint64_t th_now_ns(void);
void     th_exit(uint64_t code) __TH_NORETURN;
/* This thread's thread pointer: where its thread-local storage and its stack
 * guard are found through FS. Register state of the calling thread only. */
int      th_set_thread_pointer(uint64_t address);

/* Descriptor staging. Every operation that carries a body sends a
 * DescriptorHeader followed by the body; the header's `operation` field is what
 * the kernel dispatches on. `TH_BODY` is where a body starts. */
#define TH_BODY   ((size_t)sizeof(thalyx_descriptor_header_t))
#define TH_DESC_MAX 512u

typedef struct {
    uint8_t bytes[TH_DESC_MAX] __attribute__((aligned(16)));
    uint32_t len;
} th_desc;

void th_desc_begin(th_desc *d, uint32_t operation);
void th_desc_put(th_desc *d, size_t offset, const void *src, size_t len);
th_result th_op(uint64_t handle, uint32_t operation, th_desc *d, uint64_t deadline);
th_result th_op_flags(uint64_t handle, uint32_t operation, th_desc *d,
                      uint64_t flags, uint64_t deadline);

/* Diagnostic notes. K1 scaffolding, kept because it is the channel the gates
 * read a program's own observations from. Two integers and nothing else: a
 * program that wants to say more says it into durable state. */
#define TH_SCAFFOLD_MARKER (1ull << 63)
#define TH_DIAG_NOTE       (TH_SCAFFOLD_MARKER | 1ull)
#define TH_NOTE_SELF_CHECK 2ull

void th_note(uint64_t check, uint64_t value);

__TH_END_DECLS

#endif /* THALYX_SYS_H */
