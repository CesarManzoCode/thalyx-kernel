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
 *
 * One lock guards all of it. A domain may have built threads, and a C++
 * program allocates from all of them; a heap that assumed one thread would
 * corrupt itself the first time two allocated at once.
 *
 * An aligned block is carved out of an ordinary one: the front part up to the
 * aligned address becomes a free block of its own, so what `free` receives is
 * always a pointer this allocator produced with a header in front of it.
 *
 * Small blocks that are freed are kept, up to a few dozen per size, on a list
 * of their own and handed out again whole: a C++ program frees and allocates
 * the same few sizes over and over, and coalescing a block only to split the
 * same size off again is most of what such a pair costs. A kept block stays
 * marked in use, so its neighbours do not coalesce with it, and the lists are
 * given back to the bins before the heap grows or refuses: what a program can
 * allocate is what it could without them.
 *
 * A new arena is zero -- the kernel hands out zeroed frames -- and the part of
 * the newest one nothing has been carved from yet stays zero except for the
 * free block's own header and links at its start. `calloc` does not clear
 * that part again.
 */

#include "thalyx/nrt.h"
#include <stdlib.h>
#include <string.h>
#include <errno.h>

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
static int heap_lock;

/* Bytes known to be zero: the newest arena from just past the links of the
 * free block at its uncarved end, up to its fence. */
static uint64_t fresh_lo;
static uint64_t fresh_hi;
/* Whether the last block `allocate` handed out came from that part. */
static int carved_fresh;

/* The kept small blocks, one list per block size from `MIN_BLOCK` up in
 * `ALIGN` steps, threaded through the first word of each payload. */
#define KEEP_CLASSES 32u
#define KEEP_DEPTH   64u
#define KEEP_MAX     (MIN_BLOCK + (KEEP_CLASSES - 1u) * ALIGN)

static Block *kept[KEEP_CLASSES];
static unsigned kept_count[KEEP_CLASSES];
static uint64_t kept_total;

static void enter(void)
{
    unsigned spins = 0;
    while (__atomic_exchange_n(&heap_lock, 1, __ATOMIC_ACQUIRE)) {
        if (++spins < 256) { __builtin_ia32_pause(); } else { th_yield_ns(10000); spins = 0; }
    }
}

static void leave(void) { __atomic_store_n(&heap_lock, 0, __ATOMIC_RELEASE); }

static uint64_t block_size(const Block *b) { return b->size_flags & ~(uint64_t)(ALIGN - 1); }
static int block_inuse(const Block *b) { return (b->size_flags & FLAG_INUSE) != 0; }

/* Bin 0 holds sizes below 32, bin `i` sizes in [2^(i+4), 2^(i+5)), and the
 * last bin everything above. */
