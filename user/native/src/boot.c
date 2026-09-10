/* Standing the runtime up, and the record it stands on.
 *
 * `_start` has already moved the stack; everything a C program assumes to be
 * true before `main` happens here. In order: zero `.bss`, check that the kernel
 * speaks the interface this image was built against, read the boot record the
 * supervisor mapped read-only, start the heap, and only then call the program.
 *
 * The version check is first because it is the one failure that makes every
 * later diagnosis wrong: a program built against a different interface would
 * otherwise report a structure mismatch as a logic error.
 */

#include "thalyx/nrt.h"
#include "thalyx/image.h"
#include <string.h>
#include <stdlib.h>

extern uint8_t __bss_start[];
extern uint8_t __bss_end[];
void _start(void);
void th_thread_trampoline(void);

/* The record a launcher reads. `KEEP` in the linker script puts it at the
 * start of the first loadable segment; nothing calls it and nothing may
 * garbage-collect it. */
__attribute__((used, section(".thalyx.image")))
const th_image_header th_image = {
    TH_IMAGE_MAGIC,
    1,
    0,
    (uint64_t)(uintptr_t)&_start,
    (uint64_t)(uintptr_t)&th_thread_trampoline,
    (uint64_t)(uintptr_t)__bss_start,
    (uint64_t)(uintptr_t)__bss_end,
    0,
};

/* K5 note numbers. Their own range, so a note from this runtime can never be
 * read as a K2, K3 or K4 one by a gate looking at one log. */
#define TH_NOTE_RUNTIME_UP     0x5001ull
#define TH_NOTE_ABI_MISMATCH   0x5002ull
#define TH_NOTE_CONFIG_BAD     0x5003ull
#define TH_NOTE_EXIT           0x5004ull
#define TH_NOTE_ASSERT         0x5005ull

static const th_config *config_page = (const th_config *)(uintptr_t)TH_CONFIG_VADDR;

const th_config *th_boot(void) { return config_page; }

uint64_t thalyx_boot_handle_of(uint32_t slot)
{
    /* Generation one: the supervisor installs into slots that have never been
     * used, so the handle is the slot with a generation of one above it. */
    return ((uint64_t)1u << 32) | (uint64_t)slot;
}

_Noreturn void th_runtime_start(void)
{
    memset(__bss_start, 0, (size_t)(__bss_end - __bss_start));

    uint64_t packed = th_abi_version();
    uint32_t major = (uint32_t)((packed >> 16) & 0xFFFF);
    uint32_t minor = (uint32_t)(packed & 0xFFFF);
    if (major != THALYX_ABI_VERSION_MAJOR || minor != THALYX_ABI_VERSION_MINOR) {
        th_note(TH_NOTE_ABI_MISMATCH, packed);
        th_exit((uint64_t)-1);
    }

    if (config_page->magic != TH_CONFIG_MAGIC) {
        th_note(TH_NOTE_CONFIG_BAD, config_page->magic);
        th_exit((uint64_t)-2);
    }

    th_heap_init();
    th_note(TH_NOTE_RUNTIME_UP, config_page->role);

    int code = th_main(config_page);
    th_note(TH_NOTE_EXIT, (uint64_t)(int64_t)code);
    /* Said before leaving, so a launcher waits on a signal instead of watching
     * the domain state change. The exit code is still the domain's. */
    th_signal_raise(TH_SLOT_SIGNAL_DONE, TH_BIT_PROGRAM_DONE);
    th_exit((uint64_t)(int64_t)code);
}

_Noreturn void exit(int code)
{
    th_note(TH_NOTE_EXIT, (uint64_t)(int64_t)code);
    th_exit((uint64_t)(int64_t)code);
}

_Noreturn void abort(void)
{
    th_note(TH_NOTE_EXIT, (uint64_t)-3);
    th_exit((uint64_t)-3);
}

_Noreturn void th_assert_failed(const char *expression, const char *file, int line)
{
    /* Two integers is all the diagnostic plane carries, so the assertion
     * reports where it was rather than what it said: a hash of the file and the
     * line, which is enough to find it and cannot be mistaken for evidence. */
    uint64_t hash = 1469598103934665603ull;
    for (const char *p = file; p && *p; p++) {
        hash = (hash ^ (uint8_t)*p) * 1099511628211ull;
    }
    (void)expression;
    th_note(TH_NOTE_ASSERT, (hash << 16) | (uint64_t)(line & 0xFFFF));
    th_exit((uint64_t)-4);
}

char *getenv(const char *name)
{
    /* There is no environment on this system and inventing one would be a
     * compatibility shim that lies. */
    (void)name;
    return NULL;
}
