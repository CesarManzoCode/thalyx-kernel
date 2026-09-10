/* What a native image tells its launcher about itself.
 *
 * A supervisor building a domain from an image needs two addresses the ELF
 * header does not carry: where a thread other than the first one starts, and
 * where `.bss` is, so it can be charged and checked. Guessing either from the
 * layout would make the link script part of the supervisor's assumptions.
 *
 * So the image publishes them. The record sits at the start of the first
 * loadable segment -- the linker script keeps `.thalyx.image` there -- and a
 * launcher finds it by reading the program headers, which is what a launcher
 * has to do anyway to know where anything is.
 */
#ifndef THALYX_IMAGE_H
#define THALYX_IMAGE_H

#include <stdint.h>

#define TH_IMAGE_MAGIC 0x35474D4958544C48ull /* "HLTXIMG5" little-endian */

typedef struct {
    uint64_t magic;
    uint32_t version;
    uint32_t flags;
    uint64_t start;              /* &_start; equals the ELF entry point      */
    uint64_t thread_trampoline;  /* where a built thread begins              */
    uint64_t bss_start;
    uint64_t bss_end;
    uint64_t reserved;
} th_image_header;

#endif
