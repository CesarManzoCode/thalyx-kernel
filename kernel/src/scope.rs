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
//!
//! # Accounting without the machine lock
//!
//! Until this revision every counter of every scope lived under the single
//! machine lock, and K6 measured what that cost: four independent IPC pairs
//! spent their time waiting for each other's reservations. The tree itself --
//! who is whose parent, what a scope's ceilings are, which slot is in use --
//! still changes only under the machine lock, because it changes rarely. The
//! *counters* are atomics, and a reservation over a chain is a sequence of
//! atomic increments, each checked against its ceiling as it lands and undone
//! from the tail if a later ancestor refuses. A ceiling is therefore never
//! exceeded, not even transiently: the increment that would exceed it is the
//! one that fails. What is given up is only the pretence that the whole chain
//! was checked before any counter moved; a refusal now leaves a charge behind
//! for the few nanoseconds it takes to undo it, and nothing can observe more
//! than the ceiling in that window.
//!
//! The processor budget goes one step further, for the reason Linux's
//! bandwidth controller keeps a per-runqueue slice of a group's quota: a
//! dispatch happens on every context switch, and a context switch on one
//! processor must not write a cache line another processor is switching on.
//! Each processor holds a **credit** per scope -- execution it has already
//! reserved from the pool for the current window and not yet handed to a
//! thread -- and dispatches out of that credit without touching the pool at
//! all. The pool is touched when the credit is empty, when the window rolls,
//! and when the processor goes idle and gives its credit back. The ceiling is
//! exactly what it was: nothing is ever dispatched that was not reserved from
//! the pool first, and the pool never hands out more than the budget.

use core::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

use thalyx_abi::generated::scope_state;

use crate::limits::{CPU_WINDOW_NS, MAX_CPUS, MAX_SCOPES};
use crate::obj::ScopeId;
use crate::sync::SpinLock;

/// `scope.debt` records written per scope before the diagnostic plane counts
/// overruns instead of writing each one. The observability contract permits
/// coalescing and requires saying so; `scope.accounting` says so.
pub const DEBT_RECORD_LIMIT: u32 = 16;

/// A scope index that names no scope.
pub const NO_SCOPE: u16 = u16::MAX;

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

/// The ceilings as they are stored: one atomic per field, so a reservation
/// on another processor reads a consistent value of each without a lock.
pub struct Ceilings {
    memory_pages: AtomicU64,
    metadata_objects: AtomicU64,
    cpu_budget_ns: AtomicU64,
    queue_bytes: AtomicU64,
    closure_reserve_ns: AtomicU64,
    parallelism: AtomicU32,
}

impl Ceilings {
    const fn zero() -> Self {
        Self {
            memory_pages: AtomicU64::new(0),
            metadata_objects: AtomicU64::new(0),
            cpu_budget_ns: AtomicU64::new(0),
            queue_bytes: AtomicU64::new(0),
            closure_reserve_ns: AtomicU64::new(0),
            parallelism: AtomicU32::new(0),
        }
    }

    /// The ceilings as a value.
    #[must_use]
    pub fn load(&self) -> Limits {
        Limits {
            memory_pages: self.memory_pages.load(Ordering::Relaxed),
            metadata_objects: self.metadata_objects.load(Ordering::Relaxed),
            cpu_budget_ns: self.cpu_budget_ns.load(Ordering::Relaxed),
            queue_bytes: self.queue_bytes.load(Ordering::Relaxed),
            closure_reserve_ns: self.closure_reserve_ns.load(Ordering::Relaxed),
            parallelism: self.parallelism.load(Ordering::Relaxed),
        }
    }

    /// Replaces the ceilings. Under the machine lock only.
    pub fn store(&self, limits: Limits) {
        self.memory_pages
            .store(limits.memory_pages, Ordering::Relaxed);
        self.metadata_objects
            .store(limits.metadata_objects, Ordering::Relaxed);
        self.cpu_budget_ns
            .store(limits.cpu_budget_ns, Ordering::Relaxed);
        self.queue_bytes
            .store(limits.queue_bytes, Ordering::Relaxed);
        self.closure_reserve_ns
            .store(limits.closure_reserve_ns, Ordering::Relaxed);
        self.parallelism
            .store(limits.parallelism, Ordering::Relaxed);
    }

    /// Execution budget per window.
    #[must_use]
    pub fn cpu_budget_ns(&self) -> u64 {
        self.cpu_budget_ns.load(Ordering::Relaxed)
    }

    /// Closing capacity per window.
    #[must_use]
    pub fn closure_reserve_ns(&self) -> u64 {
        self.closure_reserve_ns.load(Ordering::Relaxed)
    }

