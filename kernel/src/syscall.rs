//! Kernel entry dispatch.
//!
//! K1 implements one assigned ABI entry, the reserved version query, and two
//! entries in the scaffolding namespace that K2 removes. Everything else is
//! refused with `UNSUPPORTED_ENTRY` rather than ignored, so a program built
//! against a future opcode table fails visibly instead of silently succeeding.
//!
//! No entry here takes a pointer, so the kernel never reads user memory in K1
//! and no user-copy path exists to get wrong. That path arrives with
//! descriptors in K2, together with the bounds and fault handling it needs.

use thalyx_abi::{entry, note, scaffold, status};

use crate::arch::x86_64::trap::TrapFrame;
use crate::event;
use crate::state::{MACHINE, ThreadKind};

/// Handles one `syscall` entry.
pub fn handle(frame: &mut TrapFrame) {
    crate::trap::note_user_entry(frame);

    let (domain, thread) = {
        let mut machine = MACHINE.lock();
        let current = machine.current;
        machine.threads[current].syscalls += 1;
        if machine.threads[current].kind != ThreadKind::User {
            // Ring 0 cannot execute `syscall` in this kernel; reaching here
            // would mean the entry was taken from a context that has no domain.
            (usize::MAX, current)
        } else {
            (machine.threads[current].domain, current)
        }
    };

    match frame.rax {
        entry::VERSION_QUERY => {
            frame.rax = status::OK as u64;
            frame.rdx =
                thalyx_abi::pack_version(thalyx_abi::VERSION_MAJOR, thalyx_abi::VERSION_MINOR);
        }
        scaffold::DIAG_NOTE => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            diag_note(domain, thread, frame);
        }
        scaffold::DIAG_EXIT => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            crate::domain::terminate_voluntarily(frame.rsi);
        }
        other => {
            frame.rax = status::UNSUPPORTED_ENTRY as u64;
            frame.rdx = 0;
            event!(
                "user.unsupported_entry",
                "domain={domain} entry=0x{other:x} rip=0x{:x}",
                frame.rip
            );
        }
    }
}

fn diag_note(domain: usize, thread: usize, frame: &mut TrapFrame) {
    let kind = frame.rsi;
    let first = frame.rdx;
    let second = frame.r10;

    let (sequence, cpu_ns, preemptions, syscalls) = {
        let mut machine = MACHINE.lock();
        machine.domains[domain].notes += 1;
        (
            machine.domains[domain].notes,
            machine.threads[thread].cpu_ns,
            machine.threads[thread].preemptions,
            machine.threads[thread].syscalls,
        )
    };

    let name = crate::domain::domain_name(domain);
    let kind_name = match kind {
        note::PROGRESS => "progress",
        note::SELF_CHECK => "self_check",
        note::PROBE_INTENT => "probe_intent",
        _ => "unknown",
    };

    event!(
        "user.note",
        "domain={domain} name={name} thread={thread} kind={kind_name} kind_id={kind} \
         a=0x{first:x} b=0x{second:x} note_seq={sequence} cpu_ns={cpu_ns} \
         preemptions={preemptions} syscalls={syscalls}"
    );

    frame.rax = status::OK as u64;
    frame.rdx = domain as u64;
}
