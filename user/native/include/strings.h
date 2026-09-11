/* Case-insensitive comparison, in the C locale. */
#ifndef _STRINGS_H
#define _STRINGS_H

#include <stddef.h>
#include "thalyx/cdefs.h"

__TH_BEGIN_DECLS
int strcasecmp(const char *a, const char *b) __TH_NOTHROW;
int strncasecmp(const char *a, const char *b, size_t n) __TH_NOTHROW;
__TH_END_DECLS

#endif