static unsigned bin_of(uint64_t size)
{
    if (size < 32) { return 0; }
    unsigned index = (unsigned)(63 - __builtin_clzll(size)) - 4u;
    return index < BINS ? index : BINS - 1u;
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

/* Adds one arena: a run of addresses backed by memory objects mapped end to
 * end, and one free block over all of it.
 *
 * Usually the run is one object. An allocation larger than one object may be
 * gets several, mapped at consecutive addresses: malloc needs contiguous
 * addresses, not one object, and the interface's per-object limit is a bound
 * on objects rather than on what a program may allocate. If a later object is
 * refused, the ones already mapped stay and become the arena, so pages the
 * scope was charged are never mapped and unusable. */
static int grow(uint64_t want_bytes)
{
    uint64_t pages = arena_pages;
    uint64_t need = (want_bytes + HEADER * 2 + 4095) / 4096;
    if (need > pages) { pages = need; }
    if (heap_pages_held + pages > heap_page_ceiling) { return 0; }
    uint64_t at = heap_next_vaddr;
    if (at + pages * 4096 > TH_HEAP_LIMIT) { return 0; }
    uint64_t mapped = 0;
    while (mapped < pages) {
        uint64_t chunk = pages - mapped;
        if (chunk > THALYX_MAX_MEMORY_PAGES_PER_OBJECT) { chunk = THALYX_MAX_MEMORY_PAGES_PER_OBJECT; }
        /* `MEMORY_MAP` has to be in the object's maximum rights, not only in
         * the mapping request: the kernel takes the intersection of what the
         * grant permits, what the object permits and what the platform can
         * express, and an object created without it can never be mapped. */
        heap_last_error = th_map_new(
            at + mapped * 4096, chunk,
            THALYX_RIGHT_MEMORY_READ | THALYX_RIGHT_MEMORY_WRITE | THALYX_RIGHT_MEMORY_MAP,
            "heap");
        if (heap_last_error != 0) { break; }
        mapped += chunk;
    }
    if (mapped == 0) { return 0; }
    pages = mapped;
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
    fresh_lo = at + sizeof(Free);
    fresh_hi = (uint64_t)(uintptr_t)fence;
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
    /* The payload past this block's links is zero if it lies in the fresh
     * part; whatever was split off after it starts with a header and links of
     * its own, and the fresh part starts past those. */
    uint64_t at = (uint64_t)(uintptr_t)block;
    carved_fresh = at + sizeof(Free) >= fresh_lo && at + have <= fresh_hi;
    if (at + have + sizeof(Free) > fresh_lo) { fresh_lo = at + have + sizeof(Free); }
    block->header.size_flags = have | FLAG_INUSE;
    Block *after = (Block *)((uint8_t *)block + have);
    after->prev_size = have;
    heap_in_use += have;
    return (uint8_t *)block + HEADER;
}

static void *find_fit(uint64_t want)
{
    for (unsigned index = bin_of(want); index < BINS; index++) {
        for (Free *block = bins[index]; block; block = block->next) {
            if (block_size(&block->header) >= want) { return carve(block, want); }
        }
    }
    return NULL;
}

static void release(void *p);

/* Gives every kept block back to the bins, coalescing as `free` would have. */
static void unkeep_all(void)
{
    for (unsigned index = 0; index < KEEP_CLASSES; index++) {
        while (kept[index]) {
            Block *block = kept[index];
            __builtin_memcpy(&kept[index], (uint8_t *)block + HEADER, sizeof(Block *));
            kept_count[index]--;
            kept_total--;
            heap_in_use += block_size(block);   /* `release` takes it off again */
            release((uint8_t *)block + HEADER);
        }
    }
}

static void *allocate(size_t n)
{
    if (!heap_ready) { th_heap_init(); }
    if (n == 0) { n = 1; }
    if (n > (size_t)1 << 40) { return NULL; }
    carved_fresh = 0;
    uint64_t want = (n + HEADER + ALIGN - 1) & ~(uint64_t)(ALIGN - 1);
    if (want < MIN_BLOCK) { want = MIN_BLOCK; }
    if (want <= KEEP_MAX) {
        unsigned index = (unsigned)((want - MIN_BLOCK) / ALIGN);
        Block *block = kept[index];
        if (block) {
            __builtin_memcpy(&kept[index], (uint8_t *)block + HEADER, sizeof(Block *));
            kept_count[index]--;
            kept_total--;
            heap_in_use += want;
            return (uint8_t *)block + HEADER;
        }
    }
    void *found = find_fit(want);
    if (found) { return found; }
    if (kept_total) {
        unkeep_all();
        found = find_fit(want);
        if (found) { return found; }
    }
    if (!grow(want)) { return NULL; }
    return find_fit(want);
}

static void release(void *p)
{
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

void *malloc(size_t n)
{
    enter();
    void *out = allocate(n);
    leave();
    if (out == NULL) { errno = ENOMEM; }
    return out;
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
    enter();
    Block *block = (Block *)((uint8_t *)p - HEADER);
    uint64_t size = block_size(block);
    if (size >= MIN_BLOCK && size <= KEEP_MAX) {
        unsigned index = (unsigned)((size - MIN_BLOCK) / ALIGN);
        if (kept_count[index] < KEEP_DEPTH) {
            __builtin_memcpy(p, &kept[index], sizeof(Block *));
            kept[index] = block;
            kept_count[index]++;
            kept_total++;
            heap_in_use -= size;
            leave();
            return;
        }
    }
    release(p);
    leave();
}

void *calloc(size_t count, size_t size)
{
    uint64_t total = (uint64_t)count * (uint64_t)size;
    if (size != 0 && total / size != count) { errno = ENOMEM; return NULL; }
    enter();
    void *out = allocate((size_t)total);
    int fresh = carved_fresh;
    leave();
    if (out == NULL) { errno = ENOMEM; return NULL; }
    /* Fresh memory is zero past the free block's links, which are the first
     * sixteen bytes of the payload. */
    memset(out, 0, fresh && total > 16 ? 16 : (size_t)total);
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

static void *allocate_aligned(size_t alignment, size_t n)
{
    if (alignment <= ALIGN) { return allocate(n); }
    uint64_t want = (n + HEADER + ALIGN - 1) & ~(uint64_t)(ALIGN - 1);
    if (want < MIN_BLOCK) { want = MIN_BLOCK; }
    uint8_t *p = allocate((size_t)(want + alignment + MIN_BLOCK));
    if (p == NULL) { return NULL; }
    uintptr_t payload = (uintptr_t)p;
    uintptr_t aligned = (payload + alignment - 1) & ~(uintptr_t)(alignment - 1);
    if (aligned == payload) { return p; }
    /* The gap in front has to be a block of its own, so it has to hold one. */
    while (aligned - payload < MIN_BLOCK) { aligned += alignment; }
    uint64_t front = aligned - payload;
    Block *old = (Block *)(p - HEADER);
    uint64_t total = block_size(old);
    Block *kept = (Block *)(aligned - HEADER);
    kept->size_flags = (total - front) | FLAG_INUSE;
    kept->prev_size = front;
    Block *after = (Block *)((uint8_t *)kept + (total - front));
    after->prev_size = total - front;
    old->size_flags = front | FLAG_INUSE;
    release((uint8_t *)old + HEADER);     /* the front, freed and coalesced */
    heap_in_use += front;                 /* `release` took it off; it was never handed out */
    heap_in_use -= front;
    return (void *)aligned;
}

int posix_memalign(void **out, size_t alignment, size_t size)
{
    if (alignment < sizeof(void *) || (alignment & (alignment - 1)) != 0) { return EINVAL; }
    enter();
    void *p = allocate_aligned(alignment, size);
    leave();
    if (p == NULL) { return ENOMEM; }
    *out = p;
    return 0;
}

void *aligned_alloc(size_t alignment, size_t size)
{
    void *p = NULL;
    int status = posix_memalign(&p, alignment < sizeof(void *) ? sizeof(void *) : alignment, size);
    if (status != 0) { errno = status; return NULL; }
    return p;
}

void *memalign(size_t alignment, size_t size) { return aligned_alloc(alignment, size); }
