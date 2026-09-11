/* Character classes of the C locale.
 *
 * The mask names and bit positions are glibc's x86-64 ones, and that is not a
 * style choice: `std::ctype<char>` in the prebuilt C++ library reads its table
 * through `__ctype_b_loc()` and tests these exact bits, and its header takes
 * the mask values from here. A table with the right answers in the wrong bits
 * would classify every character wrongly and compile cleanly.
 */
#ifndef _CTYPE_H
#define _CTYPE_H

#include <stdint.h>
#include "thalyx/cdefs.h"

#define _ISbit(bit) ((bit) < 8 ? ((1 << (bit)) << 8) : ((1 << (bit)) >> 8))

enum {
    _ISupper  = _ISbit(0),
    _ISlower  = _ISbit(1),
    _ISalpha  = _ISbit(2),
    _ISdigit  = _ISbit(3),
    _ISxdigit = _ISbit(4),
    _ISspace  = _ISbit(5),
    _ISprint  = _ISbit(6),
    _ISgraph  = _ISbit(7),
    _ISblank  = _ISbit(8),
    _IScntrl  = _ISbit(9),
    _ISpunct  = _ISbit(10),
    _ISalnum  = _ISbit(11)
};

__TH_BEGIN_DECLS
/* Tables indexed from -128 to 255, as glibc lays them out. */
const unsigned short **__ctype_b_loc(void) __TH_NOTHROW __attribute__((__const__));
const int32_t **__ctype_tolower_loc(void) __TH_NOTHROW __attribute__((__const__));
const int32_t **__ctype_toupper_loc(void) __TH_NOTHROW __attribute__((__const__));

int isalnum(int c) __TH_NOTHROW;
int isalpha(int c) __TH_NOTHROW;
int isblank(int c) __TH_NOTHROW;
int iscntrl(int c) __TH_NOTHROW;
int isdigit(int c) __TH_NOTHROW;
int isgraph(int c) __TH_NOTHROW;
int islower(int c) __TH_NOTHROW;
int isprint(int c) __TH_NOTHROW;
int ispunct(int c) __TH_NOTHROW;
int isspace(int c) __TH_NOTHROW;
int isupper(int c) __TH_NOTHROW;
int isxdigit(int c) __TH_NOTHROW;
int tolower(int c) __TH_NOTHROW;
int toupper(int c) __TH_NOTHROW;
int isascii(int c) __TH_NOTHROW;
__TH_END_DECLS

#endif
