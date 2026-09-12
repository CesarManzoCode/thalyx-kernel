//! Monotonic time.
//!
//! One clock serves every processor: the time-stamp counter, with one epoch and
//! one frequency established on the bootstrap processor and read from wherever
//! the caller happens to be. The platform contract requires that reading to be
//! monotonic *between* processors, and an unsynchronised counter would not be,
//! so the property is checked rather than assumed. [`observe`] publishes every
//! reading into one high-water mark and counts the readings that came back
//! lower than one already published on another processor. A run that reports
//! zero such readings has evidence for the property over the interleavings it
//! actually executed; it is not a proof about the hardware, and the counter is
//! reported either way.
//!
//! The frequency is measured against the PIT rather than assumed, and the
//! conversion to nanoseconds is overflow-checked.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::cpu;
use crate::limits::MAX_CPUS;

/// Which hardware the monotonic clock reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// No clock established yet.
    None,
    /// The time-stamp counter, frequency measured against the PIT. The CPU
    /// advertises invariant TSC.
    TscInvariant,
    /// The time-stamp counter without an invariant-TSC advertisement. Usable on
    /// one core under an emulator; not a basis for any frequency-stability
    /// claim.
    TscMeasured,
}

impl Source {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Source::None => "none",
            Source::TscInvariant => "tsc_invariant",
            Source::TscMeasured => "tsc_measured",
        }
    }
}

static EPOCH_TSC: AtomicU64 = AtomicU64::new(0);
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static SOURCE: AtomicU64 = AtomicU64::new(0);
/// Newest reading each processor has published, and how many it has taken.
///
/// One line per processor rather than one for the machine. The check is the
/// same -- a reading is compared against what another processor published
/// before it was taken -- but a high-water mark every processor writes on
/// every reading is a cache line four processors fight over thousands of
/// times a second, and the scheduler reads the clock on every decision. Here
/// each processor writes only its own line and reads the others'.
static PUBLISHED_NS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static READINGS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static REGRESSIONS: AtomicU64 = AtomicU64::new(0);
static WORST_REGRESSION_NS: AtomicU64 = AtomicU64::new(0);

/// Establishes the clock from a measured TSC frequency.
pub fn init(tsc_hz: u64, invariant: bool) {
    TSC_HZ.store(tsc_hz, Ordering::Relaxed);
    EPOCH_TSC.store(cpu::rdtsc(), Ordering::Relaxed);
    SOURCE.store(if invariant { 1 } else { 2 }, Ordering::Release);
}

/// The clock source currently in use.
#[must_use]
pub fn source() -> Source {
    match SOURCE.load(Ordering::Acquire) {
        1 => Source::TscInvariant,
        2 => Source::TscMeasured,
        _ => Source::None,
    }
}

/// Measured frequency of the clock source in hertz, or zero before
/// calibration.
#[must_use]
pub fn hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// Nanoseconds since the clock was established, or `None` before that.
#[must_use]
pub fn monotonic_ns() -> Option<u64> {
    if source() == Source::None {
        return None;
    }
    let hz = TSC_HZ.load(Ordering::Relaxed);
    if hz == 0 {
        return None;
    }
    let delta = cpu::rdtsc().wrapping_sub(EPOCH_TSC.load(Ordering::Relaxed));
    // 128-bit intermediate: a 64-bit product overflows after a few seconds at
    // gigahertz frequencies, which is well inside a K1 run.
    Some(((u128::from(delta) * 1_000_000_000u128) / u128::from(hz)) as u64)
}

/// Publishes one reading and reports whether it went backwards relative to a
/// reading another processor had already published *before this one was
/// taken*.
///
/// The check is the cross-processor half of the clock contract, and the order
/// of the two lines below is the whole of it. The published value is read
/// first, with acquire, and the counter second: a value seen by that load was
/// stored before it, so the reading that produced it is ordered before the
/// reading taken here, and a smaller result is a counter that went backwards
/// between two processors. Reading the counter first and comparing against
/// whatever is published afterwards compares two readings that nothing orders
/// -- two processors observing at the same instant, in which case the lower of
/// the two is not a regression at all, it is concurrency. That is what a
/// scheduler which no longer serialises its clock reads through one lock made
/// visible: hundreds of thousands of "regressions" a run, none of them a
/// violation of anything.
pub fn observe() -> u64 {
    let me = crate::percpu::index().min(MAX_CPUS - 1);
    let mut published = 0u64;
    for slot in &PUBLISHED_NS {
        published = published.max(slot.load(Ordering::Acquire));
    }
    let Some(now) = monotonic_ns() else {
        return 0;
    };
    READINGS[me].store(READINGS[me].load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    if published > now {
        REGRESSIONS.fetch_add(1, Ordering::Relaxed);
        WORST_REGRESSION_NS.fetch_max(published - now, Ordering::AcqRel);
    }
    // Only this processor writes this slot, and only ever forwards.
    if PUBLISHED_NS[me].load(Ordering::Relaxed) < now {
        PUBLISHED_NS[me].store(now, Ordering::Release);
    }
    now
}

/// Readings published, readings that went backwards, and the worst backward
/// step in nanoseconds.
#[must_use]
pub fn monotonicity() -> (u64, u64, u64) {
    (
        READINGS
            .iter()
            .map(|slot| slot.load(Ordering::Relaxed))
            .sum(),
        REGRESSIONS.load(Ordering::Relaxed),
        WORST_REGRESSION_NS.load(Ordering::Relaxed),
    )
}
