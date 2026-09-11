//! Scheduling on more than one processor.
//!
//! The mechanism is preemptive round-robin over threads, one runqueue view per
//! processor, and the policy is the resource contract's: a thread runs only if
//! the scope its current work is charged to, **and every ancestor of that
//! scope**, can still pay for it. Budget is aggregate, so two workers that
//! adopted the same client's scope draw from one balance whether they run on
//! one processor or on two.
//!
//! Two things change once there is more than one processor, and both are the
//! difference between a scheduler that looks right and one that is:
//!
//! * **A balance is reserved, not merely checked.** Two processors reading the
//!   same remaining budget would both conclude they may run, and the scope
//!   would spend twice what it has. Here a dispatch takes the time out of the
//!   scope and every ancestor before it runs, and gives back what it did not
//!   use when it stops. The second processor is refused at the reservation, not
//!   audited afterwards.
//! * **A thread being switched away from is not available yet.** Its stopped
//!   context is written by the `switch_context` call itself, after the lock is
//!   released. Another processor that picked it in that window would resume it
//!   from a stack pointer that had not been stored. So the outgoing thread stays
//!   unpickable until the incoming context on the same processor publishes it,
//!   which is what [`finish_switch`] does — including for a thread that has
//!   never run, whose first instructions go through the same publication.
//!
//! Windows are fixed and aligned, of the period the contract fixes. An overrun
//! is not forgiven at the boundary: it starts the next window already consumed.
//! A reservation, in contrast, does not cross the boundary at all — it belongs
//! to the window that made it, and a settlement arriving afterwards finds
//! nothing to return rather than crediting the new window with the old one's
//! capacity.
//!
//! Preemption stays involuntary by construction. A user thread is switched away
//! from inside the timer interrupt of the processor it is running on, at an
//! instruction it did not choose, and the interrupted frame is recorded so the
//! claim can be checked rather than believed.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::{context, cpu, fpu, gdt, trap};
use crate::limits::MAX_CPUS;
use crate::percpu;
use crate::scope;
use crate::state::{MACHINE, MAX_THREADS, Machine, ThreadKind, ThreadState, Wait, idle_thread};
use crate::{event, trace};
use crate::{smp, time, tlb};

/// Timer interrupts per second.
pub const TICK_HZ: u64 = 2000;
/// Scheduling quantum. The value the resource contract fixes for V0.
pub const QUANTUM_NS: u64 = thalyx_abi::limit::CPU_QUANTUM_NS;
/// Quantum expressed in timer ticks.
pub const QUANTUM_TICKS: u32 = (QUANTUM_NS * TICK_HZ / 1_000_000_000) as u32;

const _: () = assert!(QUANTUM_TICKS >= 1);

/// Detailed preemption records emitted per thread before the diagnostic plane
/// starts coalescing them into counters. The bound is per thread rather than
/// global so one busy domain cannot consume the whole budget and leave another
/// domain's preemptions unrecorded. The observability contract permits
/// coalescing; it requires saying so, which the summary record does.
pub const PREEMPT_RECORD_LIMIT: u32 = 8;

/// Detailed migration records emitted before the plane coalesces them.
pub const MIGRATION_RECORD_LIMIT: u64 = 16;

/// Whether a thread is a candidate for `cpu` to dispatch.
///
/// `Ready` and nothing else, with one exception: the thread this processor is
/// already running, which is `Running` and which it may keep. A thread that is
/// `Running` anywhere else belongs to that processor.
fn candidate(machine: &Machine, cpu: usize, index: usize) -> bool {
    if index < MAX_CPUS {
        return false;
    }
    let thread = &machine.threads[index];
    if thread.kind != ThreadKind::User {
        return false;
    }
    match thread.state {
        ThreadState::Ready => !standing_elsewhere(machine, cpu, index),
        ThreadState::Running => machine.cpus[cpu].current == index,
        _ => false,
    }
}

