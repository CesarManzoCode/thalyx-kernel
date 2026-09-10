/* The native heap: memory objects, mapped by the program, bounded by its scope.
 *
 * There is no `brk` and no ambient `mmap` here. An arena is a memory object the
 * program created against **its own work scope** and mapped into **its own
 * domain**, both through capabilities a supervisor installed. That is what makes
 * a language runtime's memory ceiling more than a promise: when the scope's page
 * ceiling is reached the kernel refuses to create the object, `malloc` returns
 * null, and the runtime above reports the ceiling it actually hit. Two limits,
 * one of them enforced by something the program cannot talk its way past.
 *
 * The allocator itself is an ordinary boundary-tag one: blocks carry the size of
 * the previous block and their own, free blocks are threaded through size-class
 * bins, and adjacent free blocks coalesce in both directions. Arenas do not
 * coalesce with each other -- they are separate objects at separate addresses,
 * even when the addresses happen to be adjacent -- so every arena ends in a
 * zero-length in-use fence that stops a forward walk.
 */

#include "thalyx/nrt.h"
#include <stdlib.h>
#include <string.h>

#define ALIGN      16u
#define HEADER     16u          /* prev_size + size_flags */
#define FLAG_INUSE 1u
#define MIN_BLOCK  (HEADER + 32u)
#define BINS       24u

typedef struct Block {
    uint64_t prev_size;
    uint64_t size_flags;
} Block;

typedef struct Free {
    Block header;
    struct Free *next;
    struct Free *prev;
} Free;

static Free *bins[BINS];
static uint64_t heap_next_vaddr;
static uint64_t heap_pages_held;
static uint64_t heap_in_use;
static uint64_t arena_pages;
static uint64_t heap_page_ceiling;
static int heap_ready;
static int heap_last_error;

static uint64_t block_size(const Block *b) { return b->size_flags & ~(uint64_t)(ALIGN - 1); }
static int block_inuse(const Block *b) { return (b->size_flags & FLAG_INUSE) != 0; }

static unsigned bin_of(uint64_t size)
{
    unsigned index = 0;
    uint64_t bound = 32;
    while (index + 1 < BINS && size >= bound) { bound <<= 1; index++; }
    return index;
}

static void bin_insert(Free *block)
{
    unsigned index = bin_of(block_size(&block->header));
    block->prev = NULL;
    block->next = bins[index];
    if (bins[index]) { bins[index]->prev = block; }
    bins[index] = block;
}

static void bin_remove(Free *block)
{
    unsigned index = bin_of(block_size(&block->header));
    if (block->prev) { block->prev->next = block->next; } else { bins[index] = block->next; }
    if (block->next) { block->next->prev = block->prev; }
}

static void publish_free(Block *block, uint64_t size)
{
    block->size_flags = size;
    Block *after = (Block *)((uint8_t *)block + size);
    after->prev_size = size;
    bin_insert((Free *)block);
}

/* Adds one arena: one memory object, one mapping, one free block. */
static int grow(uint64_t want_bytes)
{
    uint64_t pages = arena_pages;
    uint64_t need = (want_bytes + HEADER * 2 + 4095) / 4096;
    if (need > pages) { pages = need; }
    if (pages > THALYX_MAX_MEMORY_PAGES_PER_OBJECT) { return 0; }
    if (heap_pages_held + pages > heap_page_ceiling) { return 0; }
    uint64_t at = heap_next_vaddr;
    if (at + pages * 4096 > TH_HEAP_LIMIT) { return 0; }
    /* `MEMORY_MAP` has to be in the object's maximum rights, not only in the
     * mapping request: the kernel takes the intersection of what the grant
     * permits, what the object permits and what the platform can express, and
     * an object created without it can never be mapped by anybody. */
    heap_last_error = th_map_new(
        at, pages,
        THALYX_RIGHT_MEMORY_READ | THALYX_RIGHT_MEMORY_WRITE | THALYX_RIGHT_MEMORY_MAP,
        "heap");
    if (heap_last_error != 0) {
        return 0;
    }
    heap_next_vaddr = at + pages * 4096;
    heap_pages_held += pages;

    uint64_t span = pages * 4096;
    Block *first = (Block *)(uintptr_t)at;
    first->prev_size = 0;
    uint64_t usable = span - HEADER;      /* the fence takes the last header */
    usable &= ~(uint64_t)(ALIGN - 1);
    publish_free(first, usable);
    Block *fence = (Block *)((uint8_t *)first + usable);
    fence->size_flags = FLAG_INUSE;       /* zero length, in use: walking stops */
    return 1;
}

