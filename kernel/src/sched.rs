//! Scheduling on more than one processor.
//!
//! The mechanism is preemptive round-robin over threads, one run queue per
//! processor, and the policy is the resource contract's: a thread runs only if
//! the scope its current work is charged to, **and every ancestor of that
//! scope**, can still pay for it. Budget is aggregate, so two workers that
//! adopted the same client's scope draw from one balance whether they run on
//! one processor or on two.
//!
//! Three things make this a scheduler for several processors rather than one
//! scheduler with several processors waiting on its lock:
//!
//! * **A balance is reserved, not merely checked.** Two processors reading the
//!   same remaining budget would both conclude they may run, and the scope
//!   would spend twice what it has. A dispatch takes the time out of the
//!   scope and every ancestor before it runs, and gives back what it did not
//!   use when it stops. The reservation is taken through a per-processor
//!   *credit* (see `crate::scope`): the processor reserves a slice from the
//!   pool once and dispatches out of it, so the ordinary context switch
//!   touches nothing another processor is touching.
//! * **A thread being switched away from is not available yet.** Its stopped
//!   context is written by the `switch_context` call itself, after the run
//!   queue lock is released. Another processor that picked it in that window
//!   would resume it from a stack pointer that had not been stored. So the
//!   outgoing thread stays marked as standing on this processor until the
//!   incoming context publishes it, which is what [`finish_switch`] does, and
//!   a wake that finds a thread still standing on a processor waits for that
//!   publication before it enqueues it anywhere -- the `on_cpu` handshake
//!   Linux's `try_to_wake_up` performs for the same reason.
//! * **A wake says what the waker is about to do.** A caller that admitted a
//!   message and will now block for its reply hands its processor to the
//!   receiver directly: the receiver goes to the head of *this* processor's
//!   queue and is the next thing dispatched, with no interrupt and no
//!   migration, which is Linux's `WF_SYNC` with the one thing Linux cannot
//!   know -- that the waker really is about to block. A wake whose waker keeps
//!   running goes to an idle processor at once, with an interrupt only if
//!   that processor has halted.
//!
//! Windows are fixed and aligned, of the period the contract fixes. An overrun
//! is not forgiven at the boundary: it starts the next window already consumed.
//! A reservation, in contrast, does not cross the boundary at all -- it belongs
//! to the window that made it, and a settlement arriving afterwards finds
//! nothing to return rather than crediting the new window with the old one's
//! capacity.
//!
//! Preemption stays involuntary by construction. A user thread is switched away
//! from inside the timer interrupt of the processor it is running on, at an
//! instruction it did not choose, and the interrupted frame is recorded so the
//! claim can be checked rather than believed.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::arch::x86_64::{context, cpu, fpu, gdt, trap};
use crate::limits::{CPU_WINDOW_NS, MAX_CPUS, MAX_SCOPES, MAX_THREADS};
use crate::obj::ScopeId;
use crate::percpu;
use crate::scope;
use crate::state::MACHINE;
use crate::sync::SpinLock;
use crate::thread::{self, ThreadKind, ThreadState, idle_thread};
use crate::{event, trace};
use crate::{smp, time, tlb};

/// Timer interrupts per second.
pub const TICK_HZ: u64 = 2000;
/// Scheduling quantum. The value the resource contract fixes for V0.
pub const QUANTUM_NS: u64 = thalyx_abi::limit::CPU_QUANTUM_NS;
/// Quantum expressed in timer ticks.
pub const QUANTUM_TICKS: u32 = (QUANTUM_NS * TICK_HZ / 1_000_000_000) as u32;

const _: () = assert!(QUANTUM_TICKS >= 1);
const _: () = assert!(MAX_THREADS <= 64);

/// Detailed preemption records emitted per thread before the diagnostic plane
/// starts coalescing them into counters. The bound is per thread rather than
/// global so one busy domain cannot consume the whole budget and leave another
/// domain's preemptions unrecorded. The observability contract permits
/// coalescing; it requires saying so, which the summary record does.
pub const PREEMPT_RECORD_LIMIT: u32 = 8;

/// Detailed migration records emitted before the plane coalesces them.
pub const MIGRATION_RECORD_LIMIT: u64 = 16;

/// How long a synchronous wake stays with the processor that made it before
/// another takes it: the few microseconds a server needs to reply, close and
/// receive. A V0 parameter, and a trade K6 measured: the synchronous round
/// trip against the wake-to-run latency. With the wake itself now delivered
/// to an idle processor directly, only the synchronous case pays this.
pub const WAKE_GRACE_NS: u64 = 10_000;

/// How long an idle processor watches for work before halting.
///
/// Under hardware virtualization a halt and the interrupt that ends it are
/// each a trip through the host; a processor that keeps looking for a while
/// takes a wake that arrives soon without either. Two hundred microseconds is
/// what a Linux guest's `haltpoll` governor polls for by default, so the two
/// sides of the comparison idle the same way.
pub const IDLE_POLL_NS: u64 = 200_000;

/// A thread index that names no thread.
const NO_THREAD: usize = usize::MAX;

/// What a waker knows about what it will do next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WakeHint {
    /// The waker is about to block or yield: the woken thread goes to the
    /// head of this processor's queue and runs here, with no interrupt.
    Sync,
    /// The waker keeps running: the woken thread goes to an idle processor.
    Any,
}

/// The run queue of one processor.
pub struct RunQueue {
    /// Threads ready to run here, one bit per thread-table index.
    ready: u64,
    /// Round-robin cursor over the thread table.
    cursor: usize,
    /// A thread handed to this processor by a synchronous wake: the next
    /// thing to dispatch, unless fairness says otherwise.
    hint: usize,
}

/// A processor's credit for one scope: execution reserved from the pool for
/// the window named, not yet handed to a thread, and the simultaneity slot
/// that goes with it.
pub struct Credit {
    /// Window the reservation belongs to.
    window: AtomicU64,
    /// Nanoseconds reserved and not yet dispatched.
    reserved_ns: AtomicU64,
    /// Nanoseconds executed here and not yet flushed to the pool.
    pending_ns: AtomicU64,
}

impl Credit {
    const fn empty() -> Self {
        Self {
            window: AtomicU64::new(0),
            reserved_ns: AtomicU64::new(0),
            pending_ns: AtomicU64::new(0),
        }
    }
}

/// What one processor is doing, as the scheduler sees it.
pub struct CpuState {
    /// The run queue.
    rq: SpinLock<RunQueue>,
    /// Thread currently dispatched here.
    pub current: AtomicUsize,
    /// Thread this processor switched away from and has not yet published as
    /// ready again.
    pub previous: AtomicUsize,
    /// Whether the processor completed its handshake and may be scheduled on.
    pub online: AtomicBool,
    /// Local APIC identifier, as the firmware reported it.
    pub apic_id: AtomicU32,
    /// `IDLE_RUNNING`, `IDLE_POLLING` or `IDLE_HALTED`.
    idle: AtomicU8,
    /// A thread was enqueued here while this processor ran something else
    /// that keeps running; settled on the next interrupt or entry.
    need_resched: AtomicBool,
    /// Time-stamp counter when the pending synchronous wake was made, or zero.
    sync_since: AtomicU64,
    /// Set when every thread this processor could have run belongs to a scope
    /// that is out of budget for the current window. Cleared by the window
    /// roll and by anything that makes a new thread runnable here.
    throttled: AtomicBool,
    /// Last value this processor wrote to `IA32_FS_BASE`.
    fs_base: AtomicU64,
    /// Time-stamp counter value at which the current dispatch's reservation
    /// runs out, or `u64::MAX` when nothing user-visible is dispatched here.
    ///
    /// In counter units rather than nanoseconds so that the check on the way
    /// back to user mode is one `rdtsc` and one comparison. It is the same
    /// instant as the thread's `reservation_end_ns`, converted once when the
    /// dispatch is made rather than on every kernel exit.
    deadline_tsc: AtomicU64,
    /// Timer interrupts this processor took.
    pub ticks: AtomicU64,
    /// Times this processor dispatched a user thread.
    pub dispatches: AtomicU64,
    /// Times this processor took a user thread away involuntarily.
    pub preemptions: AtomicU64,
    /// Dispatches this processor refused for want of budget or a parallelism
    /// slot.
    pub budget_stalls: AtomicU64,
    /// Nanoseconds of user execution this processor charged.
    pub user_ns: AtomicU64,
    /// Reschedule interrupts this processor sent to others.
    pub kicks: AtomicU64,
    /// Credits, one per scope.
    credits: [Credit; MAX_SCOPES],
}

const IDLE_RUNNING: u8 = 0;
const IDLE_POLLING: u8 = 1;
const IDLE_HALTED: u8 = 2;

impl CpuState {
    const fn empty() -> Self {
        Self {
            rq: SpinLock::new(RunQueue {
                ready: 0,
                cursor: 0,
                hint: NO_THREAD,
            }),
            current: AtomicUsize::new(NO_THREAD),
            previous: AtomicUsize::new(NO_THREAD),
            online: AtomicBool::new(false),
            apic_id: AtomicU32::new(u32::MAX),
            idle: AtomicU8::new(IDLE_RUNNING),
            need_resched: AtomicBool::new(false),
            sync_since: AtomicU64::new(0),
            throttled: AtomicBool::new(false),
            fs_base: AtomicU64::new(0),
            deadline_tsc: AtomicU64::new(u64::MAX),
            ticks: AtomicU64::new(0),
            dispatches: AtomicU64::new(0),
            preemptions: AtomicU64::new(0),
            budget_stalls: AtomicU64::new(0),
            user_ns: AtomicU64::new(0),
            kicks: AtomicU64::new(0),
            credits: [const { Credit::empty() }; MAX_SCOPES],
        }
    }

