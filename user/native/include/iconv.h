/* Character set conversion: there is none.
 *
 * The C locale has one character set, and this system has no conversion
 * tables. The C++ library's encoding facets are written over this interface,
 * so it is declared, and every conversion is refused at `iconv_open` with
 * `EINVAL`, which is what glibc says of a pair of encodings it does not know.
 */
#ifndef _ICONV_H
#define _ICONV_H

#include <stddef.h>
#include "thalyx/cdefs.h"

typedef void *iconv_t;

__TH_BEGIN_DECLS
iconv_t iconv_open(const char *to, const char *from);
size_t  iconv(iconv_t cd, char **in, size_t *in_left, char **out, size_t *out_left);
int     iconv_close(iconv_t cd);
__TH_END_DECLS

#endif
