/* Non-local exit for the ported runtimes that need it.
 *
 * Callee-saved state only -- RBX, RBP, R12..R15, RSP and the return address --
 * because that is all a `longjmp` on this target has to restore: there are no
 * signal masks to save and nothing unwinds.
 */
#ifndef _SETJMP_H
#define _SETJMP_H
typedef unsigned long jmp_buf[8];
int  setjmp(jmp_buf env) __attribute__((returns_twice));
_Noreturn void longjmp(jmp_buf env, int value);
#define _setjmp setjmp
#define _longjmp longjmp
#endif
