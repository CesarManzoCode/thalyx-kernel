/* Non-local exit for the ported runtimes that need it.
 *
 * Callee-saved state only -- RBX, RBP, R12..R15, RSP and the return address --
 * because that is all a `longjmp` on this target has to restore: there are no
 * signal masks to save. A `longjmp` does not unwind C++ frames, which is what
 * the standard says of it too.
 */
#ifndef _SETJMP_H
#define _SETJMP_H
#include "thalyx/cdefs.h"
typedef unsigned long jmp_buf[8];
__TH_BEGIN_DECLS
int  setjmp(jmp_buf env) __attribute__((returns_twice));
void longjmp(jmp_buf env, int value) __TH_NORETURN;
__TH_END_DECLS
#define _setjmp setjmp
#define _longjmp longjmp
#endif
