//! Scopes: the principal that pays, fences and drains.
//!
//! A scope is not a domain. A domain is the memory boundary; a scope is what a
//! piece of work is charged to, and one scope's work can cross several domains
//! while one domain can serve several scopes. Everything a reservation touches
//! has to fit in the scope **and in every ancestor**, because otherwise creating
//! children would multiply a parent's capacity.
//!
//! The state machine is monotonic: `Open → Fenced → Quiescent → Retired`. A
//! fenced scope never reopens; retrying means creating another one with another
//! identity. Fencing is short — it publishes a state and wakes cancellable
//! waits — while draining, unmapping and reclaiming happen afterwards and are
//! reported rather than assumed.

use thalyx_abi::generated::scope_state;

use crate::limits::{CPU_WINDOW_NS, MAX_SCOPES};
use crate::obj::ScopeId;

/// Ceilings of one scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Physical pages this subtree may hold.
    pub memory_pages: u64,
    /// Kernel objects this subtree may hold.
    pub metadata_objects: u64,
    /// Execution time per fixed window, aggregated over the subtree.
    pub cpu_budget_ns: u64,
    /// Queue bytes this subtree may hold pending.
    pub queue_bytes: u64,
    /// Execution reserved for closing obligations, which ordinary work cannot
    /// reach.
    pub closure_reserve_ns: u64,
    /// Threads that may be simultaneously eligible under this scope.
    pub parallelism: u32,
}

/// Which account a reservation touches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resource {
    /// Physical pages.
    MemoryPages,
    /// Kernel objects.
    Metadata,
    /// Bytes held in bounded queues.
    QueueBytes,
}

impl Resource {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Resource::MemoryPages => "memory_pages",
            Resource::Metadata => "metadata",
            Resource::QueueBytes => "queue_bytes",
        }
    }
}

/// Lifecycle of a scope. Transitions only ever go forward.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Free table slot.
    Empty,
    /// Admitting work.
    Open,
    /// Barrier placed: no new admission under this scope or its descendants.
    Fenced,
    /// Nothing the kernel can observe is still executing or retained here.
    Quiescent,
    /// Resources released or transferred.
    Retired,
}

impl State {
    /// Interface value reported in a scope or drain descriptor.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            State::Empty => 0,
            State::Open => scope_state::OPEN,
            State::Fenced => scope_state::FENCED,
            State::Quiescent => scope_state::QUIESCENT,
            State::Retired => scope_state::RETIRED,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Open => "open",
            State::Fenced => "fenced",
            State::Quiescent => "quiescent",
            State::Retired => "retired",
        }
    }
}

/// One node of the resource tree.
pub struct Scope {
    /// Lifecycle state.
    pub state: State,
    /// Generation of this table slot, so a stale reference is refused.
    pub generation: u32,
    /// Diagnostic identity, unique within the boot epoch.
    pub id: u64,
    /// Parent scope, or `None` for the root.
    pub parent: Option<ScopeId>,
    /// Distance from the root, bounded by the interface's scope depth.
    pub depth: u16,
    /// Diagnostic label. A name grants nothing.
    pub label: [u8; 16],
    /// Ceilings.
    pub limits: Limits,
    /// Pages held by this subtree.
    pub memory_pages: u64,
    /// Kernel objects held by this subtree.
    pub metadata: u64,
    /// Queue bytes held by this subtree.
    pub queue_bytes: u64,
    /// Ordinary execution charged in the current window.
    pub cpu_window_ns: u64,
    /// Ordinary execution charged since creation.
    pub cpu_total_ns: u64,
    /// Overrun carried into later windows. Never cleared by a window change.
    pub cpu_debt_ns: u64,
    /// Execution charged to the closure reserve.
    pub closure_used_ns: u64,
    /// Window the charges above belong to.
    pub window_index: u64,
    /// Threads whose owning domain belongs to this scope.
    pub threads: u32,
    /// Threads currently charging this scope.
    pub parallelism_used: u32,
    /// Invocations admitted under this scope and not yet resolved.
    pub invocations_pending: u32,
    /// Effects admitted under this scope and not yet resolved.
    pub effects_pending: u32,
    /// Mappings installed under this scope's authority.
    pub maps_pending: u32,
    /// Messages withdrawn undelivered when the barrier went up.
    pub undelivered_cancelled: u32,
    /// Monotonic time of the last observed drain progress.
    pub last_progress_ns: u64,
    /// Progress token: two equal readings mean nothing moved between them.
    pub drain_token: u64,
    /// Live children, so a parent is not reused while a child exists.
    pub children: u32,
}