    /// Whether the processor completed its handshake.
    #[must_use]
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }
}

static CPUS: [CpuState; MAX_CPUS] = [const { CpuState::empty() }; MAX_CPUS];

/// Processors idle right now, polling or halted.
static IDLE_MASK: AtomicU64 = AtomicU64::new(0);
/// Processors halted, which need an interrupt to notice anything.
static HALTED_MASK: AtomicU64 = AtomicU64::new(0);
/// Earliest deadline of any timed wait, or `u64::MAX`.
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);
/// The window the machine is in, as last rolled to.
static CURRENT_WINDOW: AtomicU64 = AtomicU64::new(0);
/// Detailed preemption records already emitted, before coalescing.
static PREEMPT_RECORDS: AtomicU32 = AtomicU32::new(0);
/// Longest interval a single charge covered.
///
/// A quantum bounds how long a thread is *scheduled* for; this is how long
/// it was actually charged for between two observations of the clock. Under
/// an emulated platform the two are not the same number, and a budget
/// overrun is only attributable if the difference is measured rather than
/// assumed.
static MAX_CHARGE_INTERVAL_NS: AtomicU64 = AtomicU64::new(0);
/// Processors that ever completed their handshake. Never decremented.
static CPUS_STARTED: AtomicUsize = AtomicUsize::new(0);
/// Processors online right now.
static CPUS_ONLINE: AtomicUsize = AtomicUsize::new(0);

/// The state of processor `cpu`.
#[must_use]
pub fn cpu_state(cpu: usize) -> &'static CpuState {
    &CPUS[cpu.min(MAX_CPUS - 1)]
}

/// Processors that completed their handshake, the bootstrap processor
/// included.
#[must_use]
pub fn cpus_online() -> usize {
    CPUS_ONLINE.load(Ordering::Acquire)
}

/// Processors that ever completed it.
#[must_use]
pub fn cpus_started() -> usize {
    CPUS_STARTED.load(Ordering::Acquire)
}

/// Publishes processor `cpu` as online, running its idle thread.
pub fn set_online(cpu: usize, apic_id: u32) {
    let state = cpu_state(cpu);
    state.apic_id.store(apic_id, Ordering::Release);
    state.current.store(idle_thread(cpu), Ordering::Release);
    state.previous.store(NO_THREAD, Ordering::Release);
    state.online.store(true, Ordering::Release);
    CPUS_ONLINE.fetch_add(1, Ordering::AcqRel);
    CPUS_STARTED.fetch_add(1, Ordering::AcqRel);
}

/// Removes processor `cpu` from scheduling.
pub fn set_offline(cpu: usize) {
    cpu_state(cpu).online.store(false, Ordering::Release);
    CPUS_ONLINE.fetch_sub(1, Ordering::AcqRel);
    IDLE_MASK.fetch_and(!(1u64 << cpu), Ordering::AcqRel);
    HALTED_MASK.fetch_and(!(1u64 << cpu), Ordering::AcqRel);
}

/// Thread dispatched on the processor executing this call.
///
/// Reading the processor's own identity rather than a single global field
/// is the whole difference between a uniprocessor scheduler and this one:
/// there is no "the" running thread any more.
#[inline]
#[must_use]
pub fn current_thread() -> usize {
    let cpu = percpu::index();
    let index = cpu_state(cpu).current.load(Ordering::Relaxed);
    if index == NO_THREAD {
        idle_thread(cpu)
    } else {
        index
    }
}

/// Timer ticks, preemptions, budget stalls, preemption records and the
/// longest charge interval, for the run's summary.
#[must_use]
pub fn totals() -> (u64, u64, u64, u32, u64) {
    // Summed from the processors' own counters rather than kept in three
    // machine-wide words. A counter every processor increments on every tick
    // is a cache line every processor takes exclusively thousands of times a
    // second, for a number nothing reads until the run ends.
    let mut ticks = 0;
    let mut preemptions = 0;
    let mut stalls = 0;
    for state in &CPUS {
        ticks += state.ticks.load(Ordering::Relaxed);
        preemptions += state.preemptions.load(Ordering::Relaxed);
        stalls += state.budget_stalls.load(Ordering::Relaxed);
    }
    (
        ticks,
        preemptions,
        stalls,
        PREEMPT_RECORDS.load(Ordering::Relaxed),
        MAX_CHARGE_INTERVAL_NS.load(Ordering::Relaxed),
    )
}

/// Sum of every thread's migrations.
#[must_use]
pub fn migrations() -> u64 {
    thread::iter()
        .map(|(_, cell)| {
            // SAFETY: a read of one counter for a summary; a torn value is
            // impossible on this architecture and the number is diagnostic.
            unsafe { cell.sched().migrations }
        })
        .sum()
}

/// Records a timed wait's deadline so the tick knows when to look.
pub fn note_deadline(deadline_ns: u64) {
    NEXT_DEADLINE.fetch_min(deadline_ns, Ordering::AcqRel);
}

#[inline]
const fn bit(index: usize) -> u64 {
    1u64 << index
}

// ------------------------------------------------------------------ credits

/// Flushes the execution every processor charged locally to `scope` into the
/// pool. Called by the window roll before it judges the closing window, and
/// by a query that wants the exact total.
pub fn flush_pending_charges(scope: ScopeId) {
    for state in &CPUS {
        let pending = state.credits[scope as usize]
            .pending_ns
            .swap(0, Ordering::AcqRel);
        scope::charge_cpu(scope, pending, false);
    }
}

/// Execution charged to `scope` on every processor and not yet flushed.
#[must_use]
pub fn pending_charges(scope: ScopeId) -> u64 {
    CPUS.iter()
        .map(|state| {
            state.credits[scope as usize]
                .pending_ns
                .load(Ordering::Relaxed)
        })
        .sum()
}

/// Drops every processor's credit for a scope whose slot is being reused.
pub fn forget_credits(scope: ScopeId) {
    for state in &CPUS {
        let credit = &state.credits[scope as usize];
        credit.reserved_ns.store(0, Ordering::Relaxed);
        credit.pending_ns.store(0, Ordering::Relaxed);
        credit.window.store(0, Ordering::Relaxed);
    }
}

/// Gives back everything this processor holds for `scope`: the unspent
/// reservation to the pool and the pending charge to the accounts.
fn release_credit(cpu: usize, scope: ScopeId) {
    let credit = &CPUS[cpu].credits[scope as usize];
    let window = credit.window.load(Ordering::Relaxed);
    let reserved = credit.reserved_ns.swap(0, Ordering::AcqRel);
    if reserved != 0 {
        scope::return_cpu(scope, reserved, false, window);
    }
    let pending = credit.pending_ns.swap(0, Ordering::AcqRel);
    scope::charge_cpu(scope, pending, false);
}

/// Gives back every credit this processor holds, because it is about to halt
/// and a reservation held by a halted processor is capacity nobody can use.
fn release_credits(cpu: usize) {
    for scope in 0..MAX_SCOPES {
        let credit = &CPUS[cpu].credits[scope];
        if credit.reserved_ns.load(Ordering::Relaxed) != 0
            || credit.pending_ns.load(Ordering::Relaxed) != 0
        {
            release_credit(cpu, scope as ScopeId);
        }
    }
}

/// Takes the reservation one dispatch needs out of this processor's credit,
/// refilling the credit from the pool when it runs low.
///
/// Returns the execution granted, or zero when the scope or an ancestor
/// cannot pay. Selection and reservation are one step under the run-queue
/// lock: a thread chosen against a balance another processor has already
/// promised away is the scheduling groundwork's exact counterexample.
fn take_grant(cpu: usize, index: usize, now: u64) -> u64 {
    let cell = thread::get(index);
    let scope = cell.effective_scope();
    let recovery = cell.is_recovery();
    let window = now / CPU_WINDOW_NS;
    let window_left = CPU_WINDOW_NS - now % CPU_WINDOW_NS;
    if recovery {
        // Recovery work is rare and charged straight to the pool.
        let amount = QUANTUM_NS
            .min(scope::available_ns(scope, true))
            .min(window_left);
        if amount == 0 || !scope::reserve_cpu(scope, amount, true, window) {
            return 0;
        }
        return amount;
    }
    let credit = &CPUS[cpu].credits[scope as usize];
    if credit.window.load(Ordering::Relaxed) != window {
        // The pool discarded this reservation at the boundary; the charge it
        // accumulated belongs to whichever window it lands in now, as a
        // charge at a tick would.
        credit.reserved_ns.store(0, Ordering::Relaxed);
        let pending = credit.pending_ns.swap(0, Ordering::AcqRel);
        scope::charge_cpu(scope, pending, false);
        credit.window.store(window, Ordering::Relaxed);
    }
    let ceiling = scope::credit_refill_ns(scope, window_left);
    let mut have = credit.reserved_ns.load(Ordering::Relaxed);
    if have < QUANTUM_NS.min(window_left) {
        let available = scope::available_ns(scope, false);
        // Never more than half of what the scope has left. A credit is a
        // convenience for this processor and a refusal for every other one:
        // taking the last of a budget into a local reserve makes a scope look
        // exhausted to its siblings while the reserve sits unspent, and a
        // sibling that is refused waits for the window rather than for a
        // quantum.
        let want = ceiling
            .saturating_sub(have)
            .min(available)
            .min(available.div_ceil(2));
        if want == 0 {
            if have == 0 {
                return 0;
            }
        } else if scope::reserve_cpu(scope, want, false, window) {
            have = credit.reserved_ns.fetch_add(want, Ordering::AcqRel) + want;
        } else if have == 0 {
            return 0;
        }
    }
    loop {
        // A dispatch is promised a quantum at most, whatever the credit holds.
        // The credit exists to keep the common dispatch off the shared
        // counters, not to lengthen a turn: a thread that may run for two
        // quanta without the scheduler looking at it again is a thread the
        // preemption granularity no longer applies to, and the excess a late
        // tick can produce grows with it.
        let grant = have.min(ceiling).min(QUANTUM_NS);
        if grant == 0 {
            return 0;
        }
        match credit.reserved_ns.compare_exchange(
            have,
            have - grant,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return grant,
            Err(seen) => have = seen,
        }
    }
}

