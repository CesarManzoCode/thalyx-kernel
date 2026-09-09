//! Monotonic time.
//!
//! K1 is uniprocessor, so the cross-core monotonicity the clock contract
//! requires is not exercised here and is not claimed. What is claimed is
//! narrower: on this single core the source is monotonic, its frequency was
//! measured against the PIT rather than assumed, and the conversion to
//! nanoseconds is overflow-checked.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::cpu;

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