impl Scope {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: State::Empty,
            generation: 0,
            id: 0,
            parent: None,
            depth: 0,
            label: [0; 16],
            limits: Limits {
                memory_pages: 0,
                metadata_objects: 0,
                cpu_budget_ns: 0,
                queue_bytes: 0,
                closure_reserve_ns: 0,
                parallelism: 0,
            },
            memory_pages: 0,
            metadata: 0,
            queue_bytes: 0,
            cpu_window_ns: 0,
            cpu_total_ns: 0,
            cpu_debt_ns: 0,
            closure_used_ns: 0,
            window_index: 0,
            threads: 0,
            parallelism_used: 0,
            invocations_pending: 0,
            effects_pending: 0,
            maps_pending: 0,
            undelivered_cancelled: 0,
            last_progress_ns: 0,
            drain_token: 0,
            children: 0,
        }
    }

    /// Label as text, for diagnostic records.
    #[must_use]
    pub fn label_str(&self) -> &str {
        let len = self.label.iter().position(|byte| *byte == 0).unwrap_or(16);
        core::str::from_utf8(&self.label[..len]).unwrap_or("?")
    }

    fn used(&self, resource: Resource) -> u64 {
        match resource {
            Resource::MemoryPages => self.memory_pages,
            Resource::Metadata => self.metadata,
            Resource::QueueBytes => self.queue_bytes,
        }
    }

    fn limit(&self, resource: Resource) -> u64 {
        match resource {
            Resource::MemoryPages => self.limits.memory_pages,
            Resource::Metadata => self.limits.metadata_objects,
            Resource::QueueBytes => self.limits.queue_bytes,
        }
    }

    fn add(&mut self, resource: Resource, amount: u64) {
        let slot = match resource {
            Resource::MemoryPages => &mut self.memory_pages,
            Resource::Metadata => &mut self.metadata,
            Resource::QueueBytes => &mut self.queue_bytes,
        };
        *slot = slot.saturating_add(amount);
    }

    fn subtract(&mut self, resource: Resource, amount: u64) {
        let slot = match resource {
            Resource::MemoryPages => &mut self.memory_pages,
            Resource::Metadata => &mut self.metadata,
            Resource::QueueBytes => &mut self.queue_bytes,
        };
        *slot = slot.saturating_sub(amount);
    }
}

/// The scope table.
pub type Table = [Scope; MAX_SCOPES];

/// Walks from `scope` to the root, calling `f` with each index.
///
/// Depth is bounded by the interface limit, so a corrupted parent link ends the
/// walk instead of looping forever.
fn ancestors(table: &Table, scope: ScopeId, mut f: impl FnMut(usize)) {
    let mut current = Some(scope);
    let bound = thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1;
    for _ in 0..bound {
        let Some(index) = current else { return };
        let Some(node) = table.get(index as usize) else {
            return;
        };
        f(index as usize);
        current = node.parent;
    }
}

/// True when `scope` and every ancestor are open.
#[must_use]
pub fn is_open(table: &Table, scope: ScopeId) -> bool {
    let mut open = true;
    ancestors(table, scope, |index| {
        if table[index].state != State::Open {
            open = false;
        }
    });
    open
}

/// True when `candidate` is `ancestor` or below it.
#[must_use]
pub fn is_within(table: &Table, ancestor: ScopeId, candidate: ScopeId) -> bool {
    let mut found = false;
    ancestors(table, candidate, |index| {
        if index as ScopeId == ancestor {
            found = true;
        }
    });
    found
}