// --------------------------------------------------------------- dispatch

/// Whether `index` may be dispatched by `cpu` right now: a user thread that
/// is `Ready` and standing on no processor, or the thread this processor is
/// already running.
fn eligible(cpu: usize, index: usize) -> bool {
    if !thread::is_user_slot(index) {
        return false;
    }
    let cell = thread::get(index);
    if !cell.is_user() {
        return false;
    }
    match cell.state() {
        ThreadState::Ready => cell.on_cpu.load(Ordering::Acquire) == 0,
        ThreadState::Running => CPUS[cpu].current.load(Ordering::Relaxed) == index,
        _ => false,
    }
}

/// Reserves for `index` and records the reservation on it.
///
/// The simultaneity slot is taken here and returned in [`settle`], so what
/// `running` counts is dispatches that are live -- exactly what the ceiling
/// is about -- and not processors that once ran the scope. A slot cached on a
/// processor would make a scope look busier than it is and would make the
/// peak the accounting reports larger than the number of processors, which is
/// not a claim about simultaneity at all.
fn reserve_for(cpu: usize, index: usize, now: u64) -> bool {
    let cell = thread::get(index);
    let slot_scope = cell.effective_scope();
    // Exactly one of the two counters moves per dispatch attempt, on the
    // scope the attempt was made in. A ceiling nothing was ever refused
    // against is a number, not a limit, and a refusal counted twice -- once
    // for the slot and once for the budget -- would not tell the two apart
    // either.
    let account = &scope::table()[slot_scope as usize];
    if !scope::take_running(slot_scope) {
        account.dispatch_refusals.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let grant = take_grant(cpu, index, now);
    if grant == 0 {
        scope::drop_running(slot_scope);
        account.dispatch_refusals.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    account.dispatch_grants.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the thread is eligible for this processor under its run-queue
    // lock: `Ready` with no processor standing on it, or already ours.
    let sched = unsafe { cell.sched() };
    sched.dispatch_reserved_ns = grant;
    sched.dispatch_charged_ns = 0;
    sched.dispatch_window = now / CPU_WINDOW_NS;
    sched.dispatch_scope = cell.effective_scope();
    sched.dispatch_recovery = cell.is_recovery();
    sched.dispatched = true;
    sched.reservation_end_ns = now.saturating_add(grant);
    // The same instant in counter units, for the check on the way back to
    // user mode. Published with the grant, not with the switch: a thread that
    // keeps the processor gets a new reservation without a switch, and a
    // deadline left over from the last one would send it straight back into
    // the scheduler on its next kernel exit.
    CPUS[cpu]
        .deadline_tsc
        .store(deadline_tsc(grant), Ordering::Relaxed);
    true
}

/// Chooses a thread for `cpu` and takes its reservation, or returns the
/// processor's idle thread.
///
/// The synchronous hint is tried first: the thread a blocking caller handed
/// this processor. Then the queue, round-robin from the cursor, with the
/// running thread as a candidate like any other. `fair` says the current
/// thread's turn is over -- its reservation ran out or another thread has
/// waited a quantum -- in which case the hint does not jump the queue.
fn pick_and_reserve(rq: &mut RunQueue, cpu: usize, current: usize, now: u64, fair: bool) -> usize {
    let mut stalled = 0u64;
    // Scopes that have already refused a dispatch in this pass. A scope that
    // has no budget left has none for its other threads either, and asking it
    // once per thread turns a refusal into a scan of the whole queue -- the
    // cost that showed up as millions of refused attempts when a scope was
    // throttled.
    let mut refused = Refused::default();
    let hint = rq.hint;
    if hint != NO_THREAD {
        rq.hint = NO_THREAD;
        CPUS[cpu].sync_since.store(0, Ordering::Relaxed);
        if !fair && rq.ready & bit(hint) != 0 && eligible(cpu, hint) {
            if reserve_for(cpu, hint, now) {
                rq.ready &= !bit(hint);
                rq.cursor = next_slot(hint);
                return hint;
            }
            refused.note(thread::get(hint).effective_scope());
            stalled += 1;
        }
    }
    let cur = thread::get(current);
    let current_runs =
        thread::is_user_slot(current) && cur.is_user() && cur.state() == ThreadState::Running;
    // The queue first when the current thread's turn is over, the current
    // thread among the others otherwise; either way round-robin from the
    // cursor, so no thread waits more than a full turn of the queue.
    let mut candidates = rq.ready;
    if current_runs && !fair {
        candidates |= bit(current);
    }
    // Over the candidates themselves, from the cursor round: the bits at or
    // after it, then the ones before. A loop over every slot in the table
    // instead was a division per slot -- the table's size is not a power of
    // two -- forty-eight of them on every dispatch, for a queue that usually
    // holds one thread.
    let start = rq.cursor.min(MAX_THREADS - 1);
    let above = candidates >> start;
    let below = candidates & (bit(start) - 1);
    let mut walk = above;
    let mut base = start;
    loop {
        if walk == 0 {
            if base == 0 {
                break;
            }
            walk = below;
            base = 0;
            continue;
        }
        let index = base + walk.trailing_zeros() as usize;
        walk &= walk - 1;
        if !eligible(cpu, index) {
            continue;
        }
        let scope = thread::get(index).effective_scope();
        if refused.holds(scope) {
            continue;
        }
        if reserve_for(cpu, index, now) {
            rq.ready &= !bit(index);
            rq.cursor = next_slot(index);
            return index;
        }
        refused.note(scope);
        stalled += 1;
    }
    if current_runs
        && fair
        && !refused.holds(thread::get(current).effective_scope())
        && reserve_for(cpu, current, now)
    {
        rq.cursor = next_slot(current);
        return current;
    }
    if stalled != 0 {
        CPUS[cpu]
            .budget_stalls
            .fetch_add(stalled, Ordering::Relaxed);
        // Everything this processor could have run belongs to a scope that
        // is out of budget for this window. It waits for the boundary rather
        // than asking again: the answer cannot change until then, and asking
        // in a loop is how a throttled processor spends a window refusing
        // instead of halting.
        CPUS[cpu].throttled.store(true, Ordering::Release);
    }
    idle_thread(cpu)
}

/// The slot a round-robin cursor moves to after `index`, without a division.
#[inline]
fn next_slot(index: usize) -> usize {
    if index + 1 >= MAX_THREADS {
        0
    } else {
        index + 1
    }
}

/// The scopes that refused a dispatch during one pass over a queue.
///
/// A short list rather than a bitmap of the table: a queue holds at most a
/// handful of distinct scopes, and a list that fills up simply stops
/// filtering, which costs a scan and never a wrong decision.
#[derive(Default)]
struct Refused {
    scopes: [ScopeId; 8],
    len: usize,
}

impl Refused {
    fn note(&mut self, scope: ScopeId) {
        if self.len < self.scopes.len() {
            self.scopes[self.len] = scope;
            self.len += 1;
        }
    }

    fn holds(&self, scope: ScopeId) -> bool {
        self.scopes[..self.len].contains(&scope)
    }
}

/// Charges the time `index` has run on this processor since it was last
/// charged.
fn charge(cpu: usize, index: usize, now: u64) {
    let cell = thread::get(index);
    // SAFETY: `index` is this processor's current thread.
    let sched = unsafe { cell.sched() };
    let ran = now.saturating_sub(sched.dispatched_ns);
    sched.cpu_ns += ran;
    sched.dispatched_ns = now;
    if !cell.is_user() || ran == 0 {
        return;
    }
    // Measured on user execution only: an idle thread's first interval spans
    // the processor's whole bring-up and would say nothing about scheduling.
    if MAX_CHARGE_INTERVAL_NS.load(Ordering::Relaxed) < ran {
        MAX_CHARGE_INTERVAL_NS.fetch_max(ran, Ordering::Relaxed);
    }
    CPUS[cpu].user_ns.fetch_add(ran, Ordering::Relaxed);
    let scope = cell.effective_scope();
    if sched.dispatched {
        sched.dispatch_charged_ns += ran;
    }
    if cell.is_recovery() {
        scope::charge_cpu(scope, ran, true);
    } else {
        // Held locally and flushed by the window roll, the credit's refill
        // or the processor going idle: the charge exists from this instant,
        // and any query sums it in, but no other processor's cache line is
        // written for it here.
        CPUS[cpu].credits[scope as usize]
            .pending_ns
            .fetch_add(ran, Ordering::Relaxed);
    }
}

/// Returns the reservation a thread was holding, and names the scope it was
/// charged to.
fn settle(cpu: usize, index: usize) -> ScopeId {
    let cell = thread::get(index);
    // SAFETY: `index` is this processor's current thread.
    let sched = unsafe { cell.sched() };
    if !sched.dispatched {
        return scope::NO_SCOPE;
    }
    sched.dispatched = false;
    let reserved = sched.dispatch_reserved_ns;
    let charged = sched.dispatch_charged_ns;
    let window = sched.dispatch_window;
    let scope = sched.dispatch_scope;
    let recovery = sched.dispatch_recovery;
    sched.dispatch_reserved_ns = 0;
    sched.dispatch_charged_ns = 0;
    scope::drop_running(scope);
    if charged > reserved {
        // Ran beyond its promise, by the interrupt latency. "Committed"
        // keeps meaning "charged plus promised" only if the excess is added.
        scope::commit_excess(scope, charged - reserved, recovery);
        return scope;
    }
    let unused = reserved - charged;
    if unused == 0 {
        return scope;
    }
    if recovery {
        scope::return_cpu(scope, unused, true, window);
        return scope;
    }
    let credit = &CPUS[cpu].credits[scope as usize];
    if credit.window.load(Ordering::Relaxed) == window {
        credit.reserved_ns.fetch_add(unused, Ordering::AcqRel);
    } else {
        scope::return_cpu(scope, unused, false, window);
    }
    scope
}

/// Publishes the thread this processor switched away from.
///
/// Called from the incoming context, on the processor that performed the
/// switch, once the outgoing thread's stopped context is written. Only a thread
/// still marked `Running` is published: one that blocked or died recorded that
/// before it gave up the processor and must not be made runnable again. The
/// publication releases the `on_cpu` mark, which a waker on another processor
/// may be waiting for.
pub extern "C" fn finish_switch() {
    let cpu = percpu::index();
    let previous = CPUS[cpu].previous.swap(NO_THREAD, Ordering::AcqRel);
    if previous == NO_THREAD {
        return;
    }
    let cell = thread::get(previous);
    if cell.state() == ThreadState::Running {
        cell.set_state(ThreadState::Ready);
    }
    cell.on_cpu.store(0, Ordering::Release);
}

/// What one processor decided to do next.
struct Plan {
    next: usize,
    save_rsp: *mut u64,
    load_rsp: u64,
    save_fpu: *mut fpu::FpuState,
    load_fpu: *const fpu::FpuState,
    kstack_top: u64,
    cr3: u64,
    fs_base: u64,
    migrated_from: usize,
    migrations: u64,
    domain: usize,
}

/// Decides what this processor runs next and claims it, in one critical
/// section.
///
/// Choosing, reserving and claiming cannot be three sections. A thread selected
/// in one and marked running in another is a thread two processors can select,
/// and two processors running one thread means one kernel stack carrying two
/// contexts. The claim -- moving the thread to `Running` while this processor's
/// `current` names it and `on_cpu` marks it -- is what makes [`eligible`]
/// refuse it everywhere else, and it happens here, under the same lock as the
/// choice.
fn plan(cpu: usize, fair: bool) -> Option<Plan> {
    let now = time::observe();
    if now / CPU_WINDOW_NS != CURRENT_WINDOW.load(Ordering::Relaxed) {
        roll(now);
    }
    let mut rq = CPUS[cpu].rq.lock();
    let mut current = CPUS[cpu].current.load(Ordering::Relaxed);
    if current >= MAX_THREADS {
        current = idle_thread(cpu);
        CPUS[cpu].current.store(current, Ordering::Relaxed);
    }
    charge(cpu, current, now);
    let settled = settle(cpu, current);
    CPUS[cpu].need_resched.store(false, Ordering::Relaxed);
    // The verdict is this decision's: `pick_and_reserve` sets it again if
    // every candidate is still throttled, and the idle loop reads it right
    // after.
    CPUS[cpu].throttled.store(false, Ordering::Relaxed);
    let next = pick_and_reserve(&mut rq, cpu, current, now, fair);
    // A credit is held only while this processor is running that scope. A
    // processor that ran a scope once and then moved on was holding two
    // quanta of its budget until it went idle or the window turned, which is
    // budget its siblings were being refused for -- and a scope waiting for
    // it waits for the window, not for a quantum.
    if settled != scope::NO_SCOPE && thread::get(next).effective_scope() != settled {
        release_credit(cpu, settled);
    }
    if next == current {
        return None;
    }

    let outgoing = thread::get(current);
    // A preempted thread stays in this processor's queue. It keeps its
    // `Running` state until this processor's incoming context publishes it,
    // which is what stops another processor from resuming a context that is
    // still being written.
    if outgoing.is_user() && outgoing.state() == ThreadState::Running {
        rq.ready |= bit(current);
    }
    CPUS[cpu].previous.store(current, Ordering::Release);

    let incoming = thread::get(next);
    incoming.on_cpu.store(cpu as u8 + 1, Ordering::Release);
    incoming.set_state(ThreadState::Running);
    CPUS[cpu].current.store(next, Ordering::Release);
    // SAFETY: `next` is claimed by this processor under its run-queue lock.
    let sched = unsafe { incoming.sched() };
    let migrated_from = sched.last_cpu;
    if migrated_from != usize::MAX && migrated_from != cpu {
        sched.migrations += 1;
    }
    sched.last_cpu = cpu;
    sched.dispatched_ns = now;
    let domain = incoming.domain();
    if incoming.is_user() {
        CPUS[cpu].dispatches.fetch_add(1, Ordering::Relaxed);
        tlb::note_dispatch(domain, cpu);
    } else {
        CPUS[cpu].deadline_tsc.store(u64::MAX, Ordering::Relaxed);
    }
    let control = incoming.control();
    drop(rq);

    // SAFETY: the cells live in a `static`, so pointers into them stay valid
    // after the lock is dropped. Neither thread can be touched by another
    // processor while the switch runs: the incoming one is `Running` and
    // named by this processor's `current`, and the outgoing one is marked as
    // standing on this processor and unpublished.
    let (save_rsp, save_fpu, load_fpu, load_rsp) = unsafe {
        let out = outgoing.sched();
        let inc = incoming.sched();
        (
            &raw mut out.saved_rsp,
            &raw mut out.fpu,
            &raw const inc.fpu,
            inc.saved_rsp,
        )
    };

    Some(Plan {
        next,
        save_rsp,
        load_rsp,
        save_fpu,
        load_fpu,
        kstack_top: control.kstack_top,
        cr3: control.cr3,
        fs_base: incoming.fs_base.load(Ordering::Relaxed),
        migrated_from,
        migrations: sched.migrations,
        domain,
    })
}

/// The counter value a reservation of `grant` nanoseconds runs out at.
fn deadline_tsc(grant_ns: u64) -> u64 {
    let hz = time::hz();
    if hz == 0 || grant_ns == 0 {
        return u64::MAX;
    }
    cpu::rdtsc().saturating_add((u128::from(grant_ns) * u128::from(hz) / 1_000_000_000) as u64)
}

/// Rolls the window forward, once per boundary machine-wide.
fn roll(now: u64) {
    scope::roll_window(now);
    CURRENT_WINDOW.store(now / CPU_WINDOW_NS, Ordering::Relaxed);
}

/// Records a thread pointer this processor has just written itself, so the
/// next dispatch onto it knows the register already holds that value.
pub fn note_fs_base(value: u64) {
    CPUS[percpu::index()]
        .fs_base
        .store(value, Ordering::Relaxed);
}

/// Runs one scheduling decision on this processor and performs the switch it
/// asks for. Returns the thread now running here.
fn schedule_with(cpu: usize, fair: bool) -> usize {
    let Some(plan) = plan(cpu, fair) else {
        return CPUS[cpu].current.load(Ordering::Relaxed);
    };

    if plan.migrated_from != usize::MAX
        && plan.migrated_from != cpu
        && plan.migrations <= MIGRATION_RECORD_LIMIT
    {
        trace!(
            "sched.migrated",
            "thread={} domain={} from_cpu={} to_cpu={cpu} migrations={} \
             fp_state=saved_and_restored",
            plan.next,
            plan.domain,
            plan.migrated_from,
            plan.migrations
        );
    }

    // Any translation removed anywhere is retired here, before this processor
    // can execute the incoming thread. That is what closes the race a snapshot
    // of "processors currently in this address space" cannot: a processor
    // entering afterwards refreshes rather than needing to have been counted.
    tlb::refresh_local();

    // SAFETY: the FP areas are the two threads' own 16-byte-aligned save areas;
    // `kstack_top` is the incoming thread's mapped kernel stack; `cr3` is the
    // root of an address space that shares the kernel's upper half, so the code
    // and stack executing here stay mapped across the write; and `load_rsp` is a
    // stopped context this kernel built. No lock is held.
    unsafe {
        fpu::save(plan.save_fpu);
        fpu::restore(plan.load_fpu);
        gdt::set_kernel_stack(plan.kstack_top);
        trap::set_syscall_stack(plan.kstack_top);
        tlb::switch_space(plan.cr3, plan.domain);
        // The incoming thread's own FS base. A thread must never run with
        // another's, and the value this processor holds is whatever the last
        // thread here left -- so it is written whenever it differs, and only
        // then. The write is a model-specific register, which on a virtual
        // machine may leave the guest entirely; almost every thread on this
        // system has no thread pointer at all, and writing the same zero on
        // every switch was paying that price for nothing.
        if CPUS[cpu].fs_base.load(Ordering::Relaxed) != plan.fs_base {
            CPUS[cpu].fs_base.store(plan.fs_base, Ordering::Relaxed);
            cpu::wrmsr(cpu::MSR_FS_BASE, plan.fs_base);
        }
        context::switch_context(plan.save_rsp, plan.load_rsp);
    }

    finish_switch();
    plan.next
}

fn schedule(cpu: usize) -> usize {
    schedule_with(cpu, false)
}

// ------------------------------------------------------------------- wakes

/// Chooses the processor a woken thread should run on when its waker keeps
/// running: where it last ran if that is idle, else any idle processor, else
/// where it last ran.
fn select_cpu(index: usize, me: usize) -> usize {
    // SAFETY: the thread is blocked and standing on no processor, so nothing
    // writes its scheduling record.
    let last = unsafe { thread::get(index).sched().last_cpu };
    // Idle *and* with nothing already waiting there. A processor stays in the
    // idle set from the moment it stops running until it notices the work it
    // has been given, so a burst of wakes that reads the set alone hands every
    // one of them to the same processor -- which is how four benchmark pairs
    // came to start life on one processor while three others idled.
    let idle = IDLE_MASK.load(Ordering::Acquire);
    let free = |cpu: usize| {
        cpu < MAX_CPUS && idle & bit(cpu) != 0 && CPUS[cpu].is_online() && queue_depth(cpu) == 0
    };
    if free(last) {
        return last;
    }
    // This processor, when the thread last ran here and nothing is queued.
    // It is not idle -- it is running the thread doing the waking -- but the
    // cache is warm here and, above all, no other processor has to be
    // interrupted: an inter-processor interrupt costs about a microsecond and
    // a half on this platform, measured, which is a third of an IPC round
    // trip. The woken thread takes this processor at the waker's next kernel
    // exit, which is what `need_resched` is for. Linux's `wake_affine` makes
    // the same choice for the same reason.
    if last == me && queue_depth(me) == 0 {
        return me;
    }
    // A processor that is polling takes the work by looking; a processor that
    // has halted has to be interrupted, and an interrupt costs about a
    // microsecond and a half here. Both are idle, and only one of them is
    // free.
    let halted = HALTED_MASK.load(Ordering::Acquire);
    for set in [idle & !halted, idle & halted] {
        let mut candidates = set;
        while candidates != 0 {
            let candidate = candidates.trailing_zeros() as usize;
            candidates &= candidates - 1;
            if free(candidate) {
                return candidate;
            }
        }
    }
    // Nothing is idle. The processor with the shortest queue then, and the
    // one it last ran on to break a tie: a wake that always returns to
    // `last_cpu` glues a thread to whichever processor happened to run it
    // first, and with more threads than processors nothing ever moves it
    // again. That was measured: eight threads, four processors, one of them
    // carrying sixty per cent of the user execution.
    let mut best = me;
    let mut best_load = u32::MAX;
    for cpu in 0..MAX_CPUS {
        if !CPUS[cpu].is_online() {
            continue;
        }
        let load = queue_depth(cpu)
            + u32::from(CPUS[cpu].current.load(Ordering::Relaxed) != idle_thread(cpu));
        if load < best_load || (load == best_load && cpu == last) {
            best = cpu;
            best_load = load;
        }
    }
    best
}

/// How many threads are waiting in a processor's queue, read without its
/// lock: a balancing hint, never a decision on its own.
fn queue_depth(cpu: usize) -> u32 {
    let rq = CPUS[cpu].rq.as_mut_ptr();
    // SAFETY: a relaxed read of one word for a scheduling hint.
    unsafe { core::ptr::read_volatile(&raw const (*rq).ready).count_ones() }
}

/// Takes one waiting thread from the busiest processor's queue.
///
/// The counterpart of the balance at wake time, for the threads that were
/// already placed when the imbalance appeared: a processor with nothing to
/// run pulls, rather than a busy processor pushing. Bounded by the number of
/// processors, with no global scan and no lock held across the two queues
/// except in ascending order, and a busy victim is skipped rather than waited
/// for -- two idle processors stealing from each other must not meet.
///
/// Linux does this in `newidle_balance`, from the same premise: the processor
/// that has nothing to do is the one that can afford to look.
fn steal_work(me: usize) -> bool {
    let idle = IDLE_MASK.load(Ordering::Acquire);
    let mut victim = usize::MAX;
    let mut depth = 0;
    for other in 0..MAX_CPUS {
        // A processor that is itself idle will take its own queue on its next
        // turn; taking it from under it would be two processors fighting over
        // one thread and would leave neither of them running it sooner.
        if other == me || !CPUS[other].is_online() || idle & bit(other) != 0 {
            continue;
        }
        let other_depth = queue_depth(other);
        if other_depth > depth {
            depth = other_depth;
            victim = other;
        }
    }
    if victim == usize::MAX {
        return false;
    }
    let (low, high) = if me < victim {
        (me, victim)
    } else {
        (victim, me)
    };
    let Some(mut first) = CPUS[low].rq.try_lock() else {
        return false;
    };
    let Some(mut second) = CPUS[high].rq.try_lock() else {
        return false;
    };
    let (mine, theirs) = if me == low {
        (&mut *first, &mut *second)
    } else {
        (&mut *second, &mut *first)
    };
    // Not the one they are about to take: their cursor points at it.
    let mut taken = NO_THREAD;
    for step in 0..MAX_THREADS {
        let index = (theirs.cursor + MAX_THREADS - 1 - step) % MAX_THREADS;
        if theirs.ready & bit(index) == 0 || index == theirs.hint {
            continue;
        }
        let cell = thread::get(index);
        if cell.state() != ThreadState::Ready || cell.on_cpu.load(Ordering::Acquire) != 0 {
            continue;
        }
        taken = index;
        break;
    }
    if taken == NO_THREAD {
        return false;
    }
    theirs.ready &= !bit(taken);
    mine.ready |= bit(taken);
    true
}

/// Whether this processor's run queue is empty, read without its lock.
///
/// A hint, and used only as one: a bit that appears set is set until this
/// processor's own dispatch clears it, and a bit that appears clear may be
/// set an instant later by a waker, which then also kicks.
fn local_queue_empty(cpu: usize) -> bool {
    let rq = CPUS[cpu].rq.as_mut_ptr();
    // SAFETY: a relaxed read of one word for a scheduling hint.
    unsafe { core::ptr::read_volatile(&raw const (*rq).ready) == 0 }
}

/// Sends a reschedule interrupt to `cpu`.
fn kick(me: usize, cpu: usize) {
    let Some(controller) = crate::arch::x86_64::lapic::current() else {
        return;
    };
    let apic_id = CPUS[cpu].apic_id.load(Ordering::Acquire);
    if apic_id != u32::MAX {
        controller.send_fixed(apic_id, trap::RESCHEDULE_VECTOR);
        CPUS[me].kicks.fetch_add(1, Ordering::Relaxed);
    }
}

/// Makes a thread whose wait has been consumed runnable.
///
/// The caller has already consumed the wait under the thread's lock and set
/// the status it will return. The thread may still be standing on the
/// processor that is switching away from it; this waits for that publication,
/// exactly as Linux waits on `p->on_cpu`, and is the only spin in the wake
/// path. It is bounded by one context switch on another processor, which
/// holds no lock at that point.
pub fn wake(index: usize, hint: WakeHint) {
    let cell = thread::get(index);
    while cell.on_cpu.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
    cell.set_state(ThreadState::Ready);
    let me = percpu::index();
    let target = match hint {
        // A synchronous handoff is worth its affinity only if this processor
        // is about to be free: the caller blocks immediately after, and a
        // queue that already holds something will not reach the woken thread
        // any sooner than an idle processor would. Handing every reply to the
        // caller's processor regardless is how four independent pairs end up
        // serialised on one processor while three others idle -- measured, at
        // `scale.ipc:4`. Linux draws the same line in `wake_affine`, from
        // `this_rq()->nr_running`.
        WakeHint::Sync if local_queue_empty(me) => me,
        _ => select_cpu(index, me),
    };
    {
        let mut rq = CPUS[target].rq.lock();
        rq.ready |= bit(index);
        if hint == WakeHint::Sync && target == me {
            rq.hint = index;
            CPUS[target]
                .sync_since
                .store(cpu::rdtsc().max(1), Ordering::Relaxed);
        }
    }
    if target == me {
        // A synchronous wake is taken when this processor blocks; anything
        // else waits for this processor's next interrupt or entry, and the
        // exit path delegates it if it ages.
        if hint == WakeHint::Any {
            CPUS[me].need_resched.store(true, Ordering::Release);
        }
        return;
    }
    // The enqueue above and the read of the target's state below must not
    // be reordered against the target's own "halted, then look once more"
    // sequence, or a wake could land in the queue of a processor that has
    // just decided the queue is empty and halts. A full fence on both sides
    // is what makes one of the two see the other.
    core::sync::atomic::fence(Ordering::SeqCst);
    match CPUS[target].idle.load(Ordering::Acquire) {
        IDLE_POLLING => {}
        IDLE_HALTED => kick(me, target),
        _ => {
            // Running something that keeps running: the woken thread has
            // waited long enough already, so it takes the processor now and
            // the running thread returns to the queue.
            CPUS[target].need_resched.store(true, Ordering::Release);
            kick(me, target);
        }
    }
}

/// Makes a thread of a domain that has just been activated schedulable.
///
/// Activation is a control-plane operation: the domain lock is held and the
/// activator keeps running. The new thread therefore goes to an idle
/// processor if there is one, exactly as any other wake that does not block
/// its waker, rather than displacing the activator.
///
/// A slot that is not `Held` is left alone: activation is idempotent for
/// threads that are already running, and a thread that died between the
/// admission and here must not be resurrected.
pub fn start_thread(index: usize) {
    let cell = thread::get(index);
    if cell.state() != ThreadState::Held {
        return;
    }
    wake(index, WakeHint::Any);
}

/// Stops a thread because its domain is being terminated.
///
/// Returns the parallelism slot the thread held with a swap, so that this and
/// a thread returning its own slot on another processor cannot both return
/// it; clears the wait so no later completion can consume it; publishes
/// `stop` before `Dead`, so a processor that sees the state also sees the
/// reason; and takes the thread out of whatever run queue holds it, so a
/// processor cannot pick a context whose mappings are about to be withdrawn.
///
/// A thread that is *on* a processor right now is not stopped here: the kick
/// the caller sends afterwards brings it into the kernel, where
/// [`leave_if_dead`] finds the state published here.
pub fn stop_thread(index: usize) {
    let cell = thread::get(index);
    if let Some(scope) = cell.take_parallelism_scope() {
        crate::scope::drop_parallelism(scope);
    }
    {
        let mut record = cell.wait.lock();
        record.wait = thread::Wait::None;
        record.deadline_ns = 0;
        cell.deadline_ns.store(0, Ordering::Relaxed);
        cell.stop.store(true, Ordering::Release);
        cell.set_state(ThreadState::Dead);
    }
    for cpu in 0..MAX_CPUS {
        if !CPUS[cpu].is_online() {
            continue;
        }
        let mut rq = CPUS[cpu].rq.lock();
        if rq.ready & bit(index) != 0 {
            rq.ready &= !bit(index);
            if rq.hint == index {
                rq.hint = NO_THREAD;
            }
        }
    }
}

/// Delegates a synchronous wake this processor has held for longer than the
/// grace, because the thread it woke is not going to be picked up here soon:
/// the current thread keeps running.
fn delegate_aged_sync(me: usize) {
    let since = CPUS[me].sync_since.load(Ordering::Relaxed);
    if since == 0 || cpu::rdtsc().wrapping_sub(since) < grace_cycles(WAKE_GRACE_NS) {
        return;
    }
    let idle = IDLE_MASK.load(Ordering::Acquire);
    if idle == 0 {
        // Nowhere to send it; it waits for this processor's next switch.
        return;
    }
    let target = idle.trailing_zeros() as usize;
    if target >= MAX_CPUS || target == me {
        return;
    }
    let moved = {
        let mut rq = CPUS[me].rq.lock();
        let hint = rq.hint;
        if hint == NO_THREAD || rq.ready & bit(hint) == 0 {
            rq.hint = NO_THREAD;
            CPUS[me].sync_since.store(0, Ordering::Relaxed);
            None
        } else {
            rq.ready &= !bit(hint);
            rq.hint = NO_THREAD;
            CPUS[me].sync_since.store(0, Ordering::Relaxed);
            Some(hint)
        }
    };
    let Some(moved) = moved else { return };
    {
        let mut rq = CPUS[target].rq.lock();
        rq.ready |= bit(moved);
    }
    if CPUS[target].idle.load(Ordering::Acquire) == IDLE_HALTED {
        kick(me, target);
    }
}

/// Called on the way back to a thread that keeps running: a thread it woke
/// synchronously and did not pick up is handed to an idle processor once it
/// has aged, and a wake that asked for this processor is honoured.
pub fn on_kernel_exit() {
    let me = percpu::index();
    if CPUS[me].need_resched.load(Ordering::Acquire) {
        schedule(me);
        return;
    }
    // A reservation that has run out ends the dispatch here rather than at
    // the next timer interrupt. Preemption by tick alone means a thread keeps
    // the processor until one arrives, and on an emulated platform that is
    // milliseconds: K3 measured single charges covering four and a half of
    // them, and a scope executing four times what its window could admit
    // because of it. Every return from the kernel to a thread's own code is
    // a point where the reservation can be honoured exactly, and it costs one
    // counter read.
    let deadline = CPUS[me].deadline_tsc.load(Ordering::Relaxed);
    if deadline != u64::MAX && cpu::rdtsc() >= deadline {
        {
            // The reservation ran out while the thread was in the kernel, so
            // its dispatch ends here rather than at the next tick. Counted the
            // same way: what the number means is "the timer ended my turn",
            // and where the kernel noticed is not the thread's business. A
            // thread that spends milliseconds inside one entry -- writing a
            // diagnostic record, say -- has every one of its turns ended here
            // and none of them at a tick.
            let cell = thread::get(CPUS[me].current.load(Ordering::Relaxed));
            if cell.is_user() {
                // SAFETY: this processor's own current thread.
                unsafe { cell.sched() }.preemptions += 1;
            }
        }
        schedule_with(me, true);
        return;
    }
    if CPUS[me].sync_since.load(Ordering::Relaxed) != 0 {
        delegate_aged_sync(me);
    }
}

/// Handles a reschedule interrupt: something was enqueued here, or this
/// processor was told to look again.
pub fn on_reschedule(frame: &trap::TrapFrame) {
    let me = percpu::index();
    if frame.from_user() || CPUS[me].current.load(Ordering::Relaxed) == idle_thread(me) {
        // Interrupted user code or the idle loop: safe to switch.
        if CPUS[me].need_resched.load(Ordering::Acquire) {
            schedule(me);
        }
    }
}

fn grace_cycles(grace_ns: u64) -> u64 {
    grace_ns * time::hz() / 1_000_000_000
}

/// An idle processor's turn to steal a synchronous wake another processor
/// has held past the grace: the waker there kept running.
fn steal_aged_sync(me: usize) -> bool {
    let grace = grace_cycles(WAKE_GRACE_NS);
    let now = cpu::rdtsc();
    for other in 0..MAX_CPUS {
        if other == me || !CPUS[other].is_online() {
            continue;
        }
        let since = CPUS[other].sync_since.load(Ordering::Relaxed);
        if since == 0 || now.wrapping_sub(since) < grace {
            continue;
        }
        // The victim may be in its own switch right now, about to take the
        // thread itself; a busy lock means exactly that, and it is skipped.
        let Some(mut rq) = CPUS[other].rq.try_lock() else {
            continue;
        };
        let hint = rq.hint;
        if hint == NO_THREAD || rq.ready & bit(hint) == 0 || !eligible(other, hint) {
            continue;
        }
        rq.ready &= !bit(hint);
        rq.hint = NO_THREAD;
        CPUS[other].sync_since.store(0, Ordering::Relaxed);
        drop(rq);
        let mut mine = CPUS[me].rq.lock();
        mine.ready |= bit(hint);
        return true;
    }
    false
}

// -------------------------------------------------------------------- ticks

/// Wakes every blocked thread whose deadline has passed.
fn expire_waits(now: u64) -> u32 {
    if NEXT_DEADLINE.load(Ordering::Acquire) > now {
        return 0;
    }
    let mut woken = 0;
    let mut next = u64::MAX;
    for (index, cell) in thread::iter() {
        if cell.state() != ThreadState::Blocked {
            continue;
        }
        let deadline = cell.deadline_ns.load(Ordering::Relaxed);
        if deadline == 0 {
            continue;
        }
        if deadline <= now {
            if thread::wake_if(
                index,
                |record| record.deadline_ns != 0 && record.deadline_ns <= now,
                thalyx_abi::status::TIMED_OUT,
                0,
                WakeHint::Any,
            ) {
                woken += 1;
            }
        } else {
            next = next.min(deadline);
        }
    }
    // A wait registered while this ran has already lowered the value; the
    // minimum keeps it.
    NEXT_DEADLINE.fetch_min(next, Ordering::AcqRel);
    // Anything registered between the scan and here that is earlier than
    // `next` set the counter itself; anything later is covered by `next`.
    woken
}

/// Work every processor's timer does: roll windows, expire waits and timers,
/// promote quiescence, drain the quarantine.
///
/// Each step is taken only when its own condition says there is something to
/// do, so a machine with four timers ticking makes the same progress a machine
/// with one does, without four processors contending for the control lock to
/// find nothing.
fn tick_bookkeeping(now: u64) {
    if now / CPU_WINDOW_NS != CURRENT_WINDOW.load(Ordering::Relaxed) {
        roll(now);
    }
    if crate::api::evtops::timer_due(now) {
        let mut machine = MACHINE.lock();
        crate::api::evtops::expire(&mut machine, now);
    }
    expire_waits(now);
    if scope::any_fenced() {
        let _machine = MACHINE.lock();
        scope::advance_quiescence(now);
    }
    if crate::mm::frame::quarantine_pending() {
        let mut machine = MACHINE.lock();
        if let Some(allocator) = machine.memory.as_mut() {
            allocator.drain_quarantine();
        }
    }
}

/// Charges the current tick and preempts the running thread when its
/// reservation has expired or another thread's turn has come. Called from the
/// timer interrupt with interrupts masked.
pub fn on_tick(frame: &trap::TrapFrame) {
    let cpu = percpu::index();
    CPUS[cpu].ticks.fetch_add(1, Ordering::Relaxed);
    let now = time::observe();
    tick_bookkeeping(now);

    let current = CPUS[cpu].current.load(Ordering::Relaxed);
    let cell = thread::get(current);
    let user = cell.is_user();
    let (expired, competition, starved) = {
        let rq = CPUS[cpu].rq.lock();
        charge(cpu, current, now);
        // SAFETY: `current` is this processor's thread, under its lock.
        let sched = unsafe { cell.sched() };
        let expired = user && sched.dispatched && now >= sched.reservation_end_ns;
        // Another thread is waiting here and the running one has had its
        // quantum: its turn is over even though its reservation is not.
        let competition = user && rq.ready != 0 && sched.dispatch_charged_ns >= QUANTUM_NS;
        // Asked of the accounts only when the dispatch has spent what it was
        // promised. A walk of the scope chain on every tick of every
        // processor answers a question that only arises when a reservation
        // has run out.
        let starved = user
            && sched.dispatched
            && sched.dispatch_charged_ns >= sched.dispatch_reserved_ns
            && scope::available_ns(cell.effective_scope(), cell.is_recovery()) == 0;
        (expired, competition, starved)
    };

    let resched = CPUS[cpu].need_resched.load(Ordering::Acquire);
    if !expired && !competition && !starved && !resched {
        if user {
            delegate_aged_sync(cpu);
        }
        return;
    }
    if !user {
        schedule_with(cpu, true);
        return;
    }

    // The record is prepared before the switch, because afterwards this
    // processor is no longer standing in the interrupted thread's context and
    // the frame that proves the preemption was involuntary would be gone.
    let record = {
        // SAFETY: `current` is this processor's thread.
        let sched = unsafe { cell.sched() };
        if sched.preempt_records < PREEMPT_RECORD_LIMIT {
            sched.preempt_records += 1;
            PREEMPT_RECORDS.fetch_add(1, Ordering::Relaxed);
            let scope = &scope::table()[cell.effective_scope() as usize];
            Some((
                cell.domain(),
                scope.id(),
                scope.cpu_window_ns.load(Ordering::Relaxed),
            ))
        } else {
            None
        }
    };

    {
        // The timer ended this thread's dispatch: the reservation it was
        // running on was taken back, and it runs on again only if the
        // scheduler grants it another. Counted here rather than after the
        // switch, because "my turn was ended by the timer" is true whether or
        // not another thread was waiting to take the processor -- and on a
        // processor with nothing else to run, the old placement counted
        // nothing at all and made a program that ran for thirteen
        // milliseconds look as if it had never been scheduled.
        //
        // SAFETY: this processor's own current thread.
        let sched = unsafe { cell.sched() };
        sched.preemptions += 1;
    }
    let next = schedule_with(cpu, expired || competition || starved);
    if next == current {
        return;
    }

    // The processor was taken away and given to another thread. That is the
    // narrower fact, and the one the preemption records are about.
    CPUS[cpu].preemptions.fetch_add(1, Ordering::Relaxed);
    if let Some((domain, scope_id, used)) = record {
        trace!(
            "sched.preempt",
            "cpu={cpu} domain={domain} thread={current} next={next} rip=0x{:x} cs=0x{:x} \
             cpl={} trigger=timer vector=0x{:x} voluntary=0 effective_scope={scope_id} \
             window_used_ns={used} starved={}",
            frame.rip,
            frame.cs,
            frame.cpl(),
            trap::TIMER_VECTOR,
            u8::from(starved)
        );
    }
}

/// Gives up the processor until something wakes this thread.
///
/// The caller has already published what it is waiting for and moved itself to
/// `Blocked` under the thread's lock. When this returns, the wake status the
/// waker left is the reason it returned.
pub fn block_current() {
    schedule(percpu::index());
}

/// Does not return to a thread that was stopped while it was running.
///
/// A domain terminated by authority marks its threads `Dead` under the lock and
/// withdraws their mappings. A thread of that domain that was on another
/// processor at that moment keeps executing user instructions until something
/// brings it into the kernel. Every entry from user mode ends here, so that
/// something is the first interrupt, trap or syscall after the termination --
/// the kick the termination sends, usually -- and not the page fault its
/// withdrawn mappings would eventually produce. A fault that did get there
/// first is classified the same way, in `domain::terminate_on_fault`, because a
/// thread that was told to stop and then touched memory taken from it has not
/// done anything the fault record should hold against its program.
///
/// Found by K5's `engine` stage: the language runtime was stopped by its
/// launcher while it was still freeing its heap on a fourth processor, faulted
/// on the arena the termination had just withdrawn, and the run counted a user
/// fault against a domain that had already been terminated.
pub fn leave_if_dead(frame: &trap::TrapFrame) {
    if !frame.from_user() {
        return;
    }
    let cpu = percpu::index();
    let current = CPUS[cpu].current.load(Ordering::Relaxed);
    let cell = thread::get(current);
    if !cell.stop.load(Ordering::Acquire) {
        return;
    }
    let domain = cell.domain();
    let name = crate::domain::domain_name(domain);
    event!(
        "thread.stopped_late",
        "cpu={cpu} domain={domain} name={name} thread={current} rip=0x{:x} vector=0x{:x} \
         class=stopped_by_authority action=leave kernel=survives",
        frame.rip,
        frame.vector
    );
    switch_away_from_dead()
}

/// Leaves a thread that will never run again and does not return.
///
/// The dying thread's kernel stack is still in use up to the switch, which is
/// why reclamation is deferred to a context that is no longer standing on it.
pub fn switch_away_from_dead() -> ! {
    let cpu = percpu::index();
    let current = CPUS[cpu].current.load(Ordering::Relaxed);
    let cell = thread::get(current);
    if let Some(scope) = cell.take_parallelism_scope() {
        scope::drop_parallelism(scope);
    }
    cell.set_state(ThreadState::Dead);
    schedule(cpu);
    // Reached only if the scheduler picked the dead thread itself, which
    // `eligible` refuses to do once its state is `Dead`. There is nothing to
    // retry: the stack this runs on is the dead thread's, so halting is the
    // only correct end.
    cpu::halt_forever()
}

/// Why the run ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Terminal {
    /// No thread is ready, running or waiting.
    NoRunnableDomain,
    /// Threads remain, all of them waiting for something that cannot arrive.
    Deadlock,
}

impl Terminal {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Terminal::NoRunnableDomain => "no_runnable_domain",
            Terminal::Deadlock => "no_progress_possible",
        }
    }
}

