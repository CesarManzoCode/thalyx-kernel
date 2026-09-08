//! Thalyx-Kernel.
//!
//! K1 scope: take the machine from the loader's hand-off to user code running
//! in ring 3 under the kernel's own protection, contain a fault by a user
//! domain without losing the kernel or the other domains, and preempt a domain
//! that never yields. Capabilities, scopes, IPC, endpoints and the supervisor
//! are K2 and are deliberately absent rather than sketched.

#![no_std]
#![no_main]

mod arch;
mod boot;
mod diag;
mod domain;
mod elf;
mod harness;
mod layout;
mod mm;
mod panic;
mod sched;
mod state;
mod sync;
mod syscall;
mod time;
mod trap;

/// Entry point the loader jumps to.
///
/// # Safety
///
/// See [`boot::start`]. The `sysv64` convention is explicit because the loader
/// is compiled for a different target with a different default convention, and
/// the argument register must not depend on either compiler's choice.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn _start(bootinfo_phys: u64) -> ! {
    // SAFETY: only the loader reaches this symbol, and it establishes the state
    // `boot::start` requires before jumping here.
    unsafe { boot::start(bootinfo_phys) }
}
