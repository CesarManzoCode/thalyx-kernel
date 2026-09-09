//! Context switching.
//!
//! A thread that is not running is a kernel stack whose top holds, in order,
//! the six callee-saved registers, a return address and a trap frame. Resuming
//! it is therefore the same operation whether it was preempted by the timer,
//! stopped inside a system call, or has never run at all: the switch returns
//! into the shared trap return path, which restores the frame and executes
//! `iretq`.

use core::arch::naked_asm;

use super::gdt;
use super::trap::TrapFrame;

unsafe extern "C" {
    /// Shared trap return path. Used here as an address, never called.
    fn thalyx_trap_return();
}

/// Layout a stopped thread's kernel stack holds at its saved stack pointer.
#[repr(C)]
struct StoppedContext {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbx: u64,
    rbp: u64,
    resume: u64,
    frame: TrapFrame,
}

const _: () = assert!(core::mem::size_of::<StoppedContext>() == 232);

/// Switches from the current stack to `load`, saving the current stack pointer
/// through `save`.
///
/// # Safety
///
/// `save` must point at storage that outlives the switch, and `load` must be a
/// stack pointer previously produced by this function or by
/// [`prepare_user_thread`]. The caller must hold no lock, because the thread
/// resumed here will not release it, and must have already updated the TSS ring
/// 0 stack, the `syscall` stack and CR3 for the thread being resumed.
#[unsafe(naked)]
pub unsafe extern "sysv64" fn switch_context(save: *mut u64, load: u64) {
    naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    )
}

/// Builds the initial stopped context of a thread that has never run, so that
/// resuming it enters ring 3 at `entry` with `argument` in RDI.
///
/// Every other general-purpose register is zero and the FP state is the
/// bootstrap template, so nothing from the kernel or from another domain is
/// visible in the new thread's initial register file. RFLAGS carries IF and the
/// reserved bit only: IOPL is zero, so port access and `cli` from ring 3 fault.
///
/// # Safety
///
/// `kernel_stack_top` must be the top of a mapped, 16-byte-aligned kernel stack
/// of at least [`core::mem::size_of::<StoppedContext>()`] bytes that no other
/// thread uses.
pub unsafe fn prepare_user_thread(
    kernel_stack_top: u64,
    entry: u64,
    user_stack_top: u64,
    argument: u64,
) -> u64 {
    // The System V ABI has RSP congruent to 8 modulo 16 at a function's first
    // instruction, because the call pushed a return address. The entry point is
    // an ordinary function, so it is entered the same way.
    let user_rsp = user_stack_top - 8;

    let context = StoppedContext {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
        rbx: 0,
        rbp: 0,
        resume: thalyx_trap_return as *const () as u64,
        frame: TrapFrame {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: argument,
            rbp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            vector: 0,
            error_code: 0,
            rip: entry,
            cs: u64::from(gdt::USER_CODE),
            // Bit 1 is reserved and must be set; bit 9 is IF.
            rflags: 0x202,
            rsp: user_rsp,
            ss: u64::from(gdt::USER_DATA),
        },
    };

    let sp = kernel_stack_top - core::mem::size_of::<StoppedContext>() as u64;
    // SAFETY: the caller guarantees the stack is mapped, large enough and
    // unused; `sp` is 8-byte aligned because both operands are.
    unsafe { core::ptr::write(sp as *mut StoppedContext, context) };
    sp
}