/// True when something can still change the set of eligible threads.
fn progress_possible() -> bool {
    let ready_but_starved = thread::iter().any(|(index, cell)| {
        thread::is_user_slot(index)
            && matches!(cell.state(), ThreadState::Ready | ThreadState::Running)
            && cell.is_user()
    });
    let timed_wait = thread::iter().any(|(_, cell)| {
        cell.state() == ThreadState::Blocked && cell.deadline_ns.load(Ordering::Relaxed) != 0
    });
    ready_but_starved || timed_wait || crate::api::evtops::any_armed()
}

/// Marks this processor idle, watching for work for [`IDLE_POLL_NS`] with
/// interrupts enabled as a halt would have them. Whether something arrived,
/// or the run was told to stop, in which case the caller should reconsider
/// rather than halt.
fn poll_before_halt(cpu: usize) -> bool {
    // Whatever this processor still holds is capacity its siblings are being
    // refused for, and it has nothing to spend it on.
    release_credits(cpu);
    if CPUS[cpu].throttled.load(Ordering::Acquire) {
        return false;
    }
    let hz = time::hz();
    if hz == 0 {
        return false;
    }
    let budget = IDLE_POLL_NS * hz / 1_000_000_000;
    let start = cpu::rdtsc();
    CPUS[cpu].idle.store(IDLE_POLLING, Ordering::Release);
    IDLE_MASK.fetch_or(bit(cpu), Ordering::AcqRel);
    // SAFETY: no lock is held; this is the idle loop, where the handler that
    // runs may switch away from this context and back.
    // Nothing inside the enabled window takes a lock. Interrupts are masked
    // everywhere else in the kernel, so a lock taken here would be a lock a
    // timer interrupt on this very processor could ask for again -- the
    // scheduler asks for this processor's run queue on every tick -- and a
    // ticket lock waiting for a ticket its own context holds waits forever.
    // The loop therefore only *looks*; the queues are moved below, masked.
    let found = unsafe {
        cpu::with_interrupts_enabled(|| {
            loop {
                if smp::shutting_down() {
                    return true;
                }
                if has_local_work(cpu) || aged_sync_elsewhere(cpu) || stealable(cpu) {
                    return true;
                }
                if cpu::rdtsc().wrapping_sub(start) > budget {
                    return false;
                }
                core::hint::spin_loop();
            }
        })
    };
    IDLE_MASK.fetch_and(!bit(cpu), Ordering::AcqRel);
    CPUS[cpu].idle.store(IDLE_RUNNING, Ordering::Release);
    if found && !has_local_work(cpu) {
        let _ = steal_aged_sync(cpu) || steal_work(cpu);
    }
    found
}