    /// Simultaneity ceiling.
    #[must_use]
    pub fn parallelism(&self) -> u32 {
        self.parallelism.load(Ordering::Relaxed)
    }
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
#[repr(u8)]
pub enum State {
    /// Free table slot.
    Empty = 0,
    /// Admitting work.
    Open = 1,
    /// Barrier placed: no new admission under this scope or its descendants.
    Fenced = 2,
    /// Nothing the kernel can observe is still executing or retained here.
    Quiescent = 3,
    /// Resources released or transferred.
    Retired = 4,
}

impl State {
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => State::Open,
            2 => State::Fenced,
            3 => State::Quiescent,
            4 => State::Retired,
            _ => State::Empty,
        }
    }

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
///
/// Fields fall in two classes. The **structure** -- state, generation,
/// identity, parent, depth, label, ceilings -- is written only under the
/// machine lock, and read anywhere: every field is atomic or written only
/// while the slot is empty, so a reader without the lock sees a value that
/// was true at some instant rather than a torn one. The **counters** are
/// atomics that any processor moves without the lock, following the rules in
/// the module documentation.
pub struct Scope {
    /// Lifecycle state.
    state: AtomicU8,
    /// Generation of this table slot, so a stale reference is refused.
    pub generation: AtomicU32,
    /// Diagnostic identity, unique within the boot epoch.
    pub id: AtomicU64,
    /// Parent scope, or [`NO_SCOPE`] for the root.
    parent: AtomicU16,
    /// Distance from the root, bounded by the interface's scope depth.
    pub depth: AtomicU16,
    /// Diagnostic label. A name grants nothing. Written once, at creation,
    /// under the machine lock, while nothing names the slot.
    label: [AtomicU8; 16],
    /// Ceilings.
    pub limits: Ceilings,
    /// Pages held by this subtree.
    pub memory_pages: AtomicU64,
    /// Kernel objects held by this subtree.
    pub metadata: AtomicU64,
    /// Queue bytes held by this subtree.
    pub queue_bytes: AtomicU64,
    /// Ordinary execution charged in the current window.
    pub cpu_window_ns: AtomicU64,
    /// Ordinary execution committed in the current window: charged plus
    /// promised to dispatches and credits not yet settled.
    ///
    /// This is what stops two processors from spending one balance. Both read
    /// the same remaining budget, but only one of them can turn it into a
    /// reservation, and the other is refused before it dispatches rather than
    /// after it has already run. It is one counter rather than "charged plus
    /// reserved" because the ceiling has to be checked against the sum at one
    /// instant, and two counters cannot be read at one instant without a lock.
    pub cpu_committed_ns: AtomicU64,
    /// Closing capacity charged in the current window.
    pub closure_used_ns: AtomicU64,
    /// Closing capacity committed: used plus promised to recovery dispatches.
    pub closure_committed_ns: AtomicU64,
    /// Processors holding a dispatch credit or a running thread of this
    /// scope right now.
    pub running: AtomicU32,
    /// Most this scope ever held dispatched at one instant.
    ///
    /// Sampled where the count changes rather than where a report is asked
    /// for. A report can only see the instant it runs in, and on several
    /// processors that instant is very unlikely to be the interesting one.
    pub max_running: AtomicU32,
    /// Most this scope ever had committed at once in a window: what it had
    /// already spent plus what it had promised and not yet spent.
    ///
    /// The reservation exists so this never exceeds the budget. Recording the
    /// peak where the commitment is made turns that from a claim about the
    /// code into a number a run either contradicts or does not.
    pub max_committed_ns: AtomicU64,
    /// Dispatch reservations granted.
    pub dispatch_grants: AtomicU64,
    /// Dispatch reservations refused for want of budget or simultaneity.
    pub dispatch_refusals: AtomicU64,
    /// What `cpu_window_ns` held when the current window opened: the overrun
    /// the previous window carried in.
    pub window_opened_ns: AtomicU64,
    /// Most this scope was charged *inside* one window, carried debt excluded.
    pub max_charged_in_window_ns: AtomicU64,
    /// Most a window was charged beyond what that window could admit.
    ///
    /// This is the number the reservation is supposed to bound, and it is not
    /// the same as the overrun. A window that opens already in debt can close
    /// over budget having executed nothing; what says whether admission held is
    /// how much ran beyond what admission allowed. Preemption is timer-driven,
    /// so a dispatch can outlive its reservation by the interrupt latency on
    /// each processor, and that is the whole of what this may contain.
    pub max_excess_ns: AtomicU64,
    /// Ordinary execution charged since creation.
    pub cpu_total_ns: AtomicU64,
    /// Overrun carried into later windows. Never cleared by a window change.
    pub cpu_debt_ns: AtomicU64,
    /// Closing capacity admitted effects are still holding but have not spent.
    pub closure_reserved_ns: AtomicU64,
    /// Window the charges above belong to.
    pub window_index: AtomicU64,
    /// Most this scope was charged in any window that has closed.
    ///
    /// The number the reservation exists to bound. With one processor it is
    /// bounded by arithmetic; with several it is bounded only if a dispatch
    /// takes the balance before it runs, and reading it back afterwards is how
    /// that stops being an assertion.
    pub max_window_ns: AtomicU64,
    /// Windows that have closed with this scope in existence.
    pub windows_closed: AtomicU64,
    /// Of those, how many closed over budget.
    pub overruns: AtomicU64,
    /// The largest overrun observed.
    pub max_overrun_ns: AtomicU64,
    /// Overruns written out as their own `scope.debt` record. Every overrun is
    /// counted in `overruns`; past [`DEBT_RECORD_LIMIT`] the plane stops writing
    /// one line per window and the summary states how many it stands for.
    pub debt_records: AtomicU32,
    /// Threads whose owning domain belongs to this scope.
    pub threads: AtomicU32,
    /// Threads currently charging this scope.
    pub parallelism_used: AtomicU32,
    /// Invocations admitted under this scope and not yet resolved.
    pub invocations_pending: AtomicU32,
    /// Effects admitted under this scope and not yet resolved.
    pub effects_pending: AtomicU32,
    /// Mappings installed under this scope's authority.
    pub maps_pending: AtomicU32,
    /// Messages withdrawn undelivered when the barrier went up.
    pub undelivered_cancelled: AtomicU32,
    /// Monotonic time of the last observed drain progress.
    pub last_progress_ns: AtomicU64,
    /// Progress token: two equal readings mean nothing moved between them.
    pub drain_token: AtomicU64,
    /// Live children, so a parent is not reused while a child exists.
    pub children: AtomicU32,
    /// Capability entries naming this scope, wherever they are held.
    pub refs: AtomicU32,
}

