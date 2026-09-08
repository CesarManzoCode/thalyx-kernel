//! Scheduling.
//!
//! K1 implements the mechanism the resource contract needs and none of the
//! policy that depends on objects it does not have. There is round-robin
//! selection between runnable threads, a quantum measured in timer ticks, CPU
//! time charged in nanoseconds to the thread that ran, and involuntary
//! preemption from the timer interrupt. There is no scope tree, no hierarchical
//! fair share, no budget, no window and no parallelism slot: all of those are
//! properties of scopes, which arrive in K2. The per-thread nanosecond charge
//! is the substrate they will aggregate.
//!
//! Preemption is involuntary by construction. A user thread is switched away
//! from inside the timer interrupt handler, at an instruction it did not
//! choose, and the interrupted frame is recorded so that claim can be checked
//! rather than believed.

use crate::arch::x86_64::{context, cpu, fpu, gdt, trap};
use crate::event;
use crate::state::{IDLE_THREAD, MACHINE, MAX_THREADS, ThreadKind, ThreadState};
use crate::time;

/// Timer interrupts per second.
pub const TICK_HZ: u64 = 2000;
/// Scheduling quantum. The value the resource contract fixes for V0.
pub const QUANTUM_NS: u64 = 1_000_000;
/// Quantum expressed in timer ticks.
pub const QUANTUM_TICKS: u32 = (QUANTUM_NS * TICK_HZ / 1_000_000_000) as u32;

const _: () = assert!(QUANTUM_TICKS >= 1);

/// Detailed preemption records emitted per thread before the diagnostic plane
/// starts coalescing them into counters. The bound is per thread rather than
/// global so one busy domain cannot consume the whole budget and leave another
/// domain's preemptions unrecorded. The observability contract permits
/// coalescing; it requires saying so, which the summary record does.
pub const PREEMPT_RECORD_LIMIT: u32 = 8;

/// Chooses the next thread to run, or the idle thread when none is ready.
fn pick(machine: &mut crate::state::Machine) -> usize {
    let start = machine.cursor;
    for step in 0..MAX_THREADS {
        let index = (start + step) % MAX_THREADS;
        if index == IDLE_THREAD {
            continue;
        }
        let thread = &machine.threads[index];
        if thread.state == ThreadState::Ready || thread.state == ThreadState::Running {
            machine.cursor = (index + 1) % MAX_THREADS;
            return index;
        }
    }
    IDLE_THREAD
}

/// True when at least one user thread can still run.
#[must_use]
pub fn has_runnable_user_thread() -> bool {
    let machine = MACHINE.lock();
    machine.threads.iter().enumerate().any(|(index, thread)| {
        index != IDLE_THREAD && matches!(thread.state, ThreadState::Ready | ThreadState::Running)
    })
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
        let ran = now.saturating_sub(machine.threads[current].dispatched_ns);
        machine.threads[current].cpu_ns += ran;
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
        // runs: K1 is uniprocessor and interrupts are masked in kernel context.
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
        let thread = &mut machine.threads[current];
        thread.cpu_ns += now.saturating_sub(thread.dispatched_ns);
        thread.dispatched_ns = now;
        thread.quantum_ticks = thread.quantum_ticks.saturating_sub(1);
        (thread.quantum_ticks == 0, current)
    };

    if !expired {
        return;
    }

    let next = {
        let mut machine = MACHINE.lock();
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
                drop(machine);
                event!(
                    "sched.preempt",
                    "domain={domain} thread={current} next={next} rip=0x{:x} cs=0x{:x} cpl={} \
                     trigger=timer vector=0x{:x} voluntary=0",
                    frame.rip,
                    frame.cs,
                    frame.cpl(),
                    trap::TIMER_VECTOR
                );
            }
        }
    }

    switch_to(next);
}

/// Leaves a thread that will never run again and does not return.
///
/// The dying thread's kernel stack is still in use up to the switch, which is
/// why reclamation is deferred to a context that is no longer standing on it.
pub fn switch_away_from_dead() -> ! {
    let next = {
        let mut machine = MACHINE.lock();
        pick(&mut machine)
    };
    switch_to(next);
    // Reached only if the scheduler picked the dead thread itself, which `pick`
    // refuses to do once its state is `Dead`. There is nothing to retry: the
    // stack this runs on is the dead thread's, so halting is the only correct
    // end.
    cpu::halt_forever()
}

/// Runs the idle thread: reclaims what died, and returns when no user thread
/// can run again.
pub fn run_until_idle() {
    loop {
        crate::domain::reap_dead();
        if !has_runnable_user_thread() {
            return;
        }
        let next = {
            let mut machine = MACHINE.lock();
            pick(&mut machine)
        };
        if next == IDLE_THREAD {
            return;
        }
        switch_to(next);
    }
}
