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
use crate::sched;
use crate::state::ThreadKind;
use crate::tlb;
use crate::{event, trace};

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
    let current = crate::sched::current_thread();
    note_entry_of(frame, current, crate::thread::get(current));
}

/// The same, for a caller that has already found this processor's thread.
///
/// Every entry from user mode passes through one of these, so neither takes a
/// lock and the common case is a single read: the confirmation is a one-shot
/// flag on this processor's own current thread, already set for every entry
/// after the first.
pub fn note_entry_of(frame: &TrapFrame, current: usize, cell: &'static crate::thread::ThreadCell) {
    let announce = {
        if cell.kind() != ThreadKind::User
            || cell
                .ring3_confirmed
                .load(core::sync::atomic::Ordering::Relaxed)
            || cell
                .ring3_confirmed
                .swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            None
        } else {
            Some((current, cell.domain()))
        }
    };
    if let Some((thread, domain)) = announce {
        let name = crate::domain::domain_name(domain);
        trace!(
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
            // Acknowledged first: the switch below does not return to this
            // frame, and a controller left waiting for an acknowledgement
            // would deliver nothing else to this processor.
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
            // A processor told to look again does so here rather than on its
            // next timer tick. That is the difference between a wake taking
            // effect in microseconds and taking effect in a tick.
            sched::on_reschedule(frame);
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
            // The interrupted thread keeps running; a driver thread the
            // interrupt woke is handed to an idle processor now.
            sched::on_kernel_exit();
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

    // Every return to ring 3 passes here. A thread whose domain was terminated
    // by authority while it was running does not get the return.
    sched::leave_if_dead(frame);
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