impl Scope {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: AtomicU8::new(0),
            generation: AtomicU32::new(0),
            id: AtomicU64::new(0),
            parent: AtomicU16::new(NO_SCOPE),
            depth: AtomicU16::new(0),
            label: [const { AtomicU8::new(0) }; 16],
            limits: Ceilings::zero(),
            memory_pages: AtomicU64::new(0),
            metadata: AtomicU64::new(0),
            queue_bytes: AtomicU64::new(0),
            cpu_window_ns: AtomicU64::new(0),
            cpu_committed_ns: AtomicU64::new(0),
            closure_used_ns: AtomicU64::new(0),
            closure_committed_ns: AtomicU64::new(0),
            running: AtomicU32::new(0),
            max_running: AtomicU32::new(0),
            max_committed_ns: AtomicU64::new(0),
            dispatch_grants: AtomicU64::new(0),
            dispatch_refusals: AtomicU64::new(0),
            window_opened_ns: AtomicU64::new(0),
            max_charged_in_window_ns: AtomicU64::new(0),
            max_excess_ns: AtomicU64::new(0),
            cpu_total_ns: AtomicU64::new(0),
            cpu_debt_ns: AtomicU64::new(0),
            closure_reserved_ns: AtomicU64::new(0),
            window_index: AtomicU64::new(0),
            max_window_ns: AtomicU64::new(0),
            windows_closed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            max_overrun_ns: AtomicU64::new(0),
            debt_records: AtomicU32::new(0),
            threads: AtomicU32::new(0),
            parallelism_used: AtomicU32::new(0),
            invocations_pending: AtomicU32::new(0),
            effects_pending: AtomicU32::new(0),
            maps_pending: AtomicU32::new(0),
            undelivered_cancelled: AtomicU32::new(0),
            last_progress_ns: AtomicU64::new(0),
            drain_token: AtomicU64::new(0),
            children: AtomicU32::new(0),
            refs: AtomicU32::new(0),
        }
    }

    /// Resets every field of the slot but its generation. Under the machine
    /// lock, while nothing names the slot.
    pub fn reset(&self) {
        let generation = self.generation.load(Ordering::Relaxed);
        let fresh = Scope::empty();
        // Every field is an atomic; copying them one by one keeps this the
        // one place that knows the whole list.
        macro_rules! copy {
            ($($field:ident),*) => { $( self.$field.store(fresh.$field.load(Ordering::Relaxed), Ordering::Relaxed); )* };
        }
        copy!(
            state,
            id,
            parent,
            depth,
            memory_pages,
            metadata,
            queue_bytes,
            cpu_window_ns,
            cpu_committed_ns,
            closure_used_ns,
            closure_committed_ns,
            running,
            max_running,
            max_committed_ns,
            dispatch_grants,
            dispatch_refusals,
            window_opened_ns,
            max_charged_in_window_ns,
            max_excess_ns,
            cpu_total_ns,
            cpu_debt_ns,
            closure_reserved_ns,
            window_index,
            max_window_ns,
            windows_closed,
            overruns,
            max_overrun_ns,
            debt_records,
            threads,
            parallelism_used,
            invocations_pending,
            effects_pending,
            maps_pending,
            undelivered_cancelled,
            last_progress_ns,
            drain_token,
            children,
            refs
        );
        for byte in &self.label {
            byte.store(0, Ordering::Relaxed);
        }
        self.limits.store(Limits::default());
        self.generation.store(generation, Ordering::Relaxed);
    }

    /// Lifecycle state.
    #[must_use]
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Moves the scope to `state`. Under the machine lock only; the store is
    /// a release, so a sweep that follows it is ordered after it everywhere.
    pub fn set_state(&self, state: State) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Parent scope, if any.
    #[must_use]
    pub fn parent(&self) -> Option<ScopeId> {
        let parent = self.parent.load(Ordering::Relaxed);
        if parent == NO_SCOPE {
            None
        } else {
            Some(parent)
        }
    }

    /// Sets the parent. Under the machine lock, at creation.
    pub fn set_parent(&self, parent: Option<ScopeId>) {
        self.parent
            .store(parent.unwrap_or(NO_SCOPE), Ordering::Relaxed);
    }

    /// Diagnostic identity.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id.load(Ordering::Relaxed)
    }

    /// Writes the label. Under the machine lock, at creation.
    pub fn set_label(&self, label: [u8; 16]) {
        for (slot, byte) in self.label.iter().zip(label) {
            slot.store(byte, Ordering::Relaxed);
        }
    }

    /// The label as bytes.
    #[must_use]
    pub fn label(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (slot, byte) in out.iter_mut().zip(&self.label) {
            *slot = byte.load(Ordering::Relaxed);
        }
        out
    }

    /// Label as text, for diagnostic records.
    #[must_use]
    pub fn label_str(&self) -> LabelText {
        LabelText(self.label())
    }

    fn account(&self, resource: Resource) -> &AtomicU64 {
        match resource {
            Resource::MemoryPages => &self.memory_pages,
            Resource::Metadata => &self.metadata,
            Resource::QueueBytes => &self.queue_bytes,
        }
    }

    fn limit(&self, resource: Resource) -> u64 {
        match resource {
            Resource::MemoryPages => self.limits.memory_pages.load(Ordering::Relaxed),
            Resource::Metadata => self.limits.metadata_objects.load(Ordering::Relaxed),
            Resource::QueueBytes => self.limits.queue_bytes.load(Ordering::Relaxed),
        }
    }

    /// How much of `resource` the subtree holds.
    #[must_use]
    pub fn used(&self, resource: Resource) -> u64 {
        self.account(resource).load(Ordering::Relaxed)
    }
}

