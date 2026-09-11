/* Entry of every native C program.
 *
 * The kernel enters at `_start` with a 16-byte-aligned stack minus eight bytes
 * -- the alignment a System V function sees just after a `call` -- and every
 * other general-purpose register zeroed. That stack is the eight pages the
 * kernel maps under every domain, which is enough to reach this code and not
 * enough to run a language runtime, so the first thing that happens is a move
 * onto the region the supervisor mapped for this domain at a fixed address.
 *
 * Nothing before that line may touch the stack beyond its own frame, which is
 * why this is written here and not in C.
 */
    .section .text.startup, "ax", @progbits
    .globl _start
    .type _start, @function
_start:
    movabsq $0x20200000, %rsp     /* TH_STACK_TOP, sixteen-byte aligned       */
    xorl    %ebp, %ebp            /* the call below leaves RSP at 8 mod 16    */
    call    th_runtime_start
    hlt                           /* th_runtime_start does not return         */
    .size _start, . - _start

/* Callee-saved state and where to continue. No signal mask: there is none. */
    .globl setjmp
    .type setjmp, @function
setjmp:
    movq    %rbx,  0(%rdi)
    movq    %rbp,  8(%rdi)
    movq    %r12, 16(%rdi)
    movq    %r13, 24(%rdi)
    movq    %r14, 32(%rdi)
    movq    %r15, 40(%rdi)
    leaq    8(%rsp), %rax
    movq    %rax, 48(%rdi)        /* the caller's stack pointer               */
    movq    (%rsp), %rax
    movq    %rax, 56(%rdi)        /* the return address                       */
    xorl    %eax, %eax
    ret
    .size setjmp, . - setjmp

    .globl longjmp
    .type longjmp, @function
longjmp:
    movl    %esi, %eax
    testl   %eax, %eax
    jnz     1f
    movl    $1, %eax              /* setjmp must never appear to return zero  */
1:
    movq     0(%rdi), %rbx
    movq     8(%rdi), %rbp
    movq    16(%rdi), %r12
    movq    24(%rdi), %r13
    movq    32(%rdi), %r14
    movq    40(%rdi), %r15
    movq    48(%rdi), %rsp
    jmp     *56(%rdi)
    .size longjmp, . - longjmp

/* A thread the kernel started enters here with its argument in RDI: the kernel
 * puts the fourth argument of DOMAIN_ADD_THREAD in that register. The stack it
 * was given is already current. */
    .globl th_thread_trampoline
    .type th_thread_trampoline, @function
th_thread_trampoline:
    xorl    %ebp, %ebp
    subq    $8, %rsp              /* the kernel entered at 8 mod 16; the call
                                     below has to leave the callee at 8 too   */
    call    th_thread_body
    hlt
    .size th_thread_trampoline, . - th_thread_trampoline

    .section .note.GNU-stack, "", @progbits
