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

/// Which lock a [`SpinLock`] is, for the run's own accounting.
///
/// One total over every lock in the kernel answered "was a lock the limit"
/// and could not answer "which one": the control lock, the run queues and the
/// wait records are taken in different numbers on different paths, and an IPC
/// round trip takes all three. The classes are what the summary reports
/// separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LockClass {
    /// The machine's control lock.
    Control = 0,
    /// A processor's run queue.
    RunQueue = 1,
    /// A thread's wait record.
    Wait = 2,
    /// Everything else: the diagnostic sink, the window roll.
    Other = 3,
}

/// Number of classes, which is the number of statistics blocks per processor.
const CLASSES: usize = 4;

/// Acquisitions of the kernel's locks, and time-stamp counter cycles spent
/// waiting for them, kept by the processor that did the waiting.
///
/// Retained rather than written: a record per acquisition would cost more than
/// the acquisition and would not survive its own measurement. What a run
/// reports is the total and the worst wait, which is what "the lock was the
/// limit" or "the lock was not the limit" is an argument about.
///
/// One block per processor and class, each on its own cache line. Four
/// machine-wide words did the same sums with one difference: every
/// acquisition on every processor wrote the same line, so counting the lock's
/// traffic was itself a line handed between processors as often as the lock
/// was -- an extra transfer per acquisition, charged to the thing being
/// measured.
#[repr(C, align(64))]
struct LockStats {
    acquisitions: AtomicU64,
    contended_waits: AtomicU64,
    contended_cycles: AtomicU64,
    worst_wait_cycles: AtomicU64,
}

impl LockStats {
    const fn new() -> Self {
        Self {
            acquisitions: AtomicU64::new(0),
            contended_waits: AtomicU64::new(0),
            contended_cycles: AtomicU64::new(0),
            worst_wait_cycles: AtomicU64::new(0),
        }
    }
}

static STATS: [[LockStats; CLASSES]; crate::limits::MAX_CPUS] =
    [const { [const { LockStats::new() }; CLASSES] }; crate::limits::MAX_CPUS];

/// This processor's block for one class. Every kernel path that can take a
/// lock runs after its processor installed its per-processor block, on the
/// bootstrap path and on the application processors' alike, which is what the
/// spin loop's invalidation service already relies on.
#[inline]
fn stats(class: LockClass) -> &'static LockStats {
    &STATS[crate::percpu::index()][class as usize]
}

/// Acquisitions, acquisitions that had to wait, cycles spent waiting and the
/// longest single wait of one class of lock, summed over the processors.
#[must_use]
pub fn contention(class: LockClass) -> (u64, u64, u64, u64) {
    let mut totals = (0, 0, 0, 0u64);
    for blocks in &STATS {
        let stats = &blocks[class as usize];
        totals.0 += stats.acquisitions.load(Ordering::Relaxed);
        totals.1 += stats.contended_waits.load(Ordering::Relaxed);
        totals.2 += stats.contended_cycles.load(Ordering::Relaxed);
        totals.3 = totals
            .3
            .max(stats.worst_wait_cycles.load(Ordering::Relaxed));
    }
    totals
}

/// A mutual-exclusion cell that never blocks and never yields while held.
pub struct SpinLock<T> {
    /// The next ticket to hand out.
    next: AtomicU32,
    /// The ticket now allowed in.
    serving: AtomicU32,
    /// Which statistics an acquisition is counted under.
    class: LockClass,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialised by the ticket, so a `&SpinLock<T>`
// shared between contexts can only ever hand out one `&mut T` at a time.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: the lock adds no thread affinity of its own.
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Creates an unlocked cell counted under [`LockClass::Other`].
    pub const fn new(value: T) -> Self {
        Self::of_class(value, LockClass::Other)
    }

    /// Creates an unlocked cell counted under `class`.
    pub const fn of_class(value: T, class: LockClass) -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
            class,
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
        let stats = stats(self.class);
        stats.acquisitions.fetch_add(1, Ordering::Relaxed);
        if self.serving.load(Ordering::Acquire) == ticket {
            return SpinGuard { lock: self };
        }
        let began = crate::arch::x86_64::cpu::rdtsc();
        let mut turn = 0u32;
        while self.serving.load(Ordering::Acquire) != ticket {
            // The invalidation service is what keeps a processor waiting for
            // this lock from being the reason another processor waits for an
            // acknowledgement, and it only has to happen often enough for
            // that. Doing it on every turn of the loop meant every waiting
            // processor read the invalidation counter and its own record
            // millions of times a second -- lines written by every unmap in
            // the machine -- which made the wait itself the traffic that made
            // the wait longer. Measured: eight thousand million cycles spent
            // waiting for the control lock in one campaign.
            if turn % 64 == 0 {
                crate::tlb::refresh_local();
            }
            turn = turn.wrapping_add(1);
            core::hint::spin_loop();
        }
        let waited = crate::arch::x86_64::cpu::rdtsc().wrapping_sub(began);
        stats.contended_waits.fetch_add(1, Ordering::Relaxed);
        stats.contended_cycles.fetch_add(waited, Ordering::Relaxed);
        stats.worst_wait_cycles.fetch_max(waited, Ordering::Relaxed);
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
