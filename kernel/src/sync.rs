//! Kernel synchronisation primitives.
//!
//! Every kernel path executes with maskable interrupts masked: exception and
//! interrupt gates clear IF, `IA32_FMASK` clears it on `syscall`, and the only
//! places that set it are the idle loops, which hold no lock. So a lock is
//! never contended by a nested context on the same processor, and it is only
//! ever contended by another processor — one that is running, not one that is
//! descheduled, which is what makes spinning the right shape of wait.
//!
//! Lock order, from `vault/architecture/concurrency.md`: admission metadata and
//! trees, then resource accounts from ancestor to descendant, then objects by
//! increasing identifier, then local queues.
//!
//! One rule of the concurrency contract is enforced here rather than only
//! documented: no lock may be held while waiting for another processor's
//! acknowledgement. The spin loop below services pending invalidations. A
//! processor waiting for a lock therefore keeps answering the processor that is
//! waiting for it, so the two cannot wait on each other. A complete invalidation
//! is correct at any point, which is what makes it safe to do from inside a
//! wait for an unrelated lock.
//!
//! The lock is a ticket lock: processors acquire it in the order they asked.
//! Until K6 it was a test-and-set flag, which a processor releasing and
//! re-acquiring the lock in a tight loop wins every time, since the line is
//! hot in its cache and cold in every other. K6's closure benchmark found the
//! consequence: a supervisor spinning on `SCOPE_RETIRE` -- a lock acquisition
//! or three per call -- kept the processor whose switch away from a stopped
//! thread the retirement was waiting for from ever taking the lock to do it,
//! for milliseconds at a time, until the supervisor was preempted. Fairness
//! costs one more atomic per release and removes the starvation.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Acquisitions of the control lock, and time-stamp counter cycles spent
/// waiting for it.
///
/// Retained rather than written: a record per acquisition would cost more than
/// the acquisition and would not survive its own measurement. What a run
/// reports is the total and the worst wait, which is what "the lock was the
/// limit" or "the lock was not the limit" is an argument about.
static CONTENDED_WAITS: AtomicU64 = AtomicU64::new(0);
static CONTENDED_CYCLES: AtomicU64 = AtomicU64::new(0);
static WORST_WAIT_CYCLES: AtomicU64 = AtomicU64::new(0);
static ACQUISITIONS: AtomicU64 = AtomicU64::new(0);

/// Acquisitions, acquisitions that had to wait, cycles spent waiting and the
/// longest single wait.
#[must_use]
pub fn contention() -> (u64, u64, u64, u64) {
    (
        ACQUISITIONS.load(Ordering::Relaxed),
        CONTENDED_WAITS.load(Ordering::Relaxed),
        CONTENDED_CYCLES.load(Ordering::Relaxed),
        WORST_WAIT_CYCLES.load(Ordering::Relaxed),
    )
}

/// A mutual-exclusion cell that never blocks and never yields while held.
pub struct SpinLock<T> {
    /// The next ticket to hand out.
    next: AtomicU32,
    /// The ticket now allowed in.
    serving: AtomicU32,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialised by the ticket, so a `&SpinLock<T>`
// shared between contexts can only ever hand out one `&mut T` at a time.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: the lock adds no thread affinity of its own.
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Creates an unlocked cell.
    pub const fn new(value: T) -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquires the lock, spinning until its turn comes.
    ///
    /// The returned guard must be dropped before any context switch: switching
    /// with a kernel lock held would let the next thread deadlock against a
    /// holder that is no longer running.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
        if self.serving.load(Ordering::Acquire) == ticket {
            return SpinGuard { lock: self };
        }
        let began = crate::arch::x86_64::cpu::rdtsc();
        while self.serving.load(Ordering::Acquire) != ticket {
            crate::tlb::refresh_local();
            core::hint::spin_loop();
        }
        let waited = crate::arch::x86_64::cpu::rdtsc().wrapping_sub(began);
        CONTENDED_WAITS.fetch_add(1, Ordering::Relaxed);
        CONTENDED_CYCLES.fetch_add(waited, Ordering::Relaxed);
        WORST_WAIT_CYCLES.fetch_max(waited, Ordering::Relaxed);
        SpinGuard { lock: self }
    }

    /// Acquires the lock only if nobody holds it or waits for it.
    ///
    /// A ticket lock has no notion of "free" beyond "the next ticket is the
    /// one being served"; taking that ticket atomically is the acquisition,
    /// and losing the race is the refusal. Used where a processor may not
    /// wait for another -- an idle processor taking work from a busy one's
    /// queue -- because waiting is the one thing two such processors must not
    /// do to each other.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        let serving = self.serving.load(Ordering::Acquire);
        if self
            .next
            .compare_exchange(
                serving,
                serving.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            Some(SpinGuard { lock: self })
        } else {
            None
        }
    }

    /// Raw pointer to the protected value.
    ///
    /// Used where a reference derived from a guard would outlive the guard: the
    /// context switch has to release the lock before switching stacks, yet
    /// still needs somewhere to store the outgoing thread's stack pointer. The
    /// pointer is derived from the static itself rather than from a guard, so it
    /// carries no borrow that the release would invalidate.
    pub const fn as_mut_ptr(&self) -> *mut T {
        self.value.get()
    }

    /// Returns a mutable reference without locking.
    ///
    /// # Safety
    ///
    /// The caller must know that no other context can reach this cell: the only
    /// use is the panic path, which has already masked interrupts and must not
    /// hang on a lock a faulted context still holds. It is not a guarantee that
    /// no other processor is running; the panic path accepts that and writes a
    /// bounded record rather than hanging, which is the trade the concurrency
    /// contract asks of a failure path.
    // `mut_from_ref` is the right lint in general: handing out `&mut` from `&`
    // hides aliasing behind a safe-looking signature. Here the signature is
    // already `unsafe` and the contract above is what rules aliasing out, which
    // is the same bargain `UnsafeCell` itself makes.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_unchecked(&self) -> &mut T {
        // SAFETY: the caller guarantees exclusivity.
        unsafe { &mut *self.value.get() }
    }
}

/// Guard returned by [`SpinLock::lock`].
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: holding the guard means holding the lock.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: holding the guard means holding the lock exclusively.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.serving.fetch_add(1, Ordering::Release);
    }
}
