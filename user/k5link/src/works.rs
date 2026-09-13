//! Work scopes: the kernel's `WorkControl`, as the link uses it.
//!
//! Thalyx's platform boundary names `WorkControl` -- admission of an effect
//! against the work that asked for it, so a work whose scope was fenced starts
//! nothing and publishes nothing. On Linux that is `Scoped`, a flag a test
//! closes. Here it is a kernel scope: one per Thalyx transaction, created under
//! the parent scope the supervisor gave the link `SCOPE_CREATE` on, and fenced
//! by the link on the harness's word or the scenario's. A grant derived under a
//! work's scope stops working the instant that scope is fenced -- checked by
//! the kernel on every use, not by this program -- which is what makes "a
//! closed work cannot publish" a fact about the kernel rather than a claim.
//!
//! This is not a second persistence mechanism and not a Linux compatibility
//! layer: it is the kernel's own scope tree, used for the exact property the
//! boundary asks of it.

use thalyx_abi::ScopeLimits;
use thalyx_user_rt::k2::{self, name16};

/// Works the link keeps at once.
pub const MAX_WORKS: usize = 8;

#[derive(Clone, Copy)]
struct Slot {
    used: bool,
    work: u64,
    scope: u64,
    line: u32,
    fenced: bool,
    cpu_ns: u64,
    pages_peak: u64,
    metadata_peak: u64,
}

impl Slot {
    const EMPTY: Slot = Slot {
        used: false,
        work: 0,
        scope: 0,
        line: 0,
        fenced: false,
        cpu_ns: 0,
        pages_peak: 0,
        metadata_peak: 0,
    };
}

/// The link's factory of work scopes.
pub struct Works {
    /// The parent scope, held with `SCOPE_CREATE` and `SCOPE_FENCE`.
    parent: u64,
    /// A window of the parent's budget one work may draw on.
    work_budget_ns: u64,
    work_pages: u64,
    slots: [Slot; MAX_WORKS],
    next: u64,
    pub opened: u64,
    pub fenced: u64,
    pub retired: u64,
}

impl Works {
    pub fn new(parent: u64, work_budget_ns: u64, work_pages: u64) -> Self {
        Works {
            parent,
            work_budget_ns,
            work_pages,
            slots: [Slot::EMPTY; MAX_WORKS],
            next: 1,
            opened: 0,
            fenced: 0,
            retired: 0,
        }
    }

    fn slot(&self, work: u64) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.used && slot.work == work)
    }

    /// Opens a work: a child scope of the parent. Answers the work number and
    /// the scope handle. The scope handle is what a grant's `life_scope`
    /// points at, so that fencing this scope reaches every grant derived under
    /// it.
    pub fn open(&mut self, line: u32, name: &[u8]) -> Option<(u64, u64)> {
        let index = self.slots.iter().position(|slot| !slot.used)?;
        let mut label = [0u8; 16];
        let take = name.len().min(16);
        label[..take].copy_from_slice(&name[..take]);
        let _ = name16;
        let limits = ScopeLimits {
            memory_pages: self.work_pages,
            metadata_objects: 64,
            cpu_budget_ns: self.work_budget_ns,
            queue_bytes: 8 * 1024,
            closure_reserve_ns: 400_000,
            parallelism: 2,
            reserved0: 0,
        };
        let scope = k2::scope_create_child(self.parent, limits, label).ok()?;
        let work = self.next;
        self.next += 1;
        self.slots[index] = Slot {
            used: true,
            work,
            scope,
            line,
            fenced: false,
            cpu_ns: 0,
            pages_peak: 0,
            metadata_peak: 0,
        };
        self.opened += 1;
        Some((work, scope))
    }

    /// Records what a work's scope has been charged, before it is retired.
    fn account(&mut self, index: usize) {
        if let Ok(info) = k2::scope_query(self.slots[index].scope) {
            self.slots[index].cpu_ns = info.cpu_total_ns;
            self.slots[index].pages_peak = info.memory_pages_used;
            self.slots[index].metadata_peak = info.metadata_used;
        }
    }

    /// Fences a work's scope now: every grant derived under it stops working.
    pub fn fence(&mut self, work: u64) -> bool {
        let Some(index) = self.slot(work) else {
            return false;
        };
        if self.slots[index].fenced {
            return true;
        }
        if k2::scope_fence(self.slots[index].scope).is_err() {
            return false;
        }
        self.slots[index].fenced = true;
        self.fenced += 1;
        true
    }

    /// Closes a work: fences, drains and retires its scope, and frees the slot.
    /// The scope handle the caller holds is theirs to close.
    pub fn close(&mut self, work: u64) -> bool {
        let Some(index) = self.slot(work) else {
            return false;
        };
        self.account(index);
        let scope = self.slots[index].scope;
        let _ = k2::scope_fence(scope);
        // Drain: wait for the scope to be quiescent. The link derived one grant
        // under it and the caller closes that grant before this, so what is
        // left is the scope itself.
        let deadline = k2::now_ns() + 5_000_000_000;
        loop {
            let (outcome, _) = k2::scope_retire(scope);
            if outcome.is_ok() {
                break;
            }
            if k2::now_ns() >= deadline {
                break;
            }
            let _ = k2::signal_wait(scope, 0, k2::now_ns() + 1_000_000);
        }
        self.slots[index] = Slot::EMPTY;
        self.retired += 1;
        true
    }
    /// The work a line has open, if any: the harness fences by line.
    pub fn work_of_line(&self, line: u32) -> Option<u64> {
        self.slots
            .iter()
            .find(|slot| slot.used && slot.line == line && !slot.fenced)
            .map(|slot| slot.work)
    }
}
