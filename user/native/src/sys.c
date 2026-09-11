/* The five kernel entries and the descriptor staging every operation uses. */

#include "thalyx/sys.h"
#include "thalyx/nrt.h"
#include <string.h>

uint64_t th_abi_version(void)
{
    th_result r = th_invoke(THALYX_ENTRY_VERSION_QUERY, 0, 0, NULL, 0, 0, 0);
    return r.status == THALYX_STATUS_OK ? r.aux : 0;
}

int th_limits(thalyx_limits_t *out)
{
    th_result r = th_invoke(THALYX_ENTRY_LIMITS_QUERY, 0, 0, out,
                            sizeof(*out), 0, 0);
    return (int)r.status;
}

uint64_t th_now_ns(void)
{
    th_result r = th_invoke(THALYX_ENTRY_CLOCK_QUERY, 0, 0, NULL, 0, 0, 0);
    return r.status == THALYX_STATUS_OK ? r.aux : 0;
}

int th_set_thread_pointer(uint64_t address)
{
    th_result r = th_invoke(THALYX_ENTRY_THREAD_POINTER_SET, 0, address, NULL, 0, 0, 0);
    return (int)r.status;
}

_Noreturn void th_exit(uint64_t code)
{
    for (;;) {
        th_invoke(THALYX_ENTRY_EXIT, 0, code, NULL, 0, 0, 0);
        __asm__ __volatile__("pause");
    }
}

void th_note(uint64_t check, uint64_t value)
{
    th_invoke(TH_DIAG_NOTE, 0, TH_NOTE_SELF_CHECK, (const void *)(uintptr_t)check,
              value, 0, 0);
}

/* The descriptor the kernel checks against R10.
 *
 * `thalyx_op_spec` is the generated table: the length is read from the same
 * place the kernel reads it, so a program cannot submit a descriptor prepared
 * for one operation as another and cannot guess a size.
 */
void th_desc_begin(th_desc *d, uint32_t operation)
{
    const thalyx_op_spec_t *spec = thalyx_op_spec(operation);
    uint32_t len = spec ? spec->descriptor_len : 0;
    memset(d->bytes, 0, sizeof(d->bytes));
    thalyx_descriptor_header_t header;
    memset(&header, 0, sizeof(header));
    header.major = THALYX_ABI_VERSION_MAJOR;
    header.minor = THALYX_ABI_VERSION_MINOR;
    header.opcode = operation;
    header.total_len = len;
    memcpy(d->bytes, &header, sizeof(header));
    d->len = len;
}

void th_desc_put(th_desc *d, size_t offset, const void *src, size_t len)
{
    if (offset + len <= sizeof(d->bytes)) {
        memcpy(d->bytes + offset, src, len);
    }
}

th_result th_op_flags(uint64_t handle, uint32_t operation, th_desc *d,
                      uint64_t flags, uint64_t deadline)
{
    if (d == NULL) {
        return th_invoke(THALYX_ENTRY_INVOKE, handle, operation, NULL, 0, flags, deadline);
    }
    return th_invoke(THALYX_ENTRY_INVOKE, handle, operation, d->bytes, d->len,
                     flags, deadline);
}

th_result th_op(uint64_t handle, uint32_t operation, th_desc *d, uint64_t deadline)
{
    return th_op_flags(handle, operation, d, 0, deadline);
}

/* The DIAG_NOTE entry takes its two values in RDX and R10, which is why the
 * wrapper above passes them where a descriptor and a length would go. It is K1
 * scaffolding and stays marked as such: it reserves nothing, it can coalesce,
 * and nothing durable may be derived from it. */
