//! `SYSCALL` configuration and entry.
//!
//! Entry uses the `syscall` instruction, as the ABI contract specifies. Return
//! uses `sysretq` when the frame still describes the return `syscall` set up,
//! and `iretq` otherwise. The guards are in the entry stub in
//! `arch::x86_64::trap`, and they are the reason the fast path is safe rather
//! than merely fast: the selectors must be the two `IA32_STAR` makes the
//! instruction return to, the flags must carry neither RF nor VM, and the
//! return address must be canonical -- on Intel parts `sysretq` with a
//! non-canonical RIP faults *in ring 0*, which is how a user-chosen address
//! becomes a kernel exception. A frame that fails any of those leaves through
//! `iretq`, which checks everything the processor can check.

use super::cpu;
use super::gdt;
use super::trap::{TrapFrame, thalyx_syscall_entry};

/// RFLAGS bits cleared on `syscall` entry through `IA32_FMASK`.
///
/// IF so the kernel starts with interrupts masked; DF because System V requires
/// it clear; TF and RF so a user debug state cannot single-step kernel code;
/// IOPL and NT because ring 3 must not carry them into ring 0; AC so a user
/// alignment-check setting does not change kernel behaviour.
pub const SYSCALL_FLAG_MASK: u64 = (1 << 8)   // TF
    | (1 << 9)   // IF
    | (1 << 10)  // DF
    | (3 << 12)  // IOPL
    | (1 << 14)  // NT
    | (1 << 16)  // RF
    | (1 << 18); // AC

/// Value written to `IA32_STAR`.
#[must_use]
pub const fn star_value() -> u64 {
    (gdt::STAR_SYSRET_BASE << 48) | (gdt::STAR_SYSCALL_BASE << 32)
}

/// Enables `syscall` and points it at the kernel entry.
///
/// # Safety
///
/// Must run once during bootstrap, after the GDT is installed, after the CPU
/// feature gate confirmed SYSCALL support, and before any user code exists.
pub unsafe fn init() {
    // SAFETY: bootstrap path; the feature gate has confirmed the MSRs exist.
    unsafe {
        cpu::wrmsr(cpu::MSR_EFER, cpu::rdmsr(cpu::MSR_EFER) | cpu::EFER_SCE);
        cpu::wrmsr(cpu::MSR_STAR, star_value());
        cpu::wrmsr(cpu::MSR_LSTAR, thalyx_syscall_entry as *const () as u64);
        cpu::wrmsr(cpu::MSR_FMASK, SYSCALL_FLAG_MASK);
    }
}

/// Value currently in `IA32_LSTAR`.
#[must_use]
pub fn lstar() -> u64 {
    // SAFETY: `IA32_LSTAR` exists on every CPU that passed the feature gate.
    unsafe { cpu::rdmsr(cpu::MSR_LSTAR) }
}

/// Called from the `syscall` entry stub with interrupts masked.
pub(crate) extern "sysv64" fn syscall_dispatch(frame: *mut TrapFrame) {
    // SAFETY: the entry stub built the frame on the current thread's kernel
    // stack and passes a pointer to it; it is exclusively owned here.
    let frame = unsafe { &mut *frame };
    crate::syscall::handle(frame);
}
