//! Signals and timers.
//!
//! A signal coalesces: it holds a bit set and a sequence number, not one event
//! per occurrence. That is deliberate and it is a limitation the IPC contract
//! states rather than hides — a consumer woken by a signal has to go and read
//! the source of truth, and anything that must not lose events uses a queue
//! with a reservation instead.
//!
//! The race the sequence number exists for is the classic one: a consumer looks
//! at an empty source, a producer raises the bits, and the consumer then sleeps
//! forever. Registering the waiter and testing the bits happen under the machine
//! lock, and raising takes the same lock, so a raise that arrives between the
//! test and the sleep cannot be lost.

use crate::obj::ScopeId;

/// A coalescing bit set with a sequence.
#[derive(Clone, Copy, Debug)]
pub struct Signal {
    /// Whether the slot is in use.
    pub used: bool,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Scope charged for the object.
    pub owner_scope: ScopeId,
    /// Bits raised and not yet consumed.
    pub bits: u64,
    /// Advances on every raise, so a consumer can tell a repeat from a stale
    /// read.
    pub sequence: u64,
    /// Threads currently waiting.
    pub waiters: u32,
    /// Capability entries naming this signal.
    pub refs: u32,
}

impl Signal {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            generation: 0,
            id: 0,
            owner_scope: 0,
            bits: 0,
            sequence: 0,
            waiters: 0,
            refs: 0,
        }
    }
}

/// A monotonic expiry bound to a signal.
#[derive(Clone, Copy, Debug)]
pub struct Timer {
    /// Whether the slot is in use.
    pub used: bool,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Scope charged for the object.
    pub owner_scope: ScopeId,
    /// Signal raised on expiry.
    pub signal: u16,
    /// Generation that signal had when the timer was bound to it, so a expiry
    /// never raises a recycled object.
    pub signal_generation: u32,
    /// Bits raised on expiry.
    pub bits: u64,
    /// Monotonic deadline while armed.
    pub deadline_ns: u64,
    /// Whether the timer is armed.
    pub armed: bool,
    /// Expiries delivered.
    pub fired: u64,
    /// Capability entries naming this timer.
    pub refs: u32,
}

impl Timer {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            generation: 0,
            id: 0,
            owner_scope: 0,
            signal: 0,
            signal_generation: 0,
            bits: 0,
            deadline_ns: 0,
            armed: false,
            fired: 0,
            refs: 0,
        }
    }
}