/// A label copied out of a scope, printable as text.
pub struct LabelText([u8; 16]);

impl core::fmt::Display for LabelText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let len = self.0.iter().position(|byte| *byte == 0).unwrap_or(16);
        f.write_str(core::str::from_utf8(&self.0[..len]).unwrap_or("?"))
    }
}

/// The scope table.
pub type Table = [Scope; MAX_SCOPES];

static SCOPES: Table = [const { Scope::empty() }; MAX_SCOPES];

/// The scope table. The structure is written under the machine lock; the
/// counters move without it.
#[must_use]
pub fn table() -> &'static Table {
    &SCOPES
}

/// The chain from `scope` to the root, leaf first.
///
/// Depth is bounded by the interface limit, so a corrupted parent link ends the
/// walk instead of looping forever. The parent links of a live chain are
/// stable: an ancestor cannot be retired and collected while a descendant
/// exists, so a walk from a live scope reads links nobody is rewriting.
#[derive(Clone, Copy)]
pub struct Chain {
    nodes: [u16; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1],
    len: usize,
}

impl Chain {
    /// The chain above `scope`, `scope` first.
    #[must_use]
    pub fn of(scope: ScopeId) -> Self {
        let mut chain = Self {
            nodes: [0; thalyx_abi::limit::MAX_SCOPE_DEPTH as usize + 1],
            len: 0,
        };
        let mut current = scope;
        while chain.len < chain.nodes.len() {
            let Some(node) = SCOPES.get(current as usize) else {
                break;
            };
            chain.nodes[chain.len] = current;
            chain.len += 1;
            match node.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
        chain
    }

    /// The scopes of the chain, leaf first.
    #[must_use]
    pub fn nodes(&self) -> &[u16] {
        &self.nodes[..self.len]
    }

    /// The chain as scopes, leaf first.
    pub fn iter(&self) -> impl Iterator<Item = &'static Scope> + '_ {
        self.nodes().iter().map(|&index| &SCOPES[index as usize])
    }
}

/// True when `scope` and every ancestor are open.
#[must_use]
pub fn is_open(scope: ScopeId) -> bool {
    Chain::of(scope)
        .iter()
        .all(|node| node.state() == State::Open)
}

/// True when `candidate` is `ancestor` or below it.
#[must_use]
pub fn is_within(ancestor: ScopeId, candidate: ScopeId) -> bool {
    Chain::of(candidate).nodes().contains(&ancestor)
}

/// Adds `amount` to one counter with a ceiling, or leaves it untouched.
///
/// The check and the add are one atomic step from every observer's point of
/// view: the value the counter held before the add is what `fetch_add`
/// returns, and if that plus the amount is past the ceiling the add is undone
/// before returning. Another processor may see the counter above the ceiling
/// for the duration of the undo; nothing it can do with that reading admits
/// anything, because its own increment is checked the same way.
fn bounded_add(counter: &AtomicU64, amount: u64, limit: u64) -> Result<u64, u64> {
    let previous = counter.fetch_add(amount, Ordering::AcqRel);
    let next = previous.saturating_add(amount);
    if next > limit {
        counter.fetch_sub(amount, Ordering::AcqRel);
        return Err(previous);
    }
    Ok(next)
}

/// Reserves `amount` of `resource` in `scope` and every ancestor, or nothing.
///
/// Every level is checked as it is charged, and a refusal at any level undoes
/// the levels below it, so no counter ends up above its ceiling and a refusal
/// leaves no charge behind. A closed scope refuses like an exhausted one.
pub fn reserve(scope: ScopeId, resource: Resource, amount: u64) -> bool {
    let chain = Chain::of(scope);
    let nodes = chain.nodes();
    let mut done = 0;
    let mut binding = None;
    for &index in nodes {
        let node = &SCOPES[index as usize];
        let refused = node.state() != State::Open
            || bounded_add(node.account(resource), amount, node.limit(resource)).is_err();
        if refused {
            binding = Some(index);
            break;
        }
        done += 1;
    }
    let Some(binding) = binding else {
        return true;
    };
    for &index in &nodes[..done] {
        SCOPES[index as usize]
            .account(resource)
            .fetch_sub(amount, Ordering::AcqRel);
    }
    // Which scope and which resource, not merely that something was refused.
    // A limit that is only ever reported as a status code is a limit whose
    // accounting nobody can check: the record names the ancestor that
    // actually bound the request, which is not always the one the caller
    // addressed.
    let node = &SCOPES[binding as usize];
    crate::trace!(
        "scope.limit_refused",
        "scope={scope} binding_scope={binding} binding_id={} resource={} \
         requested={amount} used={} limit={} state={}",
        node.id(),
        resource.name(),
        node.used(resource),
        node.limit(resource),
        node.state().name()
    );
    false
}

/// Returns `amount` of `resource` to `scope` and every ancestor.
pub fn release(scope: ScopeId, resource: Resource, amount: u64) {
    for node in Chain::of(scope).iter() {
        let counter = node.account(resource);
        // Saturating at zero, as the old arithmetic did: a release that
        // exceeds what was charged is a bookkeeping error to report, not a
        // wrap to give the scope a fortune.
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(amount);
            match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
    }
}

/// Serialises window rolls and lets a reservation notice that a roll happened
/// while it was being made.
///
/// A seqlock in the sense Linux uses the word: the roller increments the
/// sequence to odd, changes the counters, increments to even. A reservation
/// reads the sequence before and after its increment; a change means the
/// increment may have landed in either window, and it is undone and retried
/// rather than left in one of them by accident.
static ROLL: SpinLock<()> = SpinLock::new(());
static ROLL_SEQ: AtomicU64 = AtomicU64::new(0);

/// Moves every scope to the window `now` belongs to.
///
/// An overrun is not forgiven by the window boundary: it starts the next window
/// already consumed, which is what "the excess is deducted from the next
/// replenishment" has to mean if it is to mean anything.
///
/// Called by any processor that notices a scope in an old window. Rolling is
/// idempotent and cheap, and it is serialised by [`ROLL`], so four processors
/// noticing the same boundary make the same progress one would, three of them
/// finding nothing left to do.
pub fn roll_window(now_ns: u64) {
    let window = now_ns / CPU_WINDOW_NS;
    if !SCOPES.iter().any(|node| {
        node.state() != State::Empty && node.window_index.load(Ordering::Relaxed) != window
    }) {
        return;
    }
    let _guard = ROLL.lock();
    // Every processor's held charges are pulled in before any window closes,
    // and not scope by scope as each one is judged. A charge against a child
    // is a charge against its ancestors too, so flushing a child after its
    // parent had already rolled would put the child's execution in the
    // parent's *next* window -- which made the root's closed windows report
    // more execution than four processors can perform in one.
    for index in 0..MAX_SCOPES {
        if SCOPES[index].state() != State::Empty {
            crate::sched::flush_pending_charges(index as ScopeId);
        }
    }
    for index in 0..MAX_SCOPES {
        let node = &SCOPES[index];
        if node.state() == State::Empty || node.window_index.load(Ordering::Relaxed) == window {
            continue;
        }
        let closing = node.cpu_window_ns.load(Ordering::Relaxed);
        let budget = node.limits.cpu_budget_ns();
        let overrun = closing.saturating_sub(budget);
        // Written for the first overruns of a scope and counted after that.
        // One line per window was a line per ten milliseconds per spinning
        // scope, written from the timer with the machine lock held; under KVM
        // each line cost about a millisecond of port I/O, the run lasted
        // longer, more windows closed in debt, and the records fed on
        // themselves until a drain that should have finished timed out.
        let written = overrun != 0 && node.debt_records.load(Ordering::Relaxed) < DEBT_RECORD_LIMIT;
        if written {
            // Carried debt that nobody can see is not a limit, it is a number.
            // The record is emitted only when a window actually closed over
            // budget, so it says something happened rather than that a window
            // went by.
            crate::trace!(
                "scope.debt",
                "scope={index} id={} label={} window={window} overrun_ns={overrun} \
                 debt_ns={} budget_ns={budget} carried_into_next=1",
                node.id(),
                node.label_str(),
                node.cpu_debt_ns
                    .load(Ordering::Relaxed)
                    .saturating_add(overrun)
            );
            node.debt_records.fetch_add(1, Ordering::Relaxed);
        }
        node.windows_closed.fetch_add(1, Ordering::Relaxed);
        node.max_window_ns.fetch_max(closing, Ordering::Relaxed);
        // What this window actually executed, and what it was allowed to
        // admit. Their difference is the only part of the overrun the
        // reservation is answerable for.
        let opened = node.window_opened_ns.load(Ordering::Relaxed);
        let charged = closing.saturating_sub(opened);
        let admissible = budget.saturating_sub(opened);
        node.max_charged_in_window_ns
            .fetch_max(charged, Ordering::Relaxed);
        node.max_excess_ns
            .fetch_max(charged.saturating_sub(admissible), Ordering::Relaxed);
        if overrun != 0 {
            node.overruns.fetch_add(1, Ordering::Relaxed);
            node.max_overrun_ns.fetch_max(overrun, Ordering::Relaxed);
        }
        node.cpu_debt_ns.fetch_add(overrun, Ordering::Relaxed);
        // The counters of the new window are written between two increments
        // of the sequence, so a reservation that overlapped the change knows
        // to redo itself. A reservation belongs to the window it was made in.
        // Carrying it across the boundary would credit the new window with
        // capacity the old one had promised, which is the delayed-settlement
        // mistake the scheduling groundwork names; the credit that made it
        // finds its window gone and starts afresh.
        ROLL_SEQ.fetch_add(1, Ordering::AcqRel);
        node.cpu_window_ns.store(overrun, Ordering::Relaxed);
        node.cpu_committed_ns.store(overrun, Ordering::Relaxed);
        node.window_opened_ns.store(overrun, Ordering::Relaxed);
        node.closure_used_ns.store(0, Ordering::Relaxed);
        node.closure_committed_ns.store(0, Ordering::Relaxed);
        node.window_index.store(window, Ordering::Release);
        ROLL_SEQ.fetch_add(1, Ordering::AcqRel);
    }
}

/// Charges executed time to `scope` and every ancestor.
///
/// `recovery` charges the closure account instead of the ordinary one, so the
/// cost of finishing an already admitted obligation is visible as recovery
/// spending rather than hidden inside normal consumption.
///
/// The ordinary path is batched per processor by the scheduler and lands here
/// through [`crate::sched::flush_pending_charges`]; the closure path is charged
/// directly, because recovery work is rare.
pub fn charge_cpu(scope: ScopeId, ns: u64, recovery: bool) {
    if ns == 0 {
        return;
    }
    for node in Chain::of(scope).iter() {
        node.cpu_total_ns.fetch_add(ns, Ordering::Relaxed);
        if recovery {
            node.closure_used_ns.fetch_add(ns, Ordering::Relaxed);
        } else {
            node.cpu_window_ns.fetch_add(ns, Ordering::Relaxed);
        }
    }
}

/// Adds execution that ran beyond its reservation to the committed total, so
/// "committed" keeps meaning "charged plus promised".
pub fn commit_excess(scope: ScopeId, ns: u64, recovery: bool) {
    if ns == 0 {
        return;
    }
    for node in Chain::of(scope).iter() {
        if recovery {
            node.closure_committed_ns.fetch_add(ns, Ordering::Relaxed);
        } else {
            node.cpu_committed_ns.fetch_add(ns, Ordering::Relaxed);
        }
    }
}

/// Execution `scope` and its ancestors can still promise this window.
///
/// The minimum over the chain, because a reservation has to fit in every
/// ancestor, and zero when any of them is closed to new work.
#[must_use]
pub fn available_ns(scope: ScopeId, recovery: bool) -> u64 {
    let mut available = u64::MAX;
    for node in Chain::of(scope).iter() {
        let state = node.state();
        let usable = if recovery {
            !matches!(state, State::Retired | State::Empty)
        } else {
            matches!(state, State::Open | State::Fenced)
        };
        if !usable {
            return 0;
        }
        let (committed, limit) = if recovery {
            (
                node.closure_committed_ns.load(Ordering::Relaxed),
                node.limits.closure_reserve_ns(),
            )
        } else {
            (
                node.cpu_committed_ns.load(Ordering::Relaxed),
                node.limits.cpu_budget_ns(),
            )
        };
        available = available.min(limit.saturating_sub(committed));
    }
    if available == u64::MAX { 0 } else { available }
}

/// Takes `amount` of execution in `scope` and every ancestor for window
/// `window`, or takes nothing.
///
/// This is the reservation the resource contract requires before a dispatch.
/// Each level is checked as it is committed; a level that cannot pay undoes
/// the levels already taken. A roll of the window that overlaps the
/// reservation is detected through the roll sequence and the reservation is
/// redone against the new window's counters, so a promise never straddles two
/// windows.
pub fn reserve_cpu(scope: ScopeId, amount: u64, recovery: bool, window: u64) -> bool {
    let chain = Chain::of(scope);
    let nodes = chain.nodes();
    loop {
        let sequence = ROLL_SEQ.load(Ordering::Acquire);
        if sequence & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        if SCOPES[scope as usize].window_index.load(Ordering::Acquire) != window {
            return false;
        }
        let mut done = 0;
        let mut refused = false;
        let mut peak = 0u64;
        for &index in nodes {
            let node = &SCOPES[index as usize];
            let (counter, limit) = if recovery {
                (&node.closure_committed_ns, node.limits.closure_reserve_ns())
            } else {
                (&node.cpu_committed_ns, node.limits.cpu_budget_ns())
            };
            match bounded_add(counter, amount, limit) {
                Ok(next) => {
                    if index == scope {
                        peak = next;
                    }
                    done += 1;
                }
                Err(_) => {
                    refused = true;
                    break;
                }
            }
        }
        if refused || ROLL_SEQ.load(Ordering::Acquire) != sequence {
            for &index in &nodes[..done] {
                let node = &SCOPES[index as usize];
                let counter = if recovery {
                    &node.closure_committed_ns
                } else {
                    &node.cpu_committed_ns
                };
                counter.fetch_sub(amount, Ordering::AcqRel);
            }
            if refused {
                return false;
            }
            continue;
        }
        if !recovery {
            // Recorded at every level where the commitment was made, so a
            // parent's peak reflects what its children promised together.
            for &index in nodes {
                let node = &SCOPES[index as usize];
                let committed = if index == scope {
                    peak
                } else {
                    node.cpu_committed_ns.load(Ordering::Relaxed)
                };
                node.max_committed_ns
                    .fetch_max(committed, Ordering::Relaxed);
            }
        }
        return true;
    }
}

/// Returns `amount` of execution reserved for `window` to `scope` and every
/// ancestor.
///
/// If the window has moved on, the reservation was already discarded at the
/// boundary and there is nothing to return.
pub fn return_cpu(scope: ScopeId, amount: u64, recovery: bool, window: u64) {
    if amount == 0 {
        return;
    }
    for node in Chain::of(scope).iter() {
        if node.window_index.load(Ordering::Acquire) != window {
            continue;
        }
        let counter = if recovery {
            &node.closure_committed_ns
        } else {
            &node.cpu_committed_ns
        };
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(amount);
            match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
    }
}

/// Takes one simultaneity slot in `scope` and every ancestor, or none.
pub fn take_running(scope: ScopeId) -> bool {
    let chain = Chain::of(scope);
    let nodes = chain.nodes();
    let mut done = 0;
    for &index in nodes {
        let node = &SCOPES[index as usize];
        let limit = node.limits.parallelism();
        let previous = node.running.fetch_add(1, Ordering::AcqRel);
        if previous >= limit {
            node.running.fetch_sub(1, Ordering::AcqRel);
            break;
        }
        node.max_running.fetch_max(previous + 1, Ordering::Relaxed);
        done += 1;
    }
    if done == nodes.len() {
        return true;
    }
    for &index in &nodes[..done] {
        SCOPES[index as usize]
            .running
            .fetch_sub(1, Ordering::AcqRel);
    }
    false
}

/// Returns one simultaneity slot to `scope` and every ancestor.
pub fn drop_running(scope: ScopeId) {
    for node in Chain::of(scope).iter() {
        let mut current = node.running.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(1);
            match node.running.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
    }
}

/// Highest simultaneity any scope in the table ever held.
#[must_use]
pub fn peak_running() -> u32 {
    SCOPES
        .iter()
        .map(|node| node.max_running.load(Ordering::Relaxed))
        .max()
        .unwrap_or(0)
}

/// True when `scope` and every ancestor have a free parallelism slot.
///
/// The limit is checked when a thread starts charging a scope, not when it is
/// dispatched: a running thread already holds its own slot, so checking at
/// dispatch would make every thread ineligible against itself. On one core the
/// limit cannot be exceeded by execution anyway; what it bounds is how many
/// threads may be charging one scope at all, borrowed workers included.
#[must_use]
pub fn has_parallelism(scope: ScopeId) -> bool {
    Chain::of(scope)
        .iter()
        .all(|node| node.parallelism_used.load(Ordering::Relaxed) < node.limits.parallelism())
}

/// Takes a parallelism slot in `scope` and every ancestor.
pub fn take_parallelism(scope: ScopeId) {
    for node in Chain::of(scope).iter() {
        node.parallelism_used.fetch_add(1, Ordering::AcqRel);
    }
}

/// Returns a parallelism slot to `scope` and every ancestor.
pub fn drop_parallelism(scope: ScopeId) {
    for node in Chain::of(scope).iter() {
        let mut current = node.parallelism_used.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(1);
            match node.parallelism_used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
    }
}

/// Decrements a counter, saturating at zero.
pub fn decrement(counter: &AtomicU32) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(1);
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(seen) => current = seen,
        }
    }
}

