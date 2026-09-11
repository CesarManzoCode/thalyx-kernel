/* Exact-width integers, as the compiler defines them.
 *
 * GCC's own <stdint.h> forwards to the C library's in a hosted compile, and a
 * C++ compile is hosted. The compiler already knows every width and limit for
 * this ABI, so this header is the compiler's definitions and nothing added:
 * `int64_t` is `long`, as it is in glibc's x86-64 interface.
 */
#ifndef _THALYX_STDINT_H
#define _THALYX_STDINT_H
#include <stdint-gcc.h>
#endif
