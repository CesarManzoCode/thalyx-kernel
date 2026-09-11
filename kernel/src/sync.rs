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
use core::sync::atomic::{AtomicU32, Ordering};

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
        while self.serving.load(Ordering::Acquire) != ticket {
            crate::tlb::refresh_local();
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
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
