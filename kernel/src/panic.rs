//! Panic path.
//!
//! A kernel exception is a system failure, not an ordinary error: the state
//! that would have to be trusted to continue is exactly the state whose
//! invariants are in question. So the panic path records what it can within
//! bounds and stops. It does not take the kernel lock, because the context that
//! panicked may be holding it.

use crate::arch::x86_64::cpu;
use crate::harness;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: interrupts are masked first and K1 is uniprocessor, so no other
    // context can be emitting on the diagnostic plane.
    unsafe {
        cpu::disable_interrupts();
        let location = info.location();
        match location {
            Some(location) => crate::diag::emit_event_unlocked(
                "kernel.panic",
                format_args!(
                    "file={} line={} column={} message=\"{}\"",
                    location.file(),
                    location.line(),
                    location.column(),
                    info.message()
                ),
            ),
            None => crate::diag::emit_event_unlocked(
                "kernel.panic",
                format_args!("file=? line=0 column=0 message=\"{}\"", info.message()),
            ),
        }
    }
    harness::finish(harness::STATUS_PANIC)
}
