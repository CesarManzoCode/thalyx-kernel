//! Diagnostic plane.
//!
//! This is the third plane of `vault/architecture/observability.md`: traces and
//! counters for debugging and for the K1 gate. It is **not** the control-receipt
//! plane. It reserves nothing, it is not readable through a capability, it can
//! coalesce repeated records, and no operation is refused because it could not
//! be written. Nothing here may be presented as an audited history.
//!
//! Records are one line each:
//!
//! ```text
//! THLX1 <source> <seq> <ns|-> <event> [key=value ...]
//! ```
//!
//! `seq` is a single sequence continued from the loader's, so a gap is
//! detectable. `ns` is monotonic nanoseconds since the kernel established a
//! clock, or `-` before that point.

use core::fmt::Write;

use crate::arch::x86_64::serial::Uart;
use crate::sync::SpinLock;

/// Format version of the record line. Any change to the shape of a line
/// changes this.
pub const FORMAT: &str = "THLX1";

struct Sink {
    uart: Option<Uart>,
    seq: u64,
}

static SINK: SpinLock<Sink> = SpinLock::new(Sink { uart: None, seq: 0 });

/// Binds the diagnostic plane to an already-initialised UART and continues the
/// loader's record sequence.
pub fn init(uart: Uart, first_seq: u64) {
    let mut sink = SINK.lock();
    sink.uart = Some(uart);
    sink.seq = first_seq;
}

/// Emits one record. Prefer the [`event!`](crate::event) macro.
pub fn emit_event(name: &str, fields: core::fmt::Arguments<'_>) {
    let mut sink = SINK.lock();
    let seq = sink.seq;
    sink.seq += 1;
    let Some(mut uart) = sink.uart else { return };
    let _ = write!(uart, "{FORMAT} kernel {seq} ");
    match crate::time::monotonic_ns() {
        Some(ns) => {
            let _ = write!(uart, "{ns}");
        }
        None => uart.write_str("-"),
    }
    let _ = write!(uart, " {name}");
    let _ = write!(uart, " {fields}");
    uart.write_str("\n");
}

/// Emits a record from a context that must not block on the lock, such as a
/// panic after a fault.
///
/// # Safety
///
/// The caller must have stopped every other context that could be emitting,
/// which in K1 means interrupts are masked and no other core exists.
pub unsafe fn emit_event_unlocked(name: &str, fields: core::fmt::Arguments<'_>) {
    // SAFETY: the caller guarantees no concurrent access.
    let sink = unsafe { SINK.get_unchecked() };
    let seq = sink.seq;
    sink.seq += 1;
    let Some(mut uart) = sink.uart else { return };
    let _ = write!(uart, "{FORMAT} kernel {seq} ");
    match crate::time::monotonic_ns() {
        Some(ns) => {
            let _ = write!(uart, "{ns}");
        }
        None => uart.write_str("-"),
    }
    let _ = write!(uart, " {name} {fields}\n");
}

/// Emits one diagnostic record.
#[macro_export]
macro_rules! event {
    ($name:expr) => {
        $crate::diag::emit_event($name, ::core::format_args!(""))
    };
    ($name:expr, $($fields:tt)*) => {
        $crate::diag::emit_event($name, ::core::format_args!($($fields)*))
    };
}