/// Subtracts from a 32-bit counter, saturating at zero.
pub fn subtract32(counter: &AtomicU32, amount: u32) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(seen) => current = seen,
        }
    }
}

/// Subtracts from a counter, saturating at zero.
pub fn subtract(counter: &AtomicU64, amount: u64) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => break,
            Err(seen) => current = seen,
        }
    }
}

/// Notes progress on a scope's drain: a token bump and a timestamp.
pub fn note_progress(scope: ScopeId, now: u64) {
    let node = &SCOPES[scope as usize];
    node.last_progress_ns.store(now, Ordering::Relaxed);
    node.drain_token.fetch_add(1, Ordering::Relaxed);
}

/// Places the barrier on `scope` and every descendant, returning how many
/// scopes changed state.
///
/// This is the short operation the authority contract requires: it publishes a
/// monotonic state that every later admission observes. Walking descendants,
/// stopping threads and reclaiming happen outside it. Under the machine lock.
pub fn fence(root: ScopeId) -> u32 {
    let mut changed = 0;
    for index in 0..MAX_SCOPES {
        let node = &SCOPES[index];
        if node.state() != State::Open {
            continue;
        }
        if !is_within(root, index as ScopeId) {
            continue;
        }
        node.set_state(State::Fenced);
        node.drain_token.fetch_add(1, Ordering::Relaxed);
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
pub fn pending(root: ScopeId) -> Pending {
    let mut total = Pending::default();
    for index in 0..MAX_SCOPES {
        let node = &SCOPES[index];
        if node.state() == State::Empty || !is_within(root, index as ScopeId) {
            continue;
        }
        // Live executions, not the ones charging the scope at this instant. A
        // thread that exists but is not currently dispatched is still something
        // a drain has to wait for; counting only the dispatched ones would
        // declare quiescence with a domain of the scope still alive.
        total.threads += node.threads.load(Ordering::Relaxed);
        total.invocations += node.invocations_pending.load(Ordering::Relaxed);
        total.effects += node.effects_pending.load(Ordering::Relaxed);
        total.maps += node.maps_pending.load(Ordering::Relaxed);
        total.undelivered += node.undelivered_cancelled.load(Ordering::Relaxed);
    }
    total.pages = SCOPES[root as usize].memory_pages.load(Ordering::Relaxed);
    total.metadata = SCOPES[root as usize].metadata.load(Ordering::Relaxed);
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
    let node = &SCOPES[index];
    if node.state() != State::Retired
        || node.refs.load(Ordering::Relaxed) != 0
        || node.threads.load(Ordering::Relaxed) != 0
    {
        return false;
    }
    let (id, parent) = (node.id(), node.parent());
    let has_child = SCOPES
        .iter()
        .any(|other| other.state() != State::Empty && other.parent() == Some(index as ScopeId));
    if has_child {
        return false;
    }
    machine.allocator().reassign_quarantine(
        crate::mm::Owner::Scope(index as u16),
        crate::mm::Owner::Kernel,
    );
    crate::sched::forget_credits(index as ScopeId);
    crate::trace!("scope.collected", "scope={index} id={id}");
    if let Some(parent) = parent {
        decrement(&SCOPES[parent as usize].children);
    }
    node.reset();
    true
}

/// Promotes fenced scopes with nothing left to observe into `Quiescent`.
///
/// Quiescence is derived from counters the kernel maintains, never from a
/// timeout: a wait that expires leaves the state exactly where it was. Under
/// the machine lock, because it changes state.
pub fn advance_quiescence(now_ns: u64) -> u32 {
    let mut promoted = 0;
    for index in 0..MAX_SCOPES {
        let node = &SCOPES[index];
        if node.state() != State::Fenced {
            continue;
        }
        let obligations = pending(index as ScopeId);
        if obligations.threads == 0
            && obligations.invocations == 0
            && obligations.effects == 0
            && obligations.maps == 0
        {
            node.set_state(State::Quiescent);
            node.last_progress_ns.store(now_ns, Ordering::Relaxed);
            node.drain_token.fetch_add(1, Ordering::Relaxed);
            promoted += 1;
        }
    }
    promoted
}

/// Whether any fenced scope exists, so a tick knows whether promotion is
/// worth scanning for.
#[must_use]
pub fn any_fenced() -> bool {
    SCOPES.iter().any(|node| node.state() == State::Fenced)
}

const _: () = assert!(MAX_CPUS <= 64);
const _: () = assert!(MAX_SCOPES < NO_SCOPE as usize);

/// How much a processor takes out of the pool at once, and the most it may
/// promise to one dispatch.
///
/// A slice, not a share. The point of the local credit is to keep the common
/// dispatch off the shared counters, not to give a processor a stake in the
/// budget: a processor that holds a large credit holds capacity its siblings
/// are refused for, and four processors holding a quarter of the budget each
/// starve the fifth thread while the scope is nowhere near its ceiling --
/// which is what the measurement showed. Linux draws the same slice for the
/// same reason, `sched_cfs_bandwidth_slice_us`, five milliseconds against a
/// hundred-millisecond period. This is two quanta against the window, and the
/// unspent remainder goes back to the pool as soon as the processor has
/// nothing of the scope to run.
#[must_use]
pub fn credit_refill_ns(_scope: ScopeId, window_left: u64) -> u64 {
    (2 * crate::sched::QUANTUM_NS).min(window_left)
}
