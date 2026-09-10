/* Memory objects and mappings a program makes for itself.
 *
 * Two capabilities and no ambient authority: `TH_SLOT_SELF_SCOPE` is where the
 * pages are charged and `TH_SLOT_SELF_DOMAIN` is where the mapping lands. A
 * program built without either cannot grow, which is the intended way to build
 * one that must not.
 */

#include "thalyx/nrt.h"
#include <string.h>

int th_map_new(uint64_t vaddr, uint64_t pages, uint32_t rights, const char *label)
{
    uint64_t scope = thalyx_boot_handle_of(TH_SLOT_SELF_SCOPE);
    uint64_t domain = thalyx_boot_handle_of(TH_SLOT_SELF_DOMAIN);

    thalyx_memory_create_request_t create;
    memset(&create, 0, sizeof(create));
    create.pages = pages;
    create.max_rights = rights;
    for (unsigned i = 0; label && label[i] && i < 15; i++) { create.label[i] = (uint8_t)label[i]; }

    th_desc d;
    th_desc_begin(&d, THALYX_OP_SCOPE_CREATE_MEMORY);
    th_desc_put(&d, TH_BODY, &create, sizeof(create));
    th_result r = th_op(scope, THALYX_OP_SCOPE_CREATE_MEMORY, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return (int)r.status; }
    uint64_t memory = r.aux;

    thalyx_map_request_t map;
    memset(&map, 0, sizeof(map));
    map.memory_handle = memory;
    map.vaddr = vaddr;
    map.offset_pages = 0;
    map.page_count = (uint32_t)pages;
    /* The mapping asks for what it will use. `MEMORY_MAP` is the right to make
     * a mapping at all and is not itself a page permission. */
    map.rights = rights & ~(uint32_t)THALYX_RIGHT_MEMORY_MAP;
    th_desc_begin(&d, THALYX_OP_DOMAIN_MAP);
    th_desc_put(&d, TH_BODY, &map, sizeof(map));
    r = th_op(domain, THALYX_OP_DOMAIN_MAP, &d, 0);
    if (r.status != THALYX_STATUS_OK) { return (int)r.status; }
    return 0;
}
