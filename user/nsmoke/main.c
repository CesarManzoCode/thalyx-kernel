/* The first native C program: does this target actually run?
 *
 * Not a fixture and not a print. Every note it emits is something that is only
 * true if a piece of the port works, and each one would be a different failure
 * if it did not:
 *
 *   - the image loaded, `.bss` was zeroed, and the stack the supervisor mapped
 *     is the one the program is standing on;
 *   - the kernel answers the version, limits and clock entries to C;
 *   - hardware double arithmetic works, which on this kernel means the eager
 *     FP save and restore covers what a C program uses;
 *   - the heap grew by creating a memory object against the program's own scope
 *     and mapping it into the program's own domain, and the bytes written
 *     through the new mapping read back;
 *   - the scope's page ceiling stops it, with the kernel's refusal and not with
 *     a number the program chose.
 */

#include "thalyx/nrt.h"
#include <stdlib.h>
#include <string.h>
#include <math.h>

#define NOTE_LIMITS       0x5100ull
#define NOTE_CLOCK        0x5101ull
#define NOTE_FLOAT        0x5102ull
#define NOTE_HEAP_GREW    0x5103ull
#define NOTE_HEAP_ROUND   0x5104ull
#define NOTE_HEAP_CEILING 0x5105ull
#define NOTE_BSS          0x5106ull
#define NOTE_MATH         0x5107ull
#define NOTE_HEAP_ERROR   0x5108ull
#define NOTE_HEAP_HELD    0x5109ull
#define NOTE_DONE         0x51FFull

/* In `.bss`: the runtime zeroes it, and a program that found it dirty would
 * report the value it actually found. */
static uint64_t zeroed_at_start[64];

int th_main(const th_config *config)
{
    uint64_t dirty = 0;
    for (unsigned i = 0; i < 64; i++) { dirty |= zeroed_at_start[i]; }
    th_note(NOTE_BSS, dirty);

    thalyx_limits_t limits;
    int status = th_limits(&limits);
    th_note(NOTE_LIMITS, status == THALYX_STATUS_OK
            ? ((uint64_t)limits.major << 48) | ((uint64_t)limits.minor << 32) | limits.cpus_online
            : (uint64_t)-1);

    uint64_t first = th_now_ns();
    th_yield_ns(2000000ull);
    uint64_t second = th_now_ns();
    th_note(NOTE_CLOCK, second > first ? second - first : 0);

    /* Hardware doubles, through a loop the compiler cannot fold: the seed comes
     * from the run and the result is reported, so a constant would be wrong. */
    double accumulator = 1.0 + (double)(config->seed & 0xFFFF) / 65536.0;
    for (int i = 0; i < 64; i++) {
        accumulator = accumulator * 1.0000001 + sqrt(accumulator) / 1024.0;
    }
    th_note(NOTE_FLOAT, (uint64_t)(accumulator * 1e9));

    /* exp and log against each other: exp(log(x)) is x to within what this
     * libm promises, and the note carries the error in parts per billion. */
    double round_trip = exp(log(3.7)) / 3.7;
    uint64_t error_ppb = (uint64_t)(fabs(round_trip - 1.0) * 1e9);
    th_note(NOTE_MATH, error_ppb);

    /* The heap: allocate past one arena so growth has to happen, and prove the
     * new mapping by reading back what was written through it. */
    uint64_t before = th_heap_pages_held();
    size_t block = (size_t)config->arena_pages * 4096u / 2u;
    unsigned char *first_block = malloc(block);
    unsigned char *second_block = malloc(block);
    unsigned char *third_block = malloc(block);
    if (first_block && second_block && third_block) {
        memset(first_block, 0xA5, block);
        memset(second_block, 0x5A, block);
        memset(third_block, 0x33, block);
        uint64_t sum = 0;
        for (size_t i = 0; i < block; i += 512) {
            sum += first_block[i] + second_block[i] + third_block[i];
        }
        th_note(NOTE_HEAP_ROUND, sum);
    } else {
        th_note(NOTE_HEAP_ROUND, 0);
    }
    th_note(NOTE_HEAP_GREW, th_heap_pages_held() - before);
    th_note(NOTE_HEAP_ERROR, (uint64_t)(int64_t)th_heap_last_error());

    /* And the ceiling, which is the kernel's and not the program's. The boot
     * record allows far more heap than the scope can charge, so what stops the
     * heap growing is `SCOPE_CREATE_MEMORY` refusing: the object is never
     * created, the mapping never happens, and `malloc` has nothing to hand
     * back. The note carries the kernel's own status so that "it stopped" and
     * "it stopped for the reason claimed" are two different observations. */
    unsigned refusals = 0;
    for (int i = 0; i < 4096; i++) {
        void *p = malloc(block);
        if (p == NULL) { refusals++; break; }
    }
    th_note(NOTE_HEAP_CEILING, refusals);
    th_note(NOTE_HEAP_ERROR, (uint64_t)(int64_t)th_heap_last_error());
    th_note(NOTE_HEAP_HELD, th_heap_pages_held());

    th_note(NOTE_DONE, th_heap_pages_held());
    return 0;
}
