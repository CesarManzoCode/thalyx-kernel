/* Format macros for the exact-width integers.
 *
 * `int64_t` is `long` in this ABI, as it is in glibc's x86-64 one, so the
 * 64-bit conversions carry the `l` prefix and not `ll`. With `ll` a correct
 * program compiled against these macros draws a format warning for every
 * 64-bit value it prints.
 */
#ifndef _INTTYPES_H
#define _INTTYPES_H

#include <stdint.h>
#include "thalyx/cdefs.h"

#define PRId8  "d"
#define PRIi8  "i"
#define PRIu8  "u"
#define PRIx8  "x"
#define PRIX8  "X"
#define PRIo8  "o"
#define PRId16 "d"
#define PRIi16 "i"
#define PRIu16 "u"
#define PRIx16 "x"
#define PRIX16 "X"
#define PRIo16 "o"
#define PRId32 "d"
#define PRIi32 "i"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"
#define PRIo32 "o"
#define PRId64 "ld"
#define PRIi64 "li"
#define PRIu64 "lu"
#define PRIx64 "lx"
#define PRIX64 "lX"
#define PRIo64 "lo"
#define PRIdPTR "ld"
#define PRIiPTR "li"
#define PRIuPTR "lu"
#define PRIxPTR "lx"
#define PRIXPTR "lX"
#define PRIdMAX "ld"
#define PRIiMAX "li"
#define PRIuMAX "lu"
#define PRIxMAX "lx"
#define PRIXMAX "lX"

#define SCNd32 "d"
#define SCNi32 "i"
#define SCNu32 "u"
#define SCNx32 "x"
#define SCNd64 "ld"
#define SCNi64 "li"
#define SCNu64 "lu"
#define SCNx64 "lx"

typedef struct { intmax_t quot; intmax_t rem; } imaxdiv_t;

__TH_BEGIN_DECLS
intmax_t  imaxabs(intmax_t value) __TH_NOTHROW;
imaxdiv_t imaxdiv(intmax_t numerator, intmax_t denominator) __TH_NOTHROW;
intmax_t  strtoimax(const char *s, char **end, int base) __TH_NOTHROW;
uintmax_t strtoumax(const char *s, char **end, int base) __TH_NOTHROW;
intmax_t  wcstoimax(const __WCHAR_TYPE__ *s, __WCHAR_TYPE__ **end, int base) __TH_NOTHROW;
uintmax_t wcstoumax(const __WCHAR_TYPE__ *s, __WCHAR_TYPE__ **end, int base) __TH_NOTHROW;
__TH_END_DECLS

#endif
