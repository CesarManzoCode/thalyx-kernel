//! Thalyx-Kernel.
//!
//! K1 took the machine from the loader's hand-off to user code running in ring
//! 3 under the kernel's own protection, contained a fault by a user domain
//! without losing the kernel or the other domains, and preempted a domain that
//! never yields.
//!
//! K2 adds what turns that into a system with authority: an object substrate
//! with generational handles, a grant tree that only ever attenuates, a scope
//! tree that pays for everything and can be fenced, drained and retired, copied
//! IPC with facets and work tickets, effect admission, signals and timers, a
//! control-receipt plane, and a supervisor that receives its authority as an
//! explicit set of boot capabilities and builds the rest of the system through
//! the interface. SMP, devices, DMA and durable state remain absent rather than
//! sketched.

#![no_std]
#![no_main]

mod api;
mod arch;
mod boot;
mod ctrl;
mod diag;
mod domain;
mod elf;
mod events;
mod harness;
mod ipc;
mod k2boot;
mod layout;
mod limits;
mod memobj;
mod mm;
mod obj;
mod panic;
mod sched;
mod scope;
mod state;
mod sync;
mod syscall;
mod time;
mod trap;
mod ucopy;

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