void th_heap_init(void)
{
    if (heap_ready) { return; }
    const th_config *config = th_boot();
    arena_pages = config->arena_pages ? config->arena_pages : 64;
    heap_page_ceiling = config->heap_pages ? config->heap_pages : 256;
    heap_next_vaddr = TH_HEAP_BASE;
    heap_ready = 1;
}

uint64_t th_heap_pages_held(void) { return heap_pages_held; }
int th_heap_last_error(void) { return heap_last_error; }
uint64_t th_heap_bytes_in_use(void) { return heap_in_use; }

static void *carve(Free *block, uint64_t want)
{
    bin_remove(block);
    uint64_t have = block_size(&block->header);
    if (have >= want + MIN_BLOCK) {
        Block *rest = (Block *)((uint8_t *)block + want);
        rest->prev_size = want;
        publish_free(rest, have - want);
        have = want;
    }
    block->header.size_flags = have | FLAG_INUSE;
    Block *after = (Block *)((uint8_t *)block + have);
    after->prev_size = have;
    heap_in_use += have;
    return (uint8_t *)block + HEADER;
}

void *malloc(size_t n)
{
    if (!heap_ready) { th_heap_init(); }
    if (n == 0) { n = 1; }
    uint64_t want = (n + HEADER + ALIGN - 1) & ~(uint64_t)(ALIGN - 1);
    if (want < MIN_BLOCK) { want = MIN_BLOCK; }

    for (unsigned index = bin_of(want); index < BINS; index++) {
        for (Free *block = bins[index]; block; block = block->next) {
            if (block_size(&block->header) >= want) { return carve(block, want); }
        }
    }
    if (!grow(want)) { return NULL; }
    for (unsigned index = bin_of(want); index < BINS; index++) {
        for (Free *block = bins[index]; block; block = block->next) {
            if (block_size(&block->header) >= want) { return carve(block, want); }
        }
    }
    return NULL;
}

size_t malloc_usable_size(void *p)
{
    if (p == NULL) { return 0; }
    Block *block = (Block *)((uint8_t *)p - HEADER);
    return (size_t)(block_size(block) - HEADER);
}

void free(void *p)
{
    if (p == NULL) { return; }
    Block *block = (Block *)((uint8_t *)p - HEADER);
    uint64_t size = block_size(block);
    heap_in_use -= size;

    Block *after = (Block *)((uint8_t *)block + size);
    if (!block_inuse(after)) {
        bin_remove((Free *)after);
        size += block_size(after);
    }
    if (block->prev_size != 0) {
        Block *before = (Block *)((uint8_t *)block - block->prev_size);
        if (!block_inuse(before)) {
            bin_remove((Free *)before);
            size += block_size(before);
            block = before;
        }
    }
    publish_free(block, size);
}

void *calloc(size_t count, size_t size)
{
    uint64_t total = (uint64_t)count * (uint64_t)size;
    if (size != 0 && total / size != count) { return NULL; }
    void *out = malloc((size_t)total);
    if (out) { memset(out, 0, (size_t)total); }
    return out;
}

void *realloc(void *p, size_t n)
{
    if (p == NULL) { return malloc(n); }
    if (n == 0) { free(p); return NULL; }
    size_t have = malloc_usable_size(p);
    if (have >= n) { return p; }
    void *out = malloc(n);
    if (out == NULL) { return NULL; }
    memcpy(out, p, have);
    free(p);
    return out;
}