/// Whether another processor is still standing on this thread's context.
///
/// The state is not enough to answer this, and assuming it was is how a
/// blocking call turns into two processors on one kernel stack. A thread that
/// blocks voluntarily writes its own state under the lock, drops the lock, and
/// only then reaches the switch that saves its context. Anything that wakes it
/// inside that window -- a reply from the peer it just published to, on another
/// processor -- makes it `Ready` while the context another processor would
/// resume has not been written yet. On one processor the window does not exist,
/// because nothing else runs during it.
///
/// `current` and `previous` are what actually say "this processor is still on
/// it": the first until this processor plans a switch away, the second until
/// the incoming context publishes the outgoing one. Between them they cover the
/// window with no gap.
fn standing_elsewhere(machine: &Machine, cpu: usize, index: usize) -> bool {
    for other in 0..MAX_CPUS {
        if other == cpu || !machine.cpus[other].online {
            continue;
        }
        if machine.cpus[other].current == index || machine.cpus[other].previous == index {
            return true;
        }
    }
    false
}

/// Execution to promise one dispatch.
///
/// The quantum, cut down by what the scope can still pay and by how much of the
/// window is left. Cutting it at the window boundary is what keeps a
/// reservation inside the window that made it.
fn dispatch_grant(machine: &Machine, index: usize, now: u64) -> u64 {
    let thread = &machine.threads[index];
    let available = scope::available_ns(&machine.scopes, thread.effective_scope, thread.recovery);
    let window_left = crate::limits::CPU_WINDOW_NS - (now % crate::limits::CPU_WINDOW_NS);
    QUANTUM_NS.min(available).min(window_left)
}

/// Ticks a dispatch may run for, from the execution it was actually granted.
///
/// A reservation that does not bound the run is not a reservation. When the
/// budget left in the window is less than a quantum, the dispatch gets the
/// smaller of the two, and the preemption that ends it has to arrive that much
/// sooner. One tick is the floor: preemption is tick-driven, so a dispatch can
/// overrun its reservation by less than a tick period and no more.
const fn quantum_ticks_for(grant: u64) -> u32 {
    let ticks = grant * TICK_HZ / 1_000_000_000;
    if ticks == 0 { 1 } else { ticks as u32 }
}

/// Chooses a thread for `cpu` and takes its reservation, or returns the
/// processor's idle thread.
///
/// Selection and reservation are one step under one lock. Splitting them is the
/// scheduling groundwork's exact counterexample: a thread chosen against a
/// balance another processor has already promised away.
fn pick_and_reserve(machine: &mut Machine, cpu: usize, now: u64) -> usize {
    let start = machine.cpus[cpu].cursor;
    let mut stalled = 0u64;
    for step in 0..MAX_THREADS {
        let index = (start + step) % MAX_THREADS;
        if !candidate(machine, cpu, index) {
            continue;
        }
        let grant = dispatch_grant(machine, index, now);
        if grant == 0 {
            // Counted on the scope that ran out, where the refusal happens. A
            // ceiling nothing was ever refused against is a number, not a
            // limit, and this is what tells the two apart.
            let target = machine.threads[index].effective_scope;
            let recovery = machine.threads[index].recovery;
            if scope::available_ns(&machine.scopes, target, recovery) == 0 {
                let node = &mut machine.scopes[target as usize];
                node.dispatch_refusals = node.dispatch_refusals.saturating_add(1);
            }
            stalled += 1;
            continue;
        }
        let thread = &machine.threads[index];
        let target = thread.effective_scope;
        let recovery = thread.recovery;
        if !scope::reserve_dispatch(&mut machine.scopes, target, grant, recovery) {
            stalled += 1;
            continue;
        }
        let window = now / crate::limits::CPU_WINDOW_NS;
        let thread = &mut machine.threads[index];
        thread.dispatch_reserved_ns = grant;
        thread.dispatch_window = window;
        thread.dispatch_scope = target;
        thread.dispatch_recovery = recovery;
        thread.dispatched = true;
        machine.cpus[cpu].cursor = (index + 1) % MAX_THREADS;
        return index;
    }
    machine.cpus[cpu].budget_stalls += stalled;
    machine.budget_stalls += stalled;
    idle_thread(cpu)
}

