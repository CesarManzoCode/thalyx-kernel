/* Assertions that reach the diagnostic plane and stop the domain.
 *
 * NDEBUG turns them off exactly as the standard says. QuickJS is compiled with
 * NDEBUG, so the cost of the ones inside it is zero; the runtime's own code is
 * not, because an assertion that fired inside a language runtime and was
 * reported as an ordinary error would be the hardest kind of defect to find.
 */
#ifndef _ASSERT_H_TH
#define _ASSERT_H_TH
#include "thalyx/cdefs.h"
__TH_BEGIN_DECLS
void th_assert_failed(const char *expression, const char *file, int line) __TH_NORETURN;
__TH_END_DECLS
#endif

#undef assert
#ifdef NDEBUG
#define assert(e) ((void)0)
#else
#define assert(e) ((e) ? (void)0 : th_assert_failed(#e, __FILE__, __LINE__))
#endif

#if !defined(__cplusplus) && !defined(static_assert)
#define static_assert _Static_assert
#endif