/// Whether another processor is holding a synchronous wake past its grace.
/// A lock-free look, for the idle loop.
fn aged_sync_elsewhere(me: usize) -> bool {
    let grace = grace_cycles(WAKE_GRACE_NS);
    let now = cpu::rdtsc();
    (0..MAX_CPUS).any(|other| {
        if other == me || !CPUS[other].is_online() {
            return false;
        }
        let since = CPUS[other].sync_since.load(Ordering::Relaxed);
        since != 0 && now.wrapping_sub(since) >= grace
    })
}

/// Whether some busy processor has a thread waiting behind the one it runs.
/// A lock-free look, for the idle loop.
fn stealable(me: usize) -> bool {
    let idle = IDLE_MASK.load(Ordering::Acquire);
    (0..MAX_CPUS).any(|other| {
        other != me && CPUS[other].is_online() && idle & bit(other) == 0 && queue_depth(other) != 0
    })
}

/// Whether this processor's queue holds anything, or it was told to look.
fn has_local_work(cpu: usize) -> bool {
    if CPUS[cpu].need_resched.load(Ordering::Acquire) {
        return true;
    }
    // A read without the lock: a bit set is set for good until a dispatch
    // clears it, and a dispatch is this processor's own.
    let rq = CPUS[cpu].rq.as_mut_ptr();
    // SAFETY: a relaxed read of one word for a hint; the lock is taken before
    // anything is acted on.
    unsafe { core::ptr::read_volatile(&raw const (*rq).ready) != 0 }
}

