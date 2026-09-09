//! The control-receipt plane.
//!
//! This is the first of the three planes of the observability contract, and it
//! is not the diagnostic plane the K1 gate reads. A receipt is reserved *before*
//! a covered operation is admitted; if no cell can be reserved, the operation is
//! refused before it has an effect. That is the whole point of an audited
//! profile: coverage is bought in advance, so a claim that an operation was
//! recorded is a claim about a reservation and not about a hopeful write to a
//! ring buffer.
//!
//! The log does not overwrite. A full log stops covered admissions, which is a
//! visible refusal, rather than silently forgetting the oldest receipt, which
//! would be a false history. Cells are freed by a reader acknowledging them.
//!
//! A fixed number of cells is reserved for closing authority. Saturating the log
//! with ordinary traffic must never prevent a fence or a retirement from being
//! recorded, and if even those cells run out the closing receipts coalesce into
//! a counter with an explicit range instead of pretending each one was written.

use thalyx_abi::generated::{ReceiptRecord, receipt_kind};

use crate::limits::{CONTROL_LOG_CAPACITY, CONTROL_LOG_RESERVED};
use crate::obj::ScopeId;

/// Schema version stamped on every receipt.
pub const RECEIPT_SCHEMA: u32 = 1;

/// True for the receipt kinds that may occupy a reserved cell.
#[must_use]
pub const fn is_closing(kind: u32) -> bool {
    kind == receipt_kind::FENCE || kind == receipt_kind::RETIRE
}

/// A bounded ring of control receipts.
pub struct ControlLog {
    /// Whether the slot is in use.
    pub used: bool,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Scope charged for the ring.
    pub owner_scope: ScopeId,
    /// Records, oldest at `head`.
    pub records: [ReceiptRecord; CONTROL_LOG_CAPACITY],
    /// Index of the oldest live record.
    pub head: usize,
    /// Live records.
    pub count: usize,
    /// Sequence the next receipt will carry.
    pub next_sequence: u64,
    /// Sequence of the oldest live record.
    pub oldest_sequence: u64,
    /// Ordinary receipts that could not be written.
    pub lost: u32,
    /// Closing receipts folded into a counter because even the reserved cells
    /// were full. Reported, never hidden.
    pub coalesced: u32,
    /// Cells reserved by an admission that has not yet committed its receipt.
    pub pending_reservations: u32,
    /// Capability entries naming this log.
    pub refs: u32,
}

impl ControlLog {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            generation: 0,
            id: 0,
            owner_scope: 0,
            records: [ReceiptRecord::zeroed(); CONTROL_LOG_CAPACITY],
            head: 0,
            count: 0,
            next_sequence: 1,
            oldest_sequence: 1,
            lost: 0,
            coalesced: 0,
            pending_reservations: 0,
            refs: 0,
        }
    }

    /// Cells ordinary traffic may occupy.
    #[must_use]
    pub const fn ordinary_capacity(&self) -> usize {
        CONTROL_LOG_CAPACITY - CONTROL_LOG_RESERVED
    }

    /// Whether a covered ordinary operation can reserve a cell right now.
    #[must_use]
    pub fn can_reserve(&self) -> bool {
        self.count + (self.pending_reservations as usize) < self.ordinary_capacity()
    }

    /// Reserves a cell for a covered operation, refusing when none is left.
    pub fn reserve(&mut self) -> bool {
        if !self.can_reserve() {
            return false;
        }
        self.pending_reservations += 1;
        true
    }

    /// Returns a reservation an operation did not use, because it was refused
    /// on a later check.
    pub fn release_reservation(&mut self) {
        self.pending_reservations = self.pending_reservations.saturating_sub(1);
    }

    fn push(&mut self, record: ReceiptRecord) -> bool {
        if self.count >= CONTROL_LOG_CAPACITY {
            return false;
        }
        let index = (self.head + self.count) % CONTROL_LOG_CAPACITY;
        self.records[index] = record;
        if self.count == 0 {
            self.oldest_sequence = record.sequence;
        }
        self.count += 1;
        true
    }

    /// Writes a receipt that a previous [`ControlLog::reserve`] paid for.
    pub fn commit_reserved(&mut self, record: ReceiptRecord) -> u64 {
        self.pending_reservations = self.pending_reservations.saturating_sub(1);
        self.write(record)
    }

    /// Writes a receipt, honouring the reserved cells.
    ///
    /// Returns the sequence written, or zero when the receipt could not be
    /// written at all; in that case the loss is counted so a reader is told its
    /// coverage has a hole rather than being left to assume completeness.
    pub fn write(&mut self, mut record: ReceiptRecord) -> u64 {
        let closing = is_closing(record.kind);
        let limit = if closing {
            CONTROL_LOG_CAPACITY
        } else {
            self.ordinary_capacity()
        };
        if self.count >= limit {
            if closing {
                self.coalesced = self.coalesced.saturating_add(1);
            } else {
                self.lost = self.lost.saturating_add(1);
            }
            return 0;
        }
        record.schema = RECEIPT_SCHEMA;
        record.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        if self.push(record) {
            record.sequence
        } else {
            self.lost = self.lost.saturating_add(1);
            0
        }
    }

    /// Copies up to `out.len()` records, oldest first, without consuming them.
    pub fn read(&self, out: &mut [ReceiptRecord]) -> usize {
        let take = out.len().min(self.count);
        for (offset, slot) in out.iter_mut().enumerate().take(take) {
            *slot = self.records[(self.head + offset) % CONTROL_LOG_CAPACITY];
        }
        take
    }

    /// Drops every record up to and including `sequence`.
    pub fn acknowledge(&mut self, sequence: u64) -> usize {
        let mut dropped = 0;
        while self.count > 0 {
            let record = self.records[self.head];
            if record.sequence > sequence {
                break;
            }
            self.head = (self.head + 1) % CONTROL_LOG_CAPACITY;
            self.count -= 1;
            dropped += 1;
        }
        self.oldest_sequence = if self.count == 0 {
            self.next_sequence
        } else {
            self.records[self.head].sequence
        };
        dropped
    }
}