/// Notes that a thread has become runnable on this processor's account.
///
/// A processor with nothing to run halts until an interrupt, and until K6 the
/// only interrupt it got was its own timer: a thread woken from another
/// processor waited, on average, a quarter of a tick to run. K6 measured the
/// wake-to-run latency at two hundred microseconds against a tick of five
/// hundred, on a machine with three processors idle.
///
/// The wake is not delegated at once. The processor doing the waking is very
/// often about to become free itself -- a caller blocks for its reply, a
/// server replies, closes the invocation and goes back to receive -- and a
/// round trip that stayed on one processor would then be split across two,
/// with an interrupt or a migration each way: K6 measured the split round
/// trip at twice to three times the cost of the one that stayed. So this only
/// records that a wake happened. The wake is delegated once it has been
/// pending for [`WAKE_GRACE_NS`] without this processor yielding: by
/// [`flush_wake`] when the processor returns to a thread that keeps running,
/// or by an idle processor that watched it age in [`poll_before_halt`]. A
/// processor that yields within the grace takes the work itself, in
/// [`schedule`]. Called under the machine lock from every place that makes a
/// thread `Ready`.
pub fn kick_idle(machine: &mut Machine) {
    let me = percpu::index();
    if !machine.cpus[me].wake_pending {
        machine.cpus[me].wake_pending = true;
        machine.cpus[me].wake_pending_at = cpu::rdtsc();
    }
    READY_EPOCH.fetch_add(1, Ordering::Release);
}

/// How long a wake stays with the processor that made it before another
/// takes it: the few microseconds a server needs to reply, close and receive,
/// with the auditor contending for the machine lock meanwhile. A V0
/// parameter, and a trade: K6 measured the synchronous round trip against the
/// wake-to-run latency with the idle processors' share of it at 3, 6 and 10
/// microseconds as 6.9/5.4, 3.3/8.0 and 2.7/12.0 microseconds. Ten keeps the
/// round trip whole.
pub const WAKE_GRACE_NS: u64 = 10_000;

fn grace_cycles(grace_ns: u64) -> u64 {
    grace_ns * time::hz() / 1_000_000_000
}

/// Bumped whenever a thread becomes runnable: what an idle processor watches
/// before it halts, so a wake that arrives while it is still awake is taken
/// without an interrupt.
static READY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// How long an idle processor watches for a wake before halting. Long enough
/// for the pattern a round trip makes -- one side wakes the other and blocks
/// within a few microseconds -- and short enough that an idle machine is a
/// halted one. Under hardware virtualization a halt and the interrupt that
/// ends it are each a trip through the host; K6 measured a round trip that
/// paid for both at twice the cost of one that did not.
pub const IDLE_POLL_NS: u64 = 50_000;

/// Watches for a wake for [`IDLE_POLL_NS`], with interrupts enabled as a halt
/// would have them. Whether one arrived, or the run was told to stop, in
/// which case the caller should reconsider rather than halt. A wake that is
/// seen is given [`WAKE_GRACE_NS`] to be taken by the processor that made it
/// before this one goes to look.
fn poll_before_halt() -> bool {
    let seen = READY_EPOCH.load(Ordering::Acquire);
    let hz = time::hz();
    if hz == 0 {
        return false;
    }
    let budget = IDLE_POLL_NS * hz / 1_000_000_000;
    let grace = grace_cycles(WAKE_GRACE_NS);
    let start = cpu::rdtsc();
    // SAFETY: no lock is held; this is the idle loop, where the handler that
    // runs may switch away from this context and back.
    unsafe {
        cpu::with_interrupts_enabled(|| {
            let mut woken_at = None;
            loop {
                if smp::shutting_down() {
                    return true;
                }
                let now = cpu::rdtsc();
                match woken_at {
                    Some(at) => {
                        if now.wrapping_sub(at) >= grace {
                            return true;
                        }
                    }
                    None => {
                        if READY_EPOCH.load(Ordering::Acquire) != seen {
                            woken_at = Some(now);
                        } else if now.wrapping_sub(start) > budget {
                            return false;
                        }
                    }
                }
                core::hint::spin_loop();
            }
        })
    }
}

