//! Trap classification.
//!
//! Classification is by the privilege level recorded in the interrupted code
//! selector, not by the address involved or by what the kernel expected. A
//! frame from ring 3 is a user fault and stops that domain; a frame from ring 0
//! is a kernel failure and stops the machine. Nothing in between is treated as
//! "probably fine".

use crate::arch::x86_64::cpu;
use crate::arch::x86_64::idt;
use crate::arch::x86_64::lapic;
use crate::arch::x86_64::trap::{
    DEVICE_VECTOR_BASE, DEVICE_VECTOR_COUNT, RESCHEDULE_VECTOR, SPURIOUS_VECTOR, TIMER_VECTOR,
    TLB_VECTOR, TrapFrame,
};
use crate::event;
use crate::sched;
use crate::state::{MACHINE, ThreadKind};
use crate::tlb;

const VECTOR_PAGE_FAULT: u64 = 14;

/// Records the first observation of a domain executing at privilege level 3.
///
/// The evidence that ring 3 was reached is this frame, not the fact that the
/// kernel executed an `iretq`: the selector and privilege level here were
/// pushed by the CPU on the way *out* of user mode.
pub fn note_user_entry(frame: &TrapFrame) {
    if !frame.from_user() {
        return;
    }
    let announce = {
        let mut machine = MACHINE.lock();
        let current = machine.current();
        if machine.threads[current].kind != ThreadKind::User
            || machine.threads[current].ring3_confirmed
        {
            None
        } else {
            machine.threads[current].ring3_confirmed = true;
            Some((current, machine.threads[current].domain))
        }
    };
    if let Some((thread, domain)) = announce {
        let name = crate::domain::domain_name(domain);
        event!(
            "user.ring3_confirmed",
            "cpu={} domain={domain} name={name} thread={thread} cs=0x{:x} ss=0x{:x} cpl={} \
             rflags=0x{:x} iopl={} rip=0x{:x}",
            crate::percpu::index(),
            frame.cs,
            frame.ss,
            frame.cpl(),
            frame.rflags,
            (frame.rflags >> 12) & 3,
            frame.rip
        );
    }
}

/// The three vectors that may arrive with a `GS` base that is not this
/// processor's.
///
/// They run on their own stacks and they can interrupt the two instructions of
/// the `syscall` entry, before the exchange that installs the per-processor
/// block. Nothing on this path reads that block: the processor identifies
/// itself from its own interrupt controller instead, and the record is written
/// without taking a lock, because the interrupted context may hold one.
fn diverted(frame: &TrapFrame, cr2: u64) -> ! {
    let apic_id = lapic::local_apic_id();
    let cpu = crate::percpu::index_by_apic_id(apic_id);
    // SAFETY: interrupts are masked by the gate. Another processor could in
    // principle be writing the diagnostic plane; the alternative is taking a
    // lock the interrupted context may already hold, which would hang instead
    // of reporting. The observability contract permits a lossy diagnostic
    // record and forbids a silent one.
    unsafe {
        crate::diag::emit_event_unlocked(
            "kernel.fatal_vector",
            format_args!(
                "vector={} apic_id={apic_id} cpu={} error=0x{:x} cr2=0x{cr2:x} rip=0x{:x} \
                 cs=0x{:x} rsp=0x{:x} gs_trusted=0",
                frame.vector,
                match cpu {
                    Some(index) => index as i64,
                    None => -1,
                },
                frame.error_code,
                frame.rip,
                frame.cs,
                frame.rsp
            ),
        );
    }
    panic!(
        "fatal vector={} error=0x{:x} cr2=0x{:x} rip=0x{:x}",
        frame.vector, frame.error_code, cr2, frame.rip
    );
}

/// Handles one interrupt or exception.
pub fn handle(frame: &mut TrapFrame) {
    // CR2 is read first: any later fault would overwrite it.
    let cr2 = if frame.vector == VECTOR_PAGE_FAULT {
        cpu::read_cr2()
    } else {
        0
    };

    // Before anything that reads per-processor state, because these three are
    // the entries that may arrive without it.
    if matches!(
        frame.vector as usize,
        idt::VECTOR_NMI | idt::VECTOR_DOUBLE_FAULT | idt::VECTOR_MACHINE_CHECK
    ) {
        diverted(frame, cr2);
    }

    note_user_entry(frame);

    match frame.vector {
        vector if vector == u64::from(TIMER_VECTOR) => {
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
            sched::on_tick(frame);
        }
        vector if vector == u64::from(TLB_VECTOR) => {
            tlb::on_shootdown_interrupt();
        }
        vector if vector == u64::from(RESCHEDULE_VECTOR) => {
            // Nothing to do beyond returning: the processor re-enters its idle
            // loop, which is where it reconsiders what to run and whether the
            // run is over.
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
        }
        vector
            if vector >= u64::from(DEVICE_VECTOR_BASE)
                && vector < u64::from(DEVICE_VECTOR_BASE) + u64::from(DEVICE_VECTOR_COUNT) =>
        {
            // Acknowledged before the binding is looked up: the controller has
            // delivered it either way, and an interrupt for a binding that has
            // gone must not leave the controller waiting.
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
            crate::device::on_interrupt(vector as u8);
        }
        vector if vector == u64::from(SPURIOUS_VECTOR) => {
            // A spurious interrupt is not acknowledged.
        }
        vector if vector < 32 => exception(frame, cr2),
        vector => {
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
            event!(
                "irq.unexpected",
                "cpu={} vector={vector} cpl={} rip=0x{:x}",
                crate::percpu::index(),
                frame.cpl(),
                frame.rip
            );
        }
    }
}

fn exception(frame: &mut TrapFrame, cr2: u64) -> ! {
    if frame.from_user() {
        crate::domain::terminate_on_fault(frame, cr2);
    }

    // A fault at ring 0 is a failure of the kernel's own invariants.
    panic!(
        "kernel exception vector={} error=0x{:x} cr2=0x{:x} rip=0x{:x} rsp=0x{:x} cs=0x{:x}",
        frame.vector, frame.error_code, cr2, frame.rip, frame.rsp, frame.cs
    );
}
