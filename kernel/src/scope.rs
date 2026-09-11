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

/// `scope.debt` records written per scope before the diagnostic plane counts
/// overruns instead of writing each one. The observability contract permits
/// coalescing and requires saying so; `scope.accounting` says so.
pub const DEBT_RECORD_LIMIT: u32 = 16;

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
    /// Ordinary execution promised to dispatches that have not been settled.
    ///
    /// This is what stops two processors from spending one balance. Both read
    /// the same remaining budget, but only one of them can turn it into a
    /// reservation, and the other is refused before it dispatches rather than
    /// after it has already run.
    pub cpu_reserved_ns: u64,
    /// Closing capacity promised to recovery dispatches not yet settled.
    pub closure_dispatch_ns: u64,
    /// Threads dispatched on some processor charging this scope right now.
    pub running: u32,
    /// Most this scope ever held dispatched at one instant.
    ///
    /// Sampled where the count changes rather than where a report is asked
    /// for. A report can only see the instant it runs in, and on several
    /// processors that instant is very unlikely to be the interesting one.
    pub max_running: u32,
    /// Most this scope ever had committed at once in a window: what it had
    /// already spent plus what it had promised and not yet spent.
    ///
    /// The reservation exists so this never exceeds the budget. Recording the
    /// peak where the commitment is made turns that from a claim about the
    /// code into a number a run either contradicts or does not.
    pub max_committed_ns: u64,
    /// Dispatch reservations granted.
    pub dispatch_grants: u64,
    /// Dispatch reservations refused for want of budget or simultaneity.
    pub dispatch_refusals: u64,
    /// What `cpu_window_ns` held when the current window opened: the overrun
    /// the previous window carried in.
    pub window_opened_ns: u64,
    /// Most this scope was charged *inside* one window, carried debt excluded.
    pub max_charged_in_window_ns: u64,
    /// Most a window was charged beyond what that window could admit.
    ///
    /// This is the number the reservation is supposed to bound, and it is not
    /// the same as the overrun. A window that opens already in debt can close
    /// over budget having executed nothing; what says whether admission held is
    /// how much ran beyond what admission allowed. Preemption is tick-driven,
    /// so a dispatch can outlive its reservation by less than a tick period on
    /// each processor, and that is the whole of what this may contain.
    pub max_excess_ns: u64,
    /// Ordinary execution charged since creation.
    pub cpu_total_ns: u64,
    /// Overrun carried into later windows. Never cleared by a window change.
    pub cpu_debt_ns: u64,
    /// Execution charged to the closure reserve.
    pub closure_used_ns: u64,
    /// Closing capacity admitted effects are still holding but have not spent.
    pub closure_reserved_ns: u64,
    /// Window the charges above belong to.
    pub window_index: u64,
    /// Most this scope was charged in any window that has closed.
    ///
    /// The number the reservation exists to bound. With one processor it is
    /// bounded by arithmetic; with several it is bounded only if a dispatch
    /// takes the balance before it runs, and reading it back afterwards is how
    /// that stops being an assertion.
    pub max_window_ns: u64,
    /// Windows that have closed with this scope in existence.
    pub windows_closed: u64,
    /// Of those, how many closed over budget.
    pub overruns: u64,
    /// The largest overrun observed.
    pub max_overrun_ns: u64,
    /// Overruns written out as their own `scope.debt` record. Every overrun is
    /// counted in `overruns`; past [`DEBT_RECORD_LIMIT`] the plane stops writing
    /// one line per window and the summary states how many it stands for.
    pub debt_records: u32,
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
    /// Capability entries naming this scope, wherever they are held.
    pub refs: u32,
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
            cpu_reserved_ns: 0,
            closure_dispatch_ns: 0,
            running: 0,
            max_running: 0,
            max_committed_ns: 0,
            dispatch_grants: 0,
            dispatch_refusals: 0,
            window_opened_ns: 0,
            max_charged_in_window_ns: 0,
            max_excess_ns: 0,
            cpu_total_ns: 0,
            cpu_debt_ns: 0,
            closure_used_ns: 0,
            closure_reserved_ns: 0,
            window_index: 0,
            max_window_ns: 0,
            windows_closed: 0,
            overruns: 0,
            max_overrun_ns: 0,
            debt_records: 0,
            threads: 0,
            parallelism_used: 0,
            invocations_pending: 0,
            effects_pending: 0,
            maps_pending: 0,
            undelivered_cancelled: 0,
            last_progress_ns: 0,
            drain_token: 0,
            children: 0,
            refs: 0,
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
    let mut binding = scope;
    ancestors(table, scope, |index| {
        let node = &table[index];
        // A closed scope and an exhausted one both refuse, and the record names
        // the first ancestor that did either. Which of the two it was is in the
        // state the record also prints.
        let refuses = node.state != State::Open
            || node.used(resource).saturating_add(amount) > node.limit(resource);
        if refuses {
            if fits {
                binding = index as ScopeId;
            }
            fits = false;
        }
    });
    if !fits {
        // Which scope and which resource, not merely that something was
        // refused. A limit that is only ever reported as a status code is a
        // limit whose accounting nobody can check: the record names the
        // ancestor that actually bound the request, which is not always the one
        // the caller addressed.
        crate::trace!(
            "scope.limit_refused",
            "scope={scope} binding_scope={binding} binding_id={} resource={} \
             requested={amount} used={} limit={} state={}",
            table[binding as usize].id,
            resource.name(),
            table[binding as usize].used(resource),
            table[binding as usize].limit(resource),
            table[binding as usize].state.name()
        );
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
    for index in 0..MAX_SCOPES {
        let node = &table[index];
        if node.state == State::Empty || node.window_index == window {
            continue;
        }
        let closing = node.cpu_window_ns;
        let overrun = node.cpu_window_ns.saturating_sub(node.limits.cpu_budget_ns);
        // Written for the first overruns of a scope and counted after that.
        // One line per window was a line per ten milliseconds per spinning
        // scope, written from the timer with the machine lock held; under KVM
        // each line cost about a millisecond of port I/O, the run lasted
        // longer, more windows closed in debt, and the records fed on
        // themselves until a drain that should have finished timed out.
        let written = overrun != 0 && node.debt_records < DEBT_RECORD_LIMIT;
        if written {
            // Carried debt that nobody can see is not a limit, it is a number.
            // The record is emitted only when a window actually closed over
            // budget, so it says something happened rather than that a window
            // went by.
            crate::trace!(
                "scope.debt",
                "scope={index} id={} label={} window={window} overrun_ns={overrun} \
                 debt_ns={} budget_ns={} carried_into_next=1",
                node.id,
                node.label_str(),
                node.cpu_debt_ns.saturating_add(overrun),
                node.limits.cpu_budget_ns
            );
        }
        let node = &mut table[index];
        if written {
            node.debt_records += 1;
        }
        node.windows_closed += 1;
        if closing > node.max_window_ns {
            node.max_window_ns = closing;
        }
        // What this window actually executed, and what it was allowed to
        // admit. Their difference is the only part of the overrun the
        // reservation is answerable for.
        let charged = closing.saturating_sub(node.window_opened_ns);
        let admissible = node
            .limits
            .cpu_budget_ns
            .saturating_sub(node.window_opened_ns);
        if charged > node.max_charged_in_window_ns {
            node.max_charged_in_window_ns = charged;
        }
        let excess = charged.saturating_sub(admissible);
        if excess > node.max_excess_ns {
            node.max_excess_ns = excess;
        }
        if overrun != 0 {
            node.overruns += 1;
            if overrun > node.max_overrun_ns {
                node.max_overrun_ns = overrun;
            }
        }
        node.cpu_debt_ns = node.cpu_debt_ns.saturating_add(overrun);
        node.cpu_window_ns = overrun;
        node.window_opened_ns = overrun;
        node.closure_used_ns = 0;
        // A reservation belongs to the window it was made in. Carrying it
        // across the boundary would credit the new window with capacity the old
        // one had promised, which is the delayed-settlement mistake the
        // scheduling groundwork names. The dispatch that made it settles
        // against the window it is in when it settles, and finds nothing to
        // return here.
        node.cpu_reserved_ns = 0;
        node.closure_dispatch_ns = 0;
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

/// Execution `scope` and its ancestors can still promise this window.
///
/// The minimum over the chain, because a reservation has to fit in every
/// ancestor, and zero when any of them is closed to new work.
#[must_use]
pub fn available_ns(table: &Table, scope: ScopeId, recovery: bool) -> u64 {
    let mut available = u64::MAX;
    ancestors(table, scope, |index| {
        let node = &table[index];
        let usable = if recovery {
            !matches!(node.state, State::Retired | State::Empty)
        } else {
            matches!(node.state, State::Open | State::Fenced)
        };
        if !usable {
            available = 0;
            return;
        }
        let (used, limit) = if recovery {
            (
                node.closure_used_ns
                    .saturating_add(node.closure_dispatch_ns),
                node.limits.closure_reserve_ns,
            )
        } else {
            (
                node.cpu_window_ns.saturating_add(node.cpu_reserved_ns),
                node.limits.cpu_budget_ns,
            )
        };
        let left = limit.saturating_sub(used);
        if left < available {
            available = left;
        }
    });
    if available == u64::MAX { 0 } else { available }
}

/// Takes `amount` of execution and one simultaneity slot in `scope` and every
/// ancestor, or takes nothing.
///
/// This is the reservation the resource contract requires before a dispatch.
/// It is one critical section over the whole chain: checking first and
/// committing afterwards, in two steps, is exactly how two processors end up
/// spending the same balance.
pub fn reserve_dispatch(table: &mut Table, scope: ScopeId, amount: u64, recovery: bool) -> bool {
    if available_ns(table, scope, recovery) < amount {
        table[scope as usize].dispatch_refusals =
            table[scope as usize].dispatch_refusals.saturating_add(1);
        return false;
    }
    let mut fits = true;
    ancestors(table, scope, |index| {
        if table[index].running >= table[index].limits.parallelism {
            fits = false;
        }
    });
    if !fits {
        table[scope as usize].dispatch_refusals =
            table[scope as usize].dispatch_refusals.saturating_add(1);
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
        let node = &mut table[*index];
        node.running = node.running.saturating_add(1);
        if node.running > node.max_running {
            node.max_running = node.running;
        }
        if recovery {
            node.closure_dispatch_ns = node.closure_dispatch_ns.saturating_add(amount);
        } else {
            node.cpu_reserved_ns = node.cpu_reserved_ns.saturating_add(amount);
            let committed = node.cpu_window_ns.saturating_add(node.cpu_reserved_ns);
            if committed > node.max_committed_ns {
                node.max_committed_ns = committed;
            }
        }
    }
    table[scope as usize].dispatch_grants = table[scope as usize].dispatch_grants.saturating_add(1);
    true
}

/// Settles a dispatch: charges what was executed and returns what was not.
///
/// `window` is the window the reservation was made in. If the window has moved
/// on, the reservation was already discarded at the boundary and there is
/// nothing to return; charging still happens, against the window the scope is
/// in now.
pub fn settle_dispatch(
    table: &mut Table,
    scope: ScopeId,
    reserved: u64,
    window: u64,
    recovery: bool,
) {
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
        node.running = node.running.saturating_sub(1);
        if node.window_index != window {
            continue;
        }
        if recovery {
            node.closure_dispatch_ns = node.closure_dispatch_ns.saturating_sub(reserved);
        } else {
            node.cpu_reserved_ns = node.cpu_reserved_ns.saturating_sub(reserved);
        }
    }
}

/// Highest simultaneity any scope in the table ever held.
#[must_use]
pub fn peak_running(table: &Table) -> u32 {
    table.iter().map(|node| node.max_running).max().unwrap_or(0)
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
        // Live executions, not the ones charging the scope at this instant. A
        // thread that exists but is not currently dispatched is still something
        // a drain has to wait for; counting only the dispatched ones would
        // declare quiescence with a domain of the scope still alive.
        total.threads += node.threads;
        total.invocations += node.invocations_pending;
        total.effects += node.effects_pending;
        total.maps += node.maps_pending;
        total.undelivered += node.undelivered_cancelled;
    }
    total.pages = table[root as usize].memory_pages;
    total.metadata = table[root as usize].metadata;
    total
}