/// Reserves `amount` of `resource` in `scope` and every ancestor, or nothing.
///
/// The check runs over the whole chain before any counter moves, so a refusal
/// leaves no partial charge behind and a child can never spend capacity its
/// parent does not have.
pub fn reserve(table: &mut Table, scope: ScopeId, resource: Resource, amount: u64) -> bool {
    let mut fits = true;
    ancestors(table, scope, |index| {
        let node = &table[index];
        if node.state != State::Open {
            fits = false;
        } else if node.used(resource).saturating_add(amount) > node.limit(resource) {
            fits = false;
        }
    });
    if !fits {
        return false;
    }
    let mut chain = [usize::MAX; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1];
    let mut count = 0;
    ancestors(table, scope, |index| {
        if count < chain.len() {
            chain[count] = index;
            count += 1;
        }
    });
    for index in &chain[..count] {
        table[*index].add(resource, amount);
    }
    true
}

/// Returns `amount` of `resource` to `scope` and every ancestor.
pub fn release(table: &mut Table, scope: ScopeId, resource: Resource, amount: u64) {
    let mut chain = [usize::MAX; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1];
    let mut count = 0;
    ancestors(table, scope, |index| {
        if count < chain.len() {
            chain[count] = index;
            count += 1;
        }
    });
    for index in &chain[..count] {
        table[*index].subtract(resource, amount);
    }
}

/// Moves a scope and every ancestor to the window `now` belongs to.
///
/// An overrun is not forgiven by the window boundary: it starts the next window
/// already consumed, which is what "the excess is deducted from the next
/// replenishment" has to mean if it is to mean anything.
pub fn roll_window(table: &mut Table, now_ns: u64) {
    let window = now_ns / CPU_WINDOW_NS;
    for node in table.iter_mut() {
        if node.state == State::Empty || node.window_index == window {
            continue;
        }
        let overrun = node.cpu_window_ns.saturating_sub(node.limits.cpu_budget_ns);
        node.cpu_debt_ns = node.cpu_debt_ns.saturating_add(overrun);
        node.cpu_window_ns = overrun;
        node.closure_used_ns = 0;
        node.window_index = window;
    }
}

/// Charges executed time to `scope` and every ancestor.
///
/// `recovery` charges the closure account instead of the ordinary one, so the
/// cost of finishing an already admitted obligation is visible as recovery
/// spending rather than hidden inside normal consumption.
pub fn charge_cpu(table: &mut Table, scope: ScopeId, ns: u64, recovery: bool) {
    let mut chain = [usize::MAX; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1];
    let mut count = 0;
    ancestors(table, scope, |index| {
        if count < chain.len() {
            chain[count] = index;
            count += 1;
        }
    });
    for index in &chain[..count] {
        let node = &mut table[*index];
        node.cpu_total_ns = node.cpu_total_ns.saturating_add(ns);
        if recovery {
            node.closure_used_ns = node.closure_used_ns.saturating_add(ns);
        } else {
            node.cpu_window_ns = node.cpu_window_ns.saturating_add(ns);
        }
    }
}

/// True when ordinary work charged to `scope` may still be dispatched.
///
/// Every ancestor must have budget left and a free parallelism slot: a client
/// gains nothing by creating more children or more threads.
#[must_use]
pub fn eligible(table: &Table, scope: ScopeId, recovery: bool) -> bool {
    let mut ok = true;
    ancestors(table, scope, |index| {
        let node = &table[index];
        if recovery {
            if node.state == State::Retired || node.state == State::Empty {
                ok = false;
            }
            if node.closure_used_ns >= node.limits.closure_reserve_ns {
                ok = false;
            }
        } else {
            if node.state != State::Open {
                ok = false;
            }
            if node.cpu_window_ns >= node.limits.cpu_budget_ns {
                ok = false;
            }
        }
    });
    ok
}