/// Sends a reschedule interrupt to one idle processor, other than this one.
///
/// The interrupt itself does nothing but end the halt; the processor then
/// reconsiders what to run on its own. One that arrives at a processor already
/// awake costs it a return.
fn interrupt_idle(machine: &mut Machine, me: usize) -> bool {
    let Some(controller) = crate::arch::x86_64::lapic::current() else {
        return false;
    };
    for cpu in 0..MAX_CPUS {
        let slot = machine.cpus[cpu];
        if cpu == me || !slot.online || slot.current != idle_thread(cpu) {
            continue;
        }
        let apic_id = smp::apic_id_of(cpu);
        if apic_id != u32::MAX {
            controller.send_fixed(apic_id, trap::RESCHEDULE_VECTOR);
            machine.cpus[me].kicks += 1;
            return true;
        }
    }
    false
}

/// Delegates a pending wake, because this processor is returning to a thread
/// that keeps running and will not pick the woken one up itself.
pub fn flush_wake() {
    let me = percpu::index();
    let mut machine = MACHINE.lock();
    if !machine.cpus[me].wake_pending
        || cpu::rdtsc().wrapping_sub(machine.cpus[me].wake_pending_at) < grace_cycles(WAKE_GRACE_NS)
    {
        return;
    }
    machine.cpus[me].wake_pending = false;
    // An idle processor that was watching may have taken it already.
    if ready_backlog(&machine, me, usize::MAX) {
        interrupt_idle(&mut machine, me);
    }
}

/// Whether a user thread other than `taken` is runnable and standing on no
/// processor: what a second idle processor could run.
fn ready_backlog(machine: &Machine, cpu: usize, taken: usize) -> bool {
    (0..MAX_THREADS).any(|index| {
        index != taken
            && index >= MAX_CPUS
            && machine.threads[index].state == ThreadState::Ready
            && machine.threads[index].kind == ThreadKind::User
            && !standing_elsewhere(machine, cpu, index)
    })
}

/// Returns the reservation a thread was holding.
fn settle(machine: &mut Machine, index: usize) {
    if !machine.threads[index].dispatched {
        return;
    }
    let thread = &machine.threads[index];
    let reserved = thread.dispatch_reserved_ns;
    let window = thread.dispatch_window;
    let target = thread.dispatch_scope;
    let recovery = thread.dispatch_recovery;
    scope::settle_dispatch(&mut machine.scopes, target, reserved, window, recovery);
    let thread = &mut machine.threads[index];
    thread.dispatched = false;
    thread.dispatch_reserved_ns = 0;
}

