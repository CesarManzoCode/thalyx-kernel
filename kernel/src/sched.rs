//! Scheduling.
//!
//! The mechanism is preemptive round-robin over threads, and the policy is the
//! resource contract's: a thread is eligible only if the scope its current work
//! is charged to, **and every ancestor of that scope**, still has budget in the
//! current window. Budget is aggregate. Two workers that adopted the same
//! client's scope draw from one balance, so a client gains nothing by making a
//! server run more threads on its behalf, and a scope gains nothing by having
//! more children.
//!
//! Windows are fixed and aligned, of the period the contract fixes. An overrun
//! is not forgiven at the boundary: it starts the next window already consumed.
//! That is a bounded, visible debt rather than a reset, and it is what makes
//! "the excess is deducted from the next replenishment" mean something.
//!
//! Preemption stays involuntary by construction. A user thread is switched away
//! from inside the timer interrupt, at an instruction it did not choose, and
//! the interrupted frame is recorded so the claim can be checked rather than
//! believed.

use crate::arch::x86_64::{context, cpu, fpu, gdt, trap};
use crate::event;
use crate::scope;
use crate::state::{IDLE_THREAD, MACHINE, MAX_THREADS, Machine, ThreadKind, ThreadState, Wait};
use crate::time;

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

/// Whether a thread may be dispatched right now.
fn eligible(machine: &Machine, index: usize) -> bool {
    let thread = &machine.threads[index];
    if index == IDLE_THREAD {
        return false;
    }
    if !matches!(thread.state, ThreadState::Ready | ThreadState::Running) {
        return false;
    }
    if thread.kind != ThreadKind::User {
        return true;
    }
    scope::eligible(&machine.scopes, thread.effective_scope, thread.recovery)
}

/// Chooses the next thread to run, or the idle thread when none is eligible.
fn pick(machine: &mut Machine) -> usize {
    let start = machine.cursor;
    for step in 0..MAX_THREADS {
        let index = (start + step) % MAX_THREADS;
        if eligible(machine, index) {
            machine.cursor = (index + 1) % MAX_THREADS;
            return index;
        }
    }
    IDLE_THREAD
}

/// True when at least one user thread is ready or running, eligible or not.
#[must_use]
pub fn has_runnable_user_thread() -> bool {
    let machine = MACHINE.lock();
    machine.threads.iter().enumerate().any(|(index, thread)| {
        index != IDLE_THREAD && matches!(thread.state, ThreadState::Ready | ThreadState::Running)
    })
}

/// True when some thread is waiting inside a kernel entry.
fn any_blocked(machine: &Machine) -> bool {
    machine
        .threads
        .iter()
        .any(|thread| thread.state == ThreadState::Blocked)
}

/// True when something can still change the set of eligible threads.
///
/// A thread that is ready but over budget becomes eligible again at the next
/// window boundary, which the timer drives. A thread blocked with a deadline
/// will be woken. An armed timer will fire. If none of those hold and nothing
/// is eligible, nothing ever will be, and saying so is more useful than
/// spinning.
fn progress_possible(machine: &Machine) -> bool {
    let ready_but_starved = machine.threads.iter().enumerate().any(|(index, thread)| {
        index != IDLE_THREAD
            && matches!(thread.state, ThreadState::Ready | ThreadState::Running)
            && thread.kind == ThreadKind::User
    });
    let timed_wait = machine
        .threads
        .iter()
        .any(|thread| thread.state == ThreadState::Blocked && thread.wait_deadline_ns != 0);
    ready_but_starved || timed_wait || crate::api::evtops::any_armed(machine)
}

