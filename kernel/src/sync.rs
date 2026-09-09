//! Kernel synchronisation primitives.
//!
//! K1 runs on one core and every kernel path executes with maskable interrupts
//! masked: exception and interrupt gates clear IF, `IA32_FMASK` clears it on
//! `syscall`, and the idle path never sets it. A lock therefore cannot be
//! contended in K1. It is still a real lock rather than an `UnsafeCell` with a
//! comment, because the ordering discipline the SMP work in K3 has to preserve
//! is the one expressed here, and because it turns a re-entrancy bug into a
//! detectable hang instead of silent corruption.
//!
//! Lock order, from `vault/architecture/concurrency.md`: admission metadata and
//! trees, then resource accounts from ancestor to descendant, then objects by
//! increasing identifier, then local queues. K1 instantiates only the first and
//! third levels.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// A mutual-exclusion cell that never blocks and never yields while held.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: access to `value` is serialised by `locked`, so a `&SpinLock<T>`
// shared between contexts can only ever hand out one `&mut T` at a time.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: the lock adds no thread affinity of its own.
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Creates an unlocked cell.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquires the lock, spinning until it is free.
    ///
    /// The returned guard must be dropped before any context switch: switching
    /// with a kernel lock held would let the next thread deadlock against a
    /// holder that is no longer running.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                core::hint::spin_loop();
            }
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
    /// use in K1 is the panic path, which has already stopped scheduling and
    /// must not hang on a lock a faulted context still holds.
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
        self.lock.locked.store(false, Ordering::Release);
    }
}