/// Halts until an interrupt, giving back the credits this processor holds
/// first: a reservation held by a halted processor is capacity nobody can
/// use.
fn halt(cpu: usize) {
    release_credits(cpu);
    // A processor with nothing to run is never the reason memory cannot come
    // back. Reclamation waits for every processor to have retired the
    // translations a withdrawal removed, and one that is about to stop
    // answering interrupts has no reason to make anyone wait: it holds
    // nothing it is going to use. Now that an invalidation interrupts only
    // the processors that could hold one of its translations, an idle
    // processor is no longer swept along by everyone else's traffic.
    tlb::refresh_local();
    CPUS[cpu].idle.store(IDLE_HALTED, Ordering::Release);
    IDLE_MASK.fetch_or(bit(cpu), Ordering::AcqRel);
    HALTED_MASK.fetch_or(bit(cpu), Ordering::AcqRel);
    // Something may have been enqueued between the poll and here; the store
    // above is what a waker checks, so this check, fenced against it, is what
    // closes the window. See `wake`.
    core::sync::atomic::fence(Ordering::SeqCst);
    if (CPUS[cpu].throttled.load(Ordering::Acquire) || !has_local_work(cpu))
        && !smp::shutting_down()
    {
        // SAFETY: no lock is held, and the handler that runs may switch away
        // from this context and back, which an idle context is built to
        // survive.
        unsafe { cpu::wait_for_interrupt() };
    }
    HALTED_MASK.fetch_and(!bit(cpu), Ordering::AcqRel);
    IDLE_MASK.fetch_and(!bit(cpu), Ordering::AcqRel);
    CPUS[cpu].idle.store(IDLE_RUNNING, Ordering::Release);
}

