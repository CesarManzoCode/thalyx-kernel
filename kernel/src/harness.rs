//! K1 test-harness scaffolding.
//!
//! The kernel has to end the run so the harness can collect the log. A real
//! system stops because a supervisor decided to, and K2 gives it the authority
//! to do that. Until then this writes to the emulator's debug-exit port when the
//! image is run with that device, and halts otherwise. It is the only place in
//! the kernel that knows anything about how it is being observed, and it is
//! removed when the supervisor exists.

use crate::arch::x86_64::cpu;

/// I/O port of the QEMU `isa-debug-exit` device, as configured by the run
/// command. Writing `v` makes QEMU exit with status `(v << 1) | 1`.
pub const DEBUG_EXIT_PORT: u16 = 0xF4;

/// The run reached its terminal state with no domain left to schedule.
pub const STATUS_COMPLETE: u8 = 0x10;
/// The kernel panicked.
pub const STATUS_PANIC: u8 = 0x11;

/// Ends the run and never returns.
pub fn finish(status: u8) -> ! {
    // SAFETY: the port is either the configured debug-exit device, in which
    // case the machine stops here, or unimplemented, in which case the write is
    // discarded and the halt loop below is the terminal state.
    unsafe {
        cpu::disable_interrupts();
        cpu::outb(DEBUG_EXIT_PORT, status);
    }
    cpu::halt_forever()
}
