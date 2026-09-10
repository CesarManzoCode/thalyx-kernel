#ifndef _UNISTD_H
#define _UNISTD_H
#include <stddef.h>
#include <stdint.h>
typedef long ssize_t;

/* There are no links here and no paths. Declared because ported code refers to
 * it on the platform it was written for; it always refuses. */
ssize_t readlink(const char *path, char *into, size_t size);
#endif