/// Switches the CPU to `next`, saving the outgoing thread's context.
///
/// The lock is released before the switch: the thread resumed here would not
/// release it, and the outgoing thread will re-acquire it when it resumes.
fn switch_to(next: usize) {
    let machine_ptr = MACHINE.as_mut_ptr();
    let save_rsp: *mut u64;
    let load_rsp: u64;
    let kstack_top: u64;
    let cr3: u64;
    let save_fpu: *mut fpu::FpuState;
    let load_fpu: *const fpu::FpuState;

    {
        let mut machine = MACHINE.lock();
        let current = machine.current;
        if current == next {
            machine.threads[current].quantum_ticks = QUANTUM_TICKS;
            return;
        }

        let now = time::monotonic_ns().unwrap_or(0);
        charge(&mut machine, current, now);
        if machine.threads[current].state == ThreadState::Running {
            machine.threads[current].state = ThreadState::Ready;
        }

        machine.threads[next].state = ThreadState::Running;
        machine.threads[next].dispatched_ns = now;
        machine.threads[next].quantum_ticks = QUANTUM_TICKS;
        machine.current = next;

        kstack_top = machine.threads[next].kstack_top;
        cr3 = machine.threads[next].cr3;
        load_rsp = machine.threads[next].saved_rsp;

        // SAFETY: the tables live in a `static`, so pointers into them stay
        // valid after the guard is dropped. Deriving them from the static's own
        // pointer rather than from the guard keeps the guard's borrow out of the
        // provenance chain. Nothing else can touch these fields while the switch
        // runs: this is a uniprocessor kernel and interrupts are masked in
        // kernel context.
        unsafe {
            save_rsp = &raw mut (*machine_ptr).threads[current].saved_rsp;
            save_fpu = &raw mut (*machine_ptr).threads[current].fpu;
            load_fpu = &raw const (*machine_ptr).threads[next].fpu;
        }
    }

    // SAFETY: the FP areas are the two threads' own 16-byte-aligned save areas;
    // `kstack_top` is the incoming thread's mapped kernel stack; `cr3` is the
    // root of an address space that shares the kernel's upper half, so the code
    // and stack executing here stay mapped across the write; and `load_rsp` is a
    // stopped context this kernel built. No lock is held.
    unsafe {
        fpu::save(save_fpu);
        fpu::restore(load_fpu);
        gdt::set_kernel_stack(kstack_top);
        trap::set_syscall_stack(kstack_top);
        if cr3 != 0 && cr3 != cpu::read_cr3() {
            cpu::write_cr3(cr3);
        }
        context::switch_context(save_rsp, load_rsp);
    }
}

/// Charges the time `index` has run since it was dispatched.
fn charge(machine: &mut Machine, index: usize, now: u64) {
    let ran = now.saturating_sub(machine.threads[index].dispatched_ns);
    machine.threads[index].cpu_ns += ran;
    machine.threads[index].dispatched_ns = now;
    if machine.threads[index].kind != ThreadKind::User || ran == 0 {
        return;
    }
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
    woken
}

/// Charges the current tick and preempts the running thread when its quantum
/// has expired. Called from the timer interrupt with interrupts masked.
pub fn on_tick(frame: &trap::TrapFrame) {
    let (expired, current) = {
        let mut machine = MACHINE.lock();
        machine.ticks += 1;
        let current = machine.current;
        // Charge the elapsed time now rather than only at a context switch, so
        // the number reported alongside a domain's own records is the time it
        // had actually consumed when it emitted them.
        let now = time::monotonic_ns().unwrap_or(0);
        charge(&mut machine, current, now);
        scope::roll_window(&mut machine.scopes, now);
        crate::api::evtops::expire(&mut machine, now);
        expire_waits(&mut machine, now);
        scope::advance_quiescence(&mut machine.scopes, now);
        let thread = &mut machine.threads[current];
        thread.quantum_ticks = thread.quantum_ticks.saturating_sub(1);
        (thread.quantum_ticks == 0, current)
    };

    let starved = {
        let machine = MACHINE.lock();
        machine.threads[current].kind == ThreadKind::User && !eligible(&machine, current)
    };

    if !expired && !starved {
        return;
    }

    let next = {
        let mut machine = MACHINE.lock();
        if starved {
            machine.budget_stalls += 1;
        }
        pick(&mut machine)
    };
    if next == current {
        let mut machine = MACHINE.lock();
        machine.threads[current].quantum_ticks = QUANTUM_TICKS;
        return;
    }

    {
        let mut machine = MACHINE.lock();
        if machine.threads[current].kind == ThreadKind::User {
            machine.threads[current].preemptions += 1;
            machine.preemptions += 1;
            let records = machine.threads[current].preempt_records;
            let domain = machine.threads[current].domain;
            if records < PREEMPT_RECORD_LIMIT {
                machine.threads[current].preempt_records += 1;
                machine.preempt_records += 1;
                let scope_id = {
                    let index = machine.threads[current].effective_scope as usize;
                    machine.scopes[index].id
                };
                let used = {
                    let index = machine.threads[current].effective_scope as usize;
                    machine.scopes[index].cpu_window_ns
                };
                drop(machine);
                event!(
                    "sched.preempt",
                    "domain={domain} thread={current} next={next} rip=0x{:x} cs=0x{:x} cpl={} \
                     trigger=timer vector=0x{:x} voluntary=0 effective_scope={scope_id} \
                     window_used_ns={used} starved={}",
                    frame.rip,
                    frame.cs,
                    frame.cpl(),
                    trap::TIMER_VECTOR,
                    u8::from(starved)
                );
            }
        }
    }

    switch_to(next);
}