/// Publishes the thread this processor switched away from.
///
/// Called from the incoming context, on the processor that performed the
/// switch, once the outgoing thread's stopped context is written. Only a thread
/// still marked `Running` is published: one that blocked or died recorded that
/// before it gave up the processor and must not be made runnable again.
pub extern "C" fn finish_switch() {
    let mut machine = MACHINE.lock();
    let cpu = percpu::index();
    let previous = machine.cpus[cpu].previous;
    if previous == usize::MAX {
        return;
    }
    machine.cpus[cpu].previous = usize::MAX;
    if machine.threads[previous].state == ThreadState::Running {
        machine.threads[previous].state = ThreadState::Ready;
    }
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
/// contexts. The claim — moving the thread to `Running` while this processor's
/// `current` names it — is what makes [`candidate`] refuse it everywhere else,
/// and it happens here, under the same lock as the choice.
fn plan(cpu: usize) -> Option<Plan> {
    let machine_ptr = MACHINE.as_mut_ptr();
    let mut machine = MACHINE.lock();
    let now = time::observe();
    let mut current = machine.cpus[cpu].current;
    if current >= MAX_THREADS {
        current = idle_thread(cpu);
        machine.cpus[cpu].current = current;
    }
    charge(&mut machine, current, now);
    settle(&mut machine, current);
    let next = pick_and_reserve(&mut machine, cpu, now);
    // This processor is taking work itself, which settles a wake it owed --
    // unless more than one thread was woken, in which case the rest is another
    // idle processor's to take.
    if machine.cpus[cpu].wake_pending {
        machine.cpus[cpu].wake_pending = false;
        if ready_backlog(&machine, cpu, next) {
            interrupt_idle(&mut machine, cpu);
        }
    }
    if next == current {
        machine.threads[current].quantum_ticks =
            quantum_ticks_for(machine.threads[current].dispatch_reserved_ns);
        return None;
    }

    // The outgoing thread keeps its `Running` state until this processor's
    // incoming context publishes it, which is what stops another processor from
    // resuming a context that is still being written.
    machine.cpus[cpu].previous = current;

    let migrated_from = machine.threads[next].last_cpu;
    if migrated_from != usize::MAX && migrated_from != cpu {
        machine.threads[next].migrations += 1;
    }
    machine.threads[next].last_cpu = cpu;
    machine.threads[next].state = ThreadState::Running;
    machine.threads[next].dispatched_ns = now;
    machine.threads[next].quantum_ticks =
        quantum_ticks_for(machine.threads[next].dispatch_reserved_ns);
    machine.cpus[cpu].current = next;
    if machine.threads[next].kind == ThreadKind::User {
        machine.cpus[cpu].dispatches += 1;
        let domain = machine.threads[next].domain;
        if domain < machine.domains.len() {
            machine.domains[domain].cpu_mask |= 1u64 << cpu;
        }
    }

    // SAFETY: the tables live in a `static`, so pointers into them stay valid
    // after the guard is dropped. Deriving them from the static's own pointer
    // rather than from the guard keeps the guard's borrow out of the provenance
    // chain. Neither thread can be touched by another processor while the
    // switch runs: the incoming one is `Running` and named by this processor's
    // `current`, and the outgoing one is `Running` and unpublished.
    let (save_rsp, save_fpu, load_fpu) = unsafe {
        (
            &raw mut (*machine_ptr).threads[current].saved_rsp,
            &raw mut (*machine_ptr).threads[current].fpu,
            &raw const (*machine_ptr).threads[next].fpu,
        )
    };

    Some(Plan {
        next,
        save_rsp,
        load_rsp: machine.threads[next].saved_rsp,
        save_fpu,
        load_fpu,
        kstack_top: machine.threads[next].kstack_top,
        cr3: machine.threads[next].cr3,
        fs_base: machine.threads[next].fs_base,
        migrated_from,
        migrations: machine.threads[next].migrations,
        domain: machine.threads[next].domain,
    })
}

/// Runs one scheduling decision on this processor and performs the switch it
/// asks for. Returns the thread now running here.
fn schedule(cpu: usize) -> usize {
    let Some(plan) = plan(cpu) else {
        return MACHINE.lock().cpus[cpu].current;
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
        if plan.cr3 != 0 && plan.cr3 != cpu::read_cr3() {
            cpu::write_cr3(plan.cr3);
        }
        // The incoming thread's own FS base, written unconditionally: the value
        // this processor holds is whatever the last thread here set, and a
        // thread must never run with another's. The kernel does not use FS, so
        // writing it before the switch changes nothing on this side.
        cpu::wrmsr(cpu::MSR_FS_BASE, plan.fs_base);
        context::switch_context(plan.save_rsp, plan.load_rsp);
    }

    finish_switch();
    plan.next
}

/// Charges the time `index` has run since it was dispatched.
fn charge(machine: &mut Machine, index: usize, now: u64) {
    let ran = now.saturating_sub(machine.threads[index].dispatched_ns);
    machine.threads[index].cpu_ns += ran;
    machine.threads[index].dispatched_ns = now;
    if machine.threads[index].kind != ThreadKind::User || ran == 0 {
        return;
    }
    // Measured on user execution only: an idle thread's first interval spans
    // the processor's whole bring-up and would say nothing about scheduling.
    if ran > machine.max_charge_interval_ns {
        machine.max_charge_interval_ns = ran;
    }
    let cpu = percpu::index();
    machine.cpus[cpu].user_ns = machine.cpus[cpu].user_ns.saturating_add(ran);
    let scope_index = machine.threads[index].effective_scope;
    let recovery = machine.threads[index].recovery;
    scope::charge_cpu(&mut machine.scopes, scope_index, ran, recovery);
}

/// Wakes every blocked thread whose deadline has passed.
fn expire_waits(machine: &mut Machine, now: u64) -> u32 {
    let mut woken = 0;
    for index in 0..machine.threads.len() {
        let thread = &machine.threads[index];
        if thread.state != ThreadState::Blocked
            || thread.wait_deadline_ns == 0
            || thread.wait_deadline_ns > now
        {
            continue;
        }
        machine.threads[index].wait = Wait::None;
        machine.threads[index].wait_deadline_ns = 0;
        machine.threads[index].wake_status = thalyx_abi::status::TIMED_OUT;
        machine.threads[index].state = ThreadState::Ready;
        woken += 1;
    }
    if woken != 0 {
        kick_idle(machine);
    }
    woken
}

/// Work every processor's timer does: charge, roll windows, expire waits.
///
/// All of it is under one lock and all of it is idempotent across processors,
/// so a machine with four timers ticking makes the same progress a machine with
/// one does, four times as often.
fn tick_bookkeeping(machine: &mut Machine, cpu: usize, now: u64) {
    machine.ticks += 1;
    machine.cpus[cpu].ticks += 1;
    let current = machine.cpus[cpu].current;
    // Charge the elapsed time now rather than only at a context switch, so the
    // number reported alongside a domain's own records is the time it had
    // actually consumed when it emitted them, and so the charge lands in the
    // window it was spent in.
    charge(machine, current, now);
    scope::roll_window(&mut machine.scopes, now);
    crate::api::evtops::expire(machine, now);
    expire_waits(machine, now);
    scope::advance_quiescence(&mut machine.scopes, now);
    if let Some(allocator) = machine.memory.as_mut() {
        allocator.drain_quarantine();
    }
}

/// Charges the current tick and preempts the running thread when its quantum
/// has expired. Called from the timer interrupt with interrupts masked.
pub fn on_tick(frame: &trap::TrapFrame) {
    let cpu = percpu::index();
    let (expired, current, starved, user) = {
        let mut machine = MACHINE.lock();
        let now = time::observe();
        tick_bookkeeping(&mut machine, cpu, now);
        let current = machine.cpus[cpu].current;
        let thread = &mut machine.threads[current];
        thread.quantum_ticks = thread.quantum_ticks.saturating_sub(1);
        let expired = thread.quantum_ticks == 0;
        let user = thread.kind == ThreadKind::User;
        let target = thread.effective_scope;
        let recovery = thread.recovery;
        let starved = user && scope::available_ns(&machine.scopes, target, recovery) == 0;
        (expired, current, starved, user)
    };

    if !expired && !starved {
        flush_wake();
        return;
    }
    if !user {
        schedule(cpu);
        return;
    }

    // The record is prepared before the switch, because afterwards this
    // processor is no longer standing in the interrupted thread's context and
    // the frame that proves the preemption was involuntary would be gone.
    let record = {
        let mut machine = MACHINE.lock();
        let records = machine.threads[current].preempt_records;
        if records < PREEMPT_RECORD_LIMIT {
            machine.threads[current].preempt_records += 1;
            machine.preempt_records += 1;
            let index = machine.threads[current].effective_scope as usize;
            Some((
                machine.threads[current].domain,
                machine.scopes[index].id,
                machine.scopes[index].cpu_window_ns,
            ))
        } else {
            None
        }
    };

    let next = schedule(cpu);
    if next == current {
        flush_wake();
        return;
    }

    {
        let mut machine = MACHINE.lock();
        machine.threads[current].preemptions += 1;
        machine.preemptions += 1;
        machine.cpus[cpu].preemptions += 1;
    }
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
/// `Blocked` under the machine lock, and has released that lock. When this
/// returns, the wake status the waker left is the reason it returned.
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
    let stopped = {
        let machine = MACHINE.lock();
        let current = machine.cpus[cpu].current;
        if current == usize::MAX {
            None
        } else {
            let thread = &machine.threads[current];
            (thread.kind == ThreadKind::User && thread.state == ThreadState::Dead)
                .then_some((current, thread.domain))
        }
    };
    let Some((thread, domain)) = stopped else {
        return;
    };
    let name = crate::domain::domain_name(domain);
    event!(
        "thread.stopped_late",
        "cpu={cpu} domain={domain} name={name} thread={thread} rip=0x{:x} vector=0x{:x} \
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
    {
        let mut machine = MACHINE.lock();
        let current = machine.cpus[cpu].current;
        if let Some(scope_index) = machine.threads[current].parallelism_scope.take() {
            scope::drop_parallelism(&mut machine.scopes, scope_index);
        }
    }
    schedule(cpu);
    // Reached only if the scheduler picked the dead thread itself, which
    // `candidate` refuses to do once its state is `Dead`. There is nothing to
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

/// True when some thread is waiting inside a kernel entry.
fn any_blocked(machine: &Machine) -> bool {
    machine
        .threads
        .iter()
        .any(|thread| thread.state == ThreadState::Blocked)
}

/// True when something can still change the set of eligible threads.
fn progress_possible(machine: &Machine) -> bool {
    let ready_but_starved = machine.threads.iter().enumerate().any(|(index, thread)| {
        index >= MAX_CPUS
            && matches!(thread.state, ThreadState::Ready | ThreadState::Running)
            && thread.kind == ThreadKind::User
    });
    let timed_wait = machine
        .threads
        .iter()
        .any(|thread| thread.state == ThreadState::Blocked && thread.wait_deadline_ns != 0);
    ready_but_starved || timed_wait || crate::api::evtops::any_armed(machine)
}

/// One turn of a processor's idle loop: reclaim, look for work, take it.
///
/// Returns the thread it dispatched, or `None` when the processor found
/// nothing and should consider whether the run is over.
fn idle_turn(cpu: usize) -> Option<usize> {
    crate::domain::reap_dead();
    {
        let mut machine = MACHINE.lock();
        let now = time::observe();
        tick_bookkeeping(&mut machine, cpu, now);
        // The bookkeeping above is the timer's work, done here because an idle
        // processor still has to roll windows and expire waits. It is not a
        // tick, so it does not count as one.
        machine.ticks -= 1;
        machine.cpus[cpu].ticks -= 1;
    }
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
        if idle_turn(cpu).is_some() || poll_before_halt() {
            continue;
        }
        // SAFETY: no lock is held, and the handler that runs may switch away
        // from this context and back, which an idle context is built to
        // survive.
        unsafe { cpu::wait_for_interrupt() };
    }
}

/// Runs the bootstrap processor's idle thread: reclaims what died, waits for
/// what can still happen, and returns when nothing can.
///
/// The decision is made here and only here. A processor that finds nothing to
/// run cannot conclude the run is over, because another processor may be in the
/// middle of making something runnable; the bootstrap processor decides from
/// the whole table, under the lock, and then stops the others.
pub fn run_until_idle() -> Terminal {
    let cpu = percpu::index();
    loop {
        if idle_turn(cpu).is_some() {
            continue;
        }

        let (blocked, possible, any, busy) = {
            let machine = MACHINE.lock();
            let blocked = any_blocked(&machine);
            let possible = progress_possible(&machine);
            let any = machine.threads.iter().enumerate().any(|(index, thread)| {
                index >= MAX_CPUS
                    && matches!(
                        thread.state,
                        ThreadState::Ready | ThreadState::Running | ThreadState::Blocked
                    )
            });
            // A thread another processor is running is not idleness, whatever
            // this processor can see to dispatch.
            let busy = (0..MAX_CPUS).any(|other| {
                other != cpu
                    && machine.cpus[other].online
                    && machine.cpus[other].current != idle_thread(other)
                    && machine.cpus[other].current != usize::MAX
            });
            (blocked, possible, any, busy)
        };

        if busy {
            if poll_before_halt() {
                continue;
            }
            // SAFETY: no lock is held.
            unsafe { cpu::wait_for_interrupt() };
            continue;
        }
        if !any {
            return Terminal::NoRunnableDomain;
        }
        if !possible {
            return Terminal::Deadlock;
        }
        let _ = blocked;
        if poll_before_halt() {
            continue;
        }
        // Something can still happen, but not here and not now. Let the timer
        // in: this and the application processors' idle loops are the only
        // points where interrupts are enabled outside user mode, and no lock is
        // held across it.
        // SAFETY: no lock is held.
        unsafe { cpu::wait_for_interrupt() };
    }
}
