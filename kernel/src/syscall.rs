//! Kernel entry dispatch.
//!
//! K2 assigns five entries. [`entry::VERSION_QUERY`], [`entry::LIMITS_QUERY`]
//! and [`entry::CLOCK_QUERY`] describe the interface and the moment, and need
//! no authority: a program has to be able to learn what it is talking to before
//! it holds anything, and every deadline this interface takes is a monotonic
//! reading it must be able to obtain. [`entry::EXIT`] ends the caller, which is
//! authority over oneself and therefore needs no capability. Everything else in
//! the interface arrives through [`entry::INVOKE`], which names a handle and an
//! operation and is the only entry that touches an object.
//!
//! That single invoking entry is the point of the shape. Structure, authority
//! and effect are checked in that order in exactly one place ([`crate::api`]),
//! so there is no second path into an object that could skip a barrier or a
//! rights check. An unassigned entry is refused with `UNSUPPORTED_ENTRY`
//! rather than ignored, so a program built against a future table fails
//! visibly instead of silently succeeding.
//!
//! The two K1 scaffolding entries stay, carrying their scaffolding bit. They
//! are not ABI, they touch no object, and they exist so the K1 regression keeps
//! running against the kernel K2 grew into. Removing them would mean losing the
//! evidence that protected boot still works.

use thalyx_abi::generated::{Limits, entry, status as k2status};
use thalyx_abi::{limit, note, scaffold, status};

use thalyx_boot_protocol::{USER_MAX_ADDR, USER_MIN_ADDR};

use crate::arch::x86_64::cpu;
use crate::arch::x86_64::trap::TrapFrame;
use crate::state::{MACHINE, ThreadKind};
use crate::ucopy;
use crate::{event, trace};

/// Handles one `syscall` entry.
pub fn handle(frame: &mut TrapFrame) {
    crate::trap::note_user_entry(frame);

    let (domain, thread) = {
        let mut machine = MACHINE.lock();
        let current = machine.current();
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
        entry::INVOKE => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            let (code, aux) = crate::api::invoke(domain, thread, frame);
            frame.rax = code as u64;
            frame.rdx = aux;
        }
        entry::CLOCK_QUERY => {
            // The monotonic reading, and nothing else. It names no object, so
            // it needs no handle, and it reveals nothing another domain owns:
            // the passage of time is not authority. Refusing to expose it while
            // taking deadlines in six operations would make those deadlines
            // unusable rather than safe.
            frame.rax = k2status::OK as u64;
            frame.rdx = crate::api::now_ns();
        }
        entry::LIMITS_QUERY => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            limits_query(domain, frame);
        }
        entry::THREAD_POINTER_SET => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            thread_pointer_set(domain, thread, frame);
        }
        entry::EXIT => {
            if domain == usize::MAX {
                frame.rax = status::INVALID_ARGUMENT as u64;
                return;
            }
            crate::domain::terminate_voluntarily(frame.rsi);
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
    // This thread keeps running: a thread it woke and nobody picked up is
    // handed to an idle processor here, on the way out.
    crate::sched::flush_wake();

    // Every return to ring 3 passes here. A thread whose domain was terminated
    // by authority while it was inside this entry does not get the return.
    crate::sched::leave_if_dead(frame);
}

