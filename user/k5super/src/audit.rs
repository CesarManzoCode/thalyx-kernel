//! The control plane, drained.
//!
//! Every admission the kernel makes writes a control receipt, and a full log
//! does not lose them: it stops the kernel admitting the work they would cover.
//! K4 established that on purpose, as the honest answer to a lost control
//! plane, and K5 inherits it -- so a supervisor that never read the log would
//! stop its own run.
//!
//! It did. The first version of the `surface` stage had no auditor, the log
//! filled after fifty-six admissions, and `ENDPOINT_CALL` began answering
//! `LIMIT_EXHAUSTED` two thirds of the way through the vertical. Nothing about
//! that is a defect of the kernel; the defect was a supervisor that had not
//! been given the job.
//!
//! The receipts are also the port's independent account of what happened. A
//! work says it published; the log says the kernel admitted an effect on an
//! invocation whose origin it stamped. The second is the one a gate reads.

use thalyx_abi::receipt_kind;
use thalyx_user_rt::k2;

use crate::note;

/// What the auditor has seen.
#[derive(Default)]
pub struct Audit {
    /// Receipts read and acknowledged.
    pub drained: u64,
    /// Effect receipts among them.
    pub effects: u64,
    /// The sequence the next receipt should carry.
    pub expected: u64,
    /// Gaps in the sequence, which would mean receipts were lost unseen.
    pub gaps: u64,
    /// The fullest the log was ever observed.
    pub high_water: u32,
    /// Receipts the kernel had to drop.
    pub lost: u32,
}

/// Reads and acknowledges everything the log holds.
pub fn drain(log: u64, audit: &mut Audit) {
    loop {
        let Ok(batch) = k2::log_read(log) else { return };
        if batch.lost > audit.lost {
            audit.lost = batch.lost;
        }
        if batch.count == 0 {
            return;
        }
        let mut through = 0u64;
        for record in batch.records.iter().take(batch.count as usize) {
            if audit.expected != 0 && record.sequence != audit.expected {
                audit.gaps += 1;
            }
            audit.expected = record.sequence + 1;
            audit.drained += 1;
            if record.kind == receipt_kind::EFFECT {
                audit.effects += 1;
                k2::note(note::AUDIT_EFFECT, record.object_id);
            }
            through = record.sequence;
        }
        if k2::log_acknowledge(log, through).is_err() {
            return;
        }
    }
}

/// Reads how full the log got, before draining it.
pub fn observe(log: u64, audit: &mut Audit) {
    if let Ok(info) = k2::log_query(log) {
        if info.used > audit.high_water {
            audit.high_water = info.used;
        }
        if u64::from(info.lost) > audit.lost.into() {
            audit.lost = info.lost;
        }
    }
}

/// States what the auditor saw, in the numbers a gate reads.
pub fn report(audit: &Audit) {
    k2::note(note::AUDIT_DRAINED, audit.drained);
    k2::note(note::AUDIT_EFFECTS, audit.effects);
    k2::note(
        note::AUDIT_HIGH_WATER,
        u64::from(audit.high_water) | (audit.gaps << 32),
    );
    k2::note(note::AUDIT_LOST, u64::from(audit.lost));
}