/// Gives up the CPU until something wakes this thread.
///
/// The caller has already published what it is waiting for and moved itself to
/// `Blocked` under the machine lock, and has released that lock. When this
/// returns, the wake status the waker left is the reason it returned.
pub fn block_current() {
    let next = {
        let mut machine = MACHINE.lock();
        pick(&mut machine)
    };
    switch_to(next);
}

/// Leaves a thread that will never run again and does not return.
///
/// The dying thread's kernel stack is still in use up to the switch, which is
/// why reclamation is deferred to a context that is no longer standing on it.
pub fn switch_away_from_dead() -> ! {
    let next = {
        let mut machine = MACHINE.lock();
        let current = machine.current;
        if let Some(scope_index) = machine.threads[current].parallelism_scope.take() {
            scope::drop_parallelism(&mut machine.scopes, scope_index);
        }
        pick(&mut machine)
    };
    switch_to(next);
    // Reached only if the scheduler picked the dead thread itself, which `pick`
    // refuses to do once its state is `Dead`. There is nothing to retry: the
    // stack this runs on is the dead thread's, so halting is the only correct
    // end.
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

/// Runs the idle thread: reclaims what died, waits for what can still happen,
/// and returns when nothing can.
pub fn run_until_idle() -> Terminal {
    loop {
        crate::domain::reap_dead();

        let (next, blocked, possible, any) = {
            let mut machine = MACHINE.lock();
            let now = time::monotonic_ns().unwrap_or(0);
            scope::roll_window(&mut machine.scopes, now);
            crate::api::evtops::expire(&mut machine, now);
            expire_waits(&mut machine, now);
            scope::advance_quiescence(&mut machine.scopes, now);
            let next = pick(&mut machine);
            let blocked = any_blocked(&machine);
            let possible = progress_possible(&machine);
            let any = machine.threads.iter().enumerate().any(|(index, thread)| {
                index != IDLE_THREAD
                    && matches!(
                        thread.state,
                        ThreadState::Ready | ThreadState::Running | ThreadState::Blocked
                    )
            });
            (next, blocked, possible, any)
        };

        if next != IDLE_THREAD {
            switch_to(next);
            continue;
        }
        if !any {
            return Terminal::NoRunnableDomain;
        }
        if !possible && !blocked {
            return Terminal::Deadlock;
        }
        if !possible {
            return Terminal::Deadlock;
        }
        // Something can still happen, but not here and not now. Let the timer
        // in: this is the only point in the kernel where interrupts are enabled
        // outside user mode, and no lock is held across it.
        // SAFETY: no lock is held, and the handler that runs may switch away
        // from this context and back, which the idle context is built to
        // survive.
        unsafe { cpu::wait_for_interrupt() };
    }
}
