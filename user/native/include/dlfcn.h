/* Dynamic loading: there is no loader.
 *
 * A native image is linked whole and the kernel maps it whole; nothing on this
 * system maps a second object into a running domain. ggml can load backends
 * from shared objects at run time and is written against this interface, so
 * the interface is here, and `dlopen` refuses with a message that says why.
 */
#ifndef _DLFCN_H
#define _DLFCN_H

#include "thalyx/cdefs.h"

#define RTLD_LAZY   0x00001
#define RTLD_NOW    0x00002
#define RTLD_GLOBAL 0x00100
#define RTLD_LOCAL  0

__TH_BEGIN_DECLS
void *dlopen(const char *path, int mode) __TH_NOTHROW;
void *dlsym(void *handle, const char *name) __TH_NOTHROW;
int   dlclose(void *handle) __TH_NOTHROW;
char *dlerror(void) __TH_NOTHROW;
__TH_END_DECLS

#endif