/// True when `scope` and every ancestor have a free parallelism slot.
///
/// The limit is checked when a thread starts charging a scope, not when it is
/// dispatched: a running thread already holds its own slot, so checking at
/// dispatch would make every thread ineligible against itself. On one core the
/// limit cannot be exceeded by execution anyway; what it bounds is how many
/// threads may be charging one scope at all, borrowed workers included.
#[must_use]
pub fn has_parallelism(table: &Table, scope: ScopeId) -> bool {
    let mut ok = true;
    ancestors(table, scope, |index| {
        if table[index].parallelism_used >= table[index].limits.parallelism {
            ok = false;
        }
    });
    ok
}

/// Takes a parallelism slot in `scope` and every ancestor.
pub fn take_parallelism(table: &mut Table, scope: ScopeId) {
    let mut chain = [usize::MAX; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1];
    let mut count = 0;
    ancestors(table, scope, |index| {
        if count < chain.len() {
            chain[count] = index;
            count += 1;
        }
    });
    for index in &chain[..count] {
        table[*index].parallelism_used = table[*index].parallelism_used.saturating_add(1);
    }
}

/// Returns a parallelism slot to `scope` and every ancestor.
pub fn drop_parallelism(table: &mut Table, scope: ScopeId) {
    let mut chain = [usize::MAX; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1];
    let mut count = 0;
    ancestors(table, scope, |index| {
        if count < chain.len() {
            chain[count] = index;
            count += 1;
        }
    });
    for index in &chain[..count] {
        table[*index].parallelism_used = table[*index].parallelism_used.saturating_sub(1);
    }
}

/// Places the barrier on `scope` and every descendant, returning how many
/// scopes changed state.
///
/// This is the short operation the authority contract requires: it publishes a
/// monotonic state that every later admission observes. Walking descendants,
/// stopping threads and reclaiming happen outside it.
pub fn fence(table: &mut Table, root: ScopeId) -> u32 {
    let mut changed = 0;
    for index in 0..MAX_SCOPES {
        if table[index].state != State::Open {
            continue;
        }
        if !is_within(table, root, index as ScopeId) {
            continue;
        }
        table[index].state = State::Fenced;
        table[index].drain_token = table[index].drain_token.wrapping_add(1);
        changed += 1;
    }
    changed
}

/// Obligations still inside a closing perimeter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pending {
    /// Threads still executing work charged here.
    pub threads: u32,
    /// Invocations admitted and unresolved.
    pub invocations: u32,
    /// Effects admitted and unresolved.
    pub effects: u32,
    /// Mappings not yet withdrawn.
    pub maps: u32,
    /// Messages withdrawn before delivery.
    pub undelivered: u32,
    /// Pages still charged.
    pub pages: u64,
    /// Kernel objects still charged.
    pub metadata: u64,
}

/// Sums the obligations of `root` and every descendant.
#[must_use]
pub fn pending(table: &Table, root: ScopeId) -> Pending {
    let mut total = Pending::default();
    for index in 0..MAX_SCOPES {
        if table[index].state == State::Empty || !is_within(table, root, index as ScopeId) {
            continue;
        }
        let node = &table[index];
        total.threads += node.parallelism_used;
        total.invocations += node.invocations_pending;
        total.effects += node.effects_pending;
        total.maps += node.maps_pending;
        total.undelivered += node.undelivered_cancelled;
    }
    total.pages = table[root as usize].memory_pages;
    total.metadata = table[root as usize].metadata;
    total
}

/// Promotes fenced scopes with nothing left to observe into `Quiescent`.
///
/// Quiescence is derived from counters the kernel maintains, never from a
/// timeout: a wait that expires leaves the state exactly where it was.
pub fn advance_quiescence(table: &mut Table, now_ns: u64) -> u32 {
    let mut promoted = 0;
    for index in 0..MAX_SCOPES {
        if table[index].state != State::Fenced {
            continue;
        }
        let obligations = pending(table, index as ScopeId);
        if obligations.threads == 0
            && obligations.invocations == 0
            && obligations.effects == 0
            && obligations.maps == 0
        {
            table[index].state = State::Quiescent;
            table[index].last_progress_ns = now_ns;
            table[index].drain_token = table[index].drain_token.wrapping_add(1);
            promoted += 1;
        }
    }
    promoted
}
