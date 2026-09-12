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
//!
//! Records come in two classes. A **trace** record says that one operation or
//! one object happened: an admission, a mapping, a derivation, a migration, a
//! scope created and retired. A **summary** record says what the kernel is,
//! what it found at boot, that something failed, what a program noted, and
//! what everything added up to at the end. The K1 to K5 gates are decided from
//! both, and a package that says nothing keeps both. A package whose
//! supervisor module carries `TRACE_OFF` asks for the summaries alone: under
//! KVM a trace record costs about as much as a thousand of the operations it
//! describes, so a measurement taken with tracing on is a measurement of the
//! tracing. Withheld records are counted, not lost silently, and the summary
//! states the count -- the contract's condition for a plane that may drop
//! events is that it declares doing so.

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// Whether trace records are written. Summaries always are.
static TRACE: AtomicBool = AtomicBool::new(true);
/// Trace records withheld while tracing was off, counted by the processor
/// that withheld them.
///
/// One word for the machine was a line every processor wrote on every
/// withheld record, and the hot paths withhold four or five per IPC round
/// trip, most of them with the control lock held: the count of records not
/// written was itself a cache line handed between processors as often as
/// the lock was.
#[repr(C, align(64))]
struct Withheld(AtomicU64);

static WITHHELD: [Withheld; crate::limits::MAX_CPUS] =
    [const { Withheld(AtomicU64::new(0)) }; crate::limits::MAX_CPUS];

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

/// Turns trace records on or off. The change is itself recorded, before it
/// takes effect, so the log says where its own coverage changed.
pub fn set_trace(enabled: bool, reason: &str) {
    crate::event!(
        "diag.trace",
        "per_operation={} reason={reason}",
        if enabled { "on" } else { "off" }
    );
    TRACE.store(enabled, Ordering::Release);
}

/// Whether trace records are being written.
#[must_use]
pub fn trace_enabled() -> bool {
    TRACE.load(Ordering::Relaxed)
}

/// Counts a trace record that was not written. The [`trace!`](crate::trace)
/// macro calls this instead of building the record's arguments.
pub fn withhold() {
    // Every path that can trace runs after its processor installed its
    // per-processor block: a written record takes the sink's lock, whose
    // statistics already read the block, so a withheld one may too.
    WITHHELD[crate::percpu::index()]
        .0
        .fetch_add(1, Ordering::Relaxed);
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
    let withheld: u64 = WITHHELD
        .iter()
        .map(|cpu| cpu.0.load(Ordering::Relaxed))
        .sum();
    crate::event!(
        "diag.summary",
        "records={records} bytes={bytes} write_cycles={cycles} write_ns={ns} tsc_hz={hz} \
         port=com1 mode=polled_synchronous trace={} trace_withheld={withheld}",
        if trace_enabled() { "on" } else { "off" }
    );
}

/// Emits one summary record: always written.
#[macro_export]
macro_rules! event {
    ($name:expr) => {
        $crate::diag::emit_event($name, ::core::format_args!(""))
    };
    ($name:expr, $($fields:tt)*) => {
        $crate::diag::emit_event($name, ::core::format_args!($($fields)*))
    };
}

/// Emits one trace record: written while tracing is on, counted while it is
/// off. The arguments are not evaluated when the record is withheld.
#[macro_export]
macro_rules! trace {
    ($name:expr) => {
        $crate::trace!($name, "")
    };
    ($name:expr, $($fields:tt)*) => {
        if $crate::diag::trace_enabled() {
            $crate::diag::emit_event($name, ::core::format_args!($($fields)*))
        } else {
            $crate::diag::withhold()
        }
    };
}