/// One turn of a processor's idle loop: reclaim, look for work, take it.
///
/// Returns the thread it dispatched, or `None` when the processor found
/// nothing and should consider whether the run is over.
fn idle_turn(cpu: usize) -> Option<usize> {
    crate::domain::reap_dead();
    tick_bookkeeping(time::observe());
    let next = schedule(cpu);
    if next == idle_thread(cpu) {
        None
    } else {
        Some(next)
    }
}

/// The idle loop of an application processor. Never returns.
///
/// It ends only when the bootstrap processor declares the run over; deciding
/// that is not this processor's job, because it cannot see whether another one
/// is about to make a thread runnable.
pub fn run_ap(cpu: usize) -> ! {
    loop {
        if smp::shutting_down() {
            smp::park(cpu);
        }
        if idle_turn(cpu).is_some() || poll_before_halt(cpu) {
            continue;
        }
        halt(cpu);
    }
}

/// Runs the bootstrap processor's idle thread: reclaims what died, waits for
/// what can still happen, and returns when nothing can.
///
/// The decision is made here and only here. A processor that finds nothing to
/// run cannot conclude the run is over, because another processor may be in the
/// middle of making something runnable; the bootstrap processor decides from
/// the whole table and then stops the others.
pub fn run_until_idle() -> Terminal {
    let cpu = percpu::index();
    loop {
        if idle_turn(cpu).is_some() {
            continue;
        }

        let possible = progress_possible();
        let any = thread::iter().any(|(index, cell)| {
            thread::is_user_slot(index)
                && matches!(
                    cell.state(),
                    ThreadState::Ready | ThreadState::Running | ThreadState::Blocked
                )
        });
        // A thread another processor is running is not idleness, whatever
        // this processor can see to dispatch.
        let busy = (0..MAX_CPUS).any(|other| {
            other != cpu
                && CPUS[other].is_online()
                && CPUS[other].current.load(Ordering::Acquire) != idle_thread(other)
                && CPUS[other].current.load(Ordering::Acquire) != NO_THREAD
        });

        if busy {
            if poll_before_halt(cpu) {
                continue;
            }
            halt(cpu);
            continue;
        }
        if !any {
            return Terminal::NoRunnableDomain;
        }
        if !possible {
            return Terminal::Deadlock;
        }
        if poll_before_halt(cpu) {
            continue;
        }
        // Something can still happen, but not here and not now. Let the timer
        // in: this and the application processors' idle loops are the only
        // points where interrupts are enabled outside user mode, and no lock is
        // held across it.
        halt(cpu);
    }
}

