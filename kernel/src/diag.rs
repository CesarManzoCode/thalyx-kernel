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
//!
//! The plane also counts what it costs: records, bytes, and the processor time
//! spent writing them. Writing is synchronous and polled, so that time is taken
//! out of whatever was running, often with the machine lock held. On an
//! emulator whose port I/O is a function call that is small; under KVM every
//! byte is a trip out of the guest, and the first KVM runs spent most of their
//! time here. A cost that decides how long a run takes is a cost the run should
//! report, so [`summary`] does.

use core::fmt::Write;

use crate::arch::x86_64::cpu;
use crate::arch::x86_64::serial::Uart;
use crate::sync::SpinLock;

/// Format version of the record line. Any change to the shape of a line
/// changes this.
pub const FORMAT: &str = "THLX1";

struct Sink {
    uart: Option<Uart>,
    seq: u64,
    /// Sequence number of the first record this kernel wrote.
    first: u64,
    /// Bytes written to the port, line terminators included.
    bytes: u64,
    /// Time-stamp-counter cycles spent formatting and writing records.
    cycles: u64,
}

static SINK: SpinLock<Sink> = SpinLock::new(Sink {
    uart: None,
    seq: 0,
    first: 0,
    bytes: 0,
    cycles: 0,
});

/// A writer that counts what it writes, so the plane's cost is the bytes it
/// actually sent and not an estimate from the format string.
struct Counted {
    uart: Uart,
    bytes: u64,
}

impl Write for Counted {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // `Uart::write_str` expands every newline to two bytes.
        self.bytes += s.len() as u64 + s.bytes().filter(|&byte| byte == b'\n').count() as u64;
        self.uart.write_str(s);
        Ok(())
    }
}

/// Binds the diagnostic plane to an already-initialised UART and continues the
/// loader's record sequence.
pub fn init(uart: Uart, first_seq: u64) {
    let mut sink = SINK.lock();
    sink.uart = Some(uart);
    sink.seq = first_seq;
    sink.first = first_seq;
}

/// Emits one record. Prefer the [`event!`](crate::event) macro.
pub fn emit_event(name: &str, fields: core::fmt::Arguments<'_>) {
    let mut sink = SINK.lock();
    let seq = sink.seq;
    sink.seq += 1;
    let Some(uart) = sink.uart else { return };
    let started = cpu::rdtsc();
    let mut out = Counted { uart, bytes: 0 };
    let _ = write!(out, "{FORMAT} kernel {seq} ");
    match crate::time::monotonic_ns() {
        Some(ns) => {
            let _ = write!(out, "{ns}");
        }
        None => {
            let _ = out.write_str("-");
        }
    }
    let _ = write!(out, " {name}");
    let _ = write!(out, " {fields}");
    let _ = out.write_str("\n");
    sink.bytes += out.bytes;
    sink.cycles += cpu::rdtsc().wrapping_sub(started);
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

/// What the plane has cost so far: records, bytes, and cycles spent writing.
#[must_use]
pub fn stats() -> (u64, u64, u64) {
    let sink = SINK.lock();
    (sink.seq - sink.first, sink.bytes, sink.cycles)
}

/// States what the plane cost over the run, in the kernel's own counts.
///
/// The record itself is not included in the numbers it reports.
pub fn summary() {
    let (records, bytes, cycles) = stats();
    let hz = crate::time::hz();
    let ns = if hz == 0 {
        0
    } else {
        ((u128::from(cycles) * 1_000_000_000u128) / u128::from(hz)) as u64
    };
    crate::event!(
        "diag.summary",
        "records={records} bytes={bytes} write_cycles={cycles} write_ns={ns} tsc_hz={hz} \
         port=com1 mode=polled_synchronous"
    );
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
