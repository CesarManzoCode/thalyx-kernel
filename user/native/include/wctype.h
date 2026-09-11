/* Wide character classes of the C locale.
 *
 * Declared whole because `<cwctype>` names all of it. A wide character above
 * 0x7F has no class in the C locale, which is what every function here answers
 * for one.
 */
#ifndef _WCTYPE_H
#define _WCTYPE_H

#include "thalyx/cdefs.h"

#ifndef __wint_t_defined
#define __wint_t_defined 1
typedef unsigned int wint_t;
#endif

typedef unsigned long wctype_t;
typedef const int *wctrans_t;

#ifndef WEOF
#define WEOF (0xffffffffu)
#endif

__TH_BEGIN_DECLS
int iswalnum(wint_t c) __TH_NOTHROW;
int iswalpha(wint_t c) __TH_NOTHROW;
int iswblank(wint_t c) __TH_NOTHROW;
int iswcntrl(wint_t c) __TH_NOTHROW;
int iswdigit(wint_t c) __TH_NOTHROW;
int iswgraph(wint_t c) __TH_NOTHROW;
int iswlower(wint_t c) __TH_NOTHROW;
int iswprint(wint_t c) __TH_NOTHROW;
int iswpunct(wint_t c) __TH_NOTHROW;
int iswspace(wint_t c) __TH_NOTHROW;
int iswupper(wint_t c) __TH_NOTHROW;
int iswxdigit(wint_t c) __TH_NOTHROW;
int iswctype(wint_t c, wctype_t type) __TH_NOTHROW;
wctype_t wctype(const char *name) __TH_NOTHROW;
wint_t towlower(wint_t c) __TH_NOTHROW;
wint_t towupper(wint_t c) __TH_NOTHROW;
wctrans_t wctrans(const char *name) __TH_NOTHROW;
wint_t towctrans(wint_t c, wctrans_t trans) __TH_NOTHROW;
__TH_END_DECLS

#endif