/// Frees the table slot of a retired scope that nothing names and nothing
/// depends on: no capability entry, no child scope, no frame of its still in
/// quarantine. The slot keeps its generation, so a handle that outlived the
/// scope names nothing rather than the next occupant, and the parent's count
/// of children goes down by one.
///
/// Called when a creation finds no empty slot, not at retirement: a retired
/// scope's slot is also its accounting, which the run's summary reads at the
/// end. Until K6 a retired scope kept its slot for the rest of the boot, and
/// K6's closure benchmark, which builds and retires a scope per sample, found
/// the table of twenty-four full after twenty-two.
pub fn collect_retired(machine: &mut crate::state::Machine, index: usize) -> bool {
    let node = &machine.scopes[index];
    if node.state != State::Retired || node.refs != 0 || node.threads != 0 {
        return false;
    }
    let (generation, id, parent) = (node.generation, node.id, node.parent);
    let has_child = machine
        .scopes
        .iter()
        .any(|other| other.state != State::Empty && other.parent == Some(index as ScopeId));
    if has_child
        || machine
            .allocator()
            .quarantined_for(crate::mm::Owner::Scope(index as u16))
            != 0
    {
        return false;
    }
    crate::trace!("scope.collected", "scope={index} id={id}");
    if let Some(parent) = parent {
        machine.scopes[parent as usize].children =
            machine.scopes[parent as usize].children.saturating_sub(1);
    }
    machine.scopes[index] = Scope::empty();
    machine.scopes[index].generation = generation;
    true
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