/// Writes the effective interface limits into the caller's buffer.
///
/// These are the interface's numbers, taken from the schema, not the kernel's
/// table capacities. A program sizes its buffers and its expectations from what
/// it reads here, so the only two values the kernel supplies itself are the
/// ones the schema cannot know: the page size it actually runs on and the epoch
/// this boot started at.
fn limits_query(domain: usize, frame: &mut TrapFrame) {
    if frame.r10 != core::mem::size_of::<Limits>() as u64 {
        frame.rax = k2status::INVALID_ARGUMENT as u64;
        frame.rdx = 0;
        return;
    }

    let (boot_epoch, cpus_online) = {
        let machine = MACHINE.lock();
        (machine.boot_epoch, machine.cpus_online as u32)
    };
    let limits = Limits {
        major: thalyx_abi::VERSION_MAJOR,
        minor: thalyx_abi::VERSION_MINOR,
        max_descriptor_len: limit::MAX_DESCRIPTOR_LEN as u32,
        max_inline_payload: limit::MAX_INLINE_PAYLOAD as u32,
        max_caps_per_message: limit::MAX_CAPS_PER_MESSAGE as u32,
        max_scope_depth: limit::MAX_SCOPE_DEPTH as u32,
        max_derive_depth: limit::MAX_DERIVE_DEPTH as u32,
        max_handles_per_domain: limit::MAX_HANDLES_PER_DOMAIN as u32,
        max_endpoint_queue: limit::MAX_ENDPOINT_QUEUE as u32,
        max_memory_pages_per_object: limit::MAX_MEMORY_PAGES_PER_OBJECT as u32,
        control_log_capacity: limit::CONTROL_LOG_CAPACITY as u32,
        control_log_reserved: limit::CONTROL_LOG_RESERVED as u32,
        receipt_batch: limit::RECEIPT_BATCH as u32,
        cpu_window_ns: limit::CPU_WINDOW_NS,
        cpu_quantum_ns: limit::CPU_QUANTUM_NS,
        page_size: limit::PAGE_SIZE,
        boot_epoch,
        // What the machine actually brought up and confirmed, not what the
        // firmware described: a program sizing its work by this number is
        // sizing it by processors it can really be scheduled on.
        cpus_online,
        reserved0: 0,
    };

    // SAFETY of the copy is `copy_out`'s: it walks the domain's own tables and
    // refuses a range that is not user-writable.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            core::ptr::from_ref(&limits).cast::<u8>(),
            core::mem::size_of::<Limits>(),
        )
    };
    let machine = MACHINE.lock();
    let Some(space) = machine.domains[domain].space.as_ref() else {
        drop(machine);
        frame.rax = k2status::PEER_DEAD as u64;
        frame.rdx = 0;
        return;
    };
    let written = ucopy::copy_out(space, frame.rdx, bytes);
    drop(machine);

    match written {
        Ok(()) => {
            frame.rax = k2status::OK as u64;
            frame.rdx = core::mem::size_of::<Limits>() as u64;
        }
        Err(_) => {
            frame.rax = k2status::INVALID_ADDRESS as u64;
            frame.rdx = 0;
        }
    }
}

/// Sets the calling thread's thread pointer.
///
/// The value is register state, like a stack pointer: it names no object, so it
/// needs no capability, and it reveals nothing another domain owns. What the
/// kernel owes it is that it stays the thread's own. It is recorded on the
/// thread and written to the FS base on every dispatch, so a thread never runs
/// with another thread's pointer, and nothing in ring 0 addresses memory
/// through FS.
///
/// Two values are refused before anything is written. A non-canonical address
/// would make the `WRMSR` fault in ring 0, and a kernel address is a pointer the
/// thread could never dereference; both are `INVALID_ADDRESS`.
///
/// This entry exists because of K5. The C++ runtime a real inference engine
/// links keeps its exception state and its stack guard in thread-local storage
/// addressed through FS, and a kernel that gives threads no FS base cannot run
/// it -- not slowly, not partly, not at all.
fn thread_pointer_set(domain: usize, thread: usize, frame: &mut TrapFrame) {
    let value = frame.rsi;
    if value != 0 && !(USER_MIN_ADDR..USER_MAX_ADDR).contains(&value) {
        frame.rax = k2status::INVALID_ADDRESS as u64;
        frame.rdx = 0;
        return;
    }
    let first = {
        let mut machine = MACHINE.lock();
        let first = machine.threads[thread].fs_base == 0 && value != 0;
        machine.threads[thread].fs_base = value;
        first
    };
    // The thread is running on this processor now, so the value takes effect on
    // the return to ring 3. If it was preempted between the store above and this
    // write, the dispatch that resumed it already wrote the stored value, and
    // writing it again here is the same value.
    //
    // SAFETY: the value is zero or canonical and in the user half, checked
    // above, so the write cannot fault; the kernel never uses FS.
    unsafe { cpu::wrmsr(cpu::MSR_FS_BASE, value) };
    if first {
        let name = crate::domain::domain_name(domain);
        trace!(
            "thread.pointer",
            "domain={domain} name={name} thread={thread} fs_base=0x{value:x}"
        );
    }
    frame.rax = k2status::OK as u64;
    frame.rdx = 0;
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
