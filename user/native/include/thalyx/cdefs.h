/* Declaration conventions every header of this C library shares.
 *
 * The same headers are read by C programs and by C++ ones, and by the
 * prebuilt C++ standard library's own headers, which include them to learn
 * what the C library provides. Three things follow.
 *
 * Every declaration has C linkage when read from C++, or a C++ program would
 * link against mangled names nothing defines.
 *
 * Functions glibc declares as not throwing are declared `noexcept` here as
 * well. GCC's own headers redeclare some of them (`<mm_malloc.h>` does
 * `posix_memalign`), and two declarations of one function with different
 * exception specifications is an error rather than a warning.
 *
 * `_Noreturn` is C and not C++, so the attribute spelling is used instead.
 */
#ifndef THALYX_CDEFS_H
#define THALYX_CDEFS_H

#ifdef __cplusplus
# define __TH_BEGIN_DECLS extern "C" {
# define __TH_END_DECLS   }
# define __TH_NOTHROW     noexcept(true)
#else
# define __TH_BEGIN_DECLS
# define __TH_END_DECLS
# define __TH_NOTHROW
#endif

#define __TH_NORETURN __attribute__((__noreturn__))

#endif
