//! Trap classification.
//!
//! Classification is by the privilege level recorded in the interrupted code
//! selector, not by the address involved or by what the kernel expected. A
//! frame from ring 3 is a user fault and stops that domain; a frame from ring 0
//! is a kernel failure and stops the machine. Nothing in between is treated as
//! "probably fine".

use crate::arch::x86_64::cpu;
use crate::arch::x86_64::lapic;
use crate::arch::x86_64::trap::{SPURIOUS_VECTOR, TIMER_VECTOR, TrapFrame};
use crate::event;
use crate::sched;
use crate::state::{MACHINE, ThreadKind};

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
        let current = machine.current;
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
            "domain={domain} name={name} thread={thread} cs=0x{:x} ss=0x{:x} cpl={} \
             rflags=0x{:x} iopl={} rip=0x{:x}",
            frame.cs,
            frame.ss,
            frame.cpl(),
            frame.rflags,
            (frame.rflags >> 12) & 3,
            frame.rip
        );
    }
}

/// Handles one interrupt or exception.
pub fn handle(frame: &mut TrapFrame) {
    // CR2 is read first: any later fault would overwrite it.
    let cr2 = if frame.vector == VECTOR_PAGE_FAULT {
        cpu::read_cr2()
    } else {
        0
    };

    note_user_entry(frame);

    match frame.vector {
        vector if vector == u64::from(TIMER_VECTOR) => {
            if let Some(lapic) = lapic::current() {
                lapic.end_of_interrupt();
            }
            sched::on_tick(frame);
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
                "vector={vector} cpl={} rip=0x{:x}",
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