/// Whether some processor is standing on a thread of domain `index` at
/// generation `generation`, or on its address space.
///
/// "Not the address space this processor is in" was a sufficient test with one
/// processor and is not one with several. Three things have to be true of every
/// online processor, and each of them is a way a reclamation could pull the
/// ground out from under a processor that is still running:
///
/// * it must not be running a thread of this domain, or the reclamation would
///   free a kernel stack a processor is executing on;
/// * it must not still be *leaving* one -- the thread it switched away from and
///   has not yet published -- because until that publication the outgoing
///   context is still being written on that stack;
/// * neither of those threads may name this address space, because a processor
///   loads the new root only after it has released the lock, so between the two
///   it is still executing with the old one in `CR3`.
#[must_use]
pub fn standing_on(domain: usize, generation: u32, cr3: u64) -> bool {
    for state in &CPUS {
        if !state.is_online() {
            continue;
        }
        for index in [
            state.current.load(Ordering::Acquire),
            state.previous.load(Ordering::Acquire),
        ] {
            if index == NO_THREAD || index >= MAX_THREADS {
                continue;
            }
            let control = thread::get(index).control();
            if control.cr3 == cr3 {
                return true;
            }
            if control.domain == domain && control.domain_generation == generation {
                return true;
            }
        }
    }
    false
}

/// Interrupts every other processor that is running a thread of domain
/// `index`, so it leaves the thread on its way out of the interrupt.
pub fn kick_domain(domain: usize, generation: u32) {
    let me = percpu::index();
    for (cpu, state) in CPUS.iter().enumerate() {
        if cpu == me || !state.is_online() {
            continue;
        }
        let current = state.current.load(Ordering::Acquire);
        if current == NO_THREAD || current >= MAX_THREADS {
            continue;
        }
        let control = thread::get(current).control();
        if control.domain == domain && control.domain_generation == generation {
            state.need_resched.store(true, Ordering::Release);
            kick(me, cpu);
        }
    }
}

/// Processors currently executing in the address space `cr3`, the calling
/// one included.
#[must_use]
pub fn cpus_in_space(cr3: u64) -> u32 {
    let mut count = 0;
    for state in &CPUS {
        if !state.is_online() {
            continue;
        }
        let current = state.current.load(Ordering::Acquire);
        if current == NO_THREAD || current >= MAX_THREADS {
            continue;
        }
        if thread::get(current).control().cr3 == cr3 {
            count += 1;
        }
    }
    count
}

/// Establishes the idle thread of `cpu` as its current thread, before the
/// processor schedules anything. Under the machine lock, at bring-up.
pub fn establish_idle(cpu: usize, kstack_top: u64, cr3: u64, now: u64) {
    let cell = thread::get(idle_thread(cpu));
    cell.set_kind(ThreadKind::Idle);
    cell.set_state(ThreadState::Running);
    cell.on_cpu.store(cpu as u8 + 1, Ordering::Release);
    // SAFETY: the idle thread of a processor that has not started scheduling;
    // nothing else touches it.
    unsafe {
        let control = cell.control_mut();
        control.kstack_top = kstack_top;
        control.cr3 = cr3;
        let sched = cell.sched();
        sched.fpu = fpu::initial();
        sched.dispatched_ns = now;
        sched.last_cpu = cpu;
    }
    CPUS[cpu].current.store(idle_thread(cpu), Ordering::Release);
}
