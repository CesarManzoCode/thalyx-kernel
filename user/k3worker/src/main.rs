//! K3 user domain `k3worker`.
//!
//! One image, four roles, and the role is not the program's choice: it reads it
//! from a page the supervisor filled in and mapped read-only before this domain
//! existed. A worker cannot decide to be a different worker.
//!
//! Each role exists to put a specific race under the kernel while it is
//! actually running on another processor:
//!
//! * `SPIN` burns processor time against a budget shared with its siblings. On
//!   one processor they take turns and the total is bounded by arithmetic; on
//!   four they are eligible at once, and the total is bounded only if the
//!   kernel reserves the balance before dispatching rather than checking it.
//! * `PROBE` reads a page in a loop. Its domain has two threads, so the address
//!   space can be live on two processors at once, and the supervisor takes the
//!   mapping away while it is. The fault that follows is the observable half:
//!   the translation was retired on a processor that was using it, not merely
//!   removed from a table.
//! * `WRITER` writes a page in a loop, and the supervisor seals the object
//!   underneath it. The seal has to withdraw the writer and confirm the
//!   withdrawal before it promises anything.
//! * `CALLER` calls an endpoint in a loop while its own scope is fenced from
//!   another processor. Every call is either admitted before the barrier or
//!   refused; there is no third outcome, and the run says which each one was.
//!
//! Three of the four are expected to die of a fault or a closure. That is the
//! point: a domain that survives every test in this file has not demonstrated
//! anything about the boundary it was supposed to be standing on.

#![no_std]
#![no_main]

use thalyx_abi::boot_handle;
use thalyx_abi::generated::status;
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report};
use thalyx_user_rt::k3::{self, WorkerConfig, bit, role, worker_slot};

/// Base of this program's FP pattern; the domain identifier is mixed in at run
/// time so two domains from the same image differ.
const FP_BASE: u64 = 0x5A5A_A5A5_1234_3001;

/// Reads this worker's configuration from the page mapped for it.
fn config() -> WorkerConfig {
    // SAFETY: the supervisor mapped a page of its own at this address before
    // activating this domain, read-only, containing exactly this structure. A
    // domain that was not given the mapping faults here rather than reading
    // something else, which is the boundary working.
    unsafe { core::ptr::read_volatile(k3::CONFIG_VADDR as *const WorkerConfig) }
}

/// Publishes this worker's own count into the page every worker shares.
///
/// Each worker writes one word of its own, so the page needs no atomics to be
/// meaningful: the supervisor reads them all and adds them up, and no two
/// workers ever write the same address.
fn publish(slot: u64, value: u64) {
    let address = k3::SHARED_VADDR + slot * 8;
    // SAFETY: the supervisor mapped a shared page writable at this address and
    // gave this worker one word of it, chosen by the configuration it also
    // wrote. The write is naturally aligned and inside the page.
    unsafe { core::ptr::write_volatile(address as *mut u64, value) }
}

/// Burns processor time under a budget shared with the other spinners.
fn spin(config: WorkerConfig, pattern: u64) -> ! {
    let mut acc = pattern;
    let mut round = 0u64;
    while round < config.rounds {
        acc = rt::burn(config.burn, acc);
        publish(config.slot, round + 1);
        // One kernel entry per interval, so the run has a record per interval
        // and the intervals are attributable to a processor.
        k2::note(report::WORK_INTERVAL, round + 1);
        round += 1;
    }
    rt::self_check(rt::check::WORK_OBSERVED, acc != 0);
    rt::exit(0)
}

/// Reads a page until the mapping is taken away.
fn probe(config: WorkerConfig) -> ! {
    let mut round = 0u64;
    loop {
        // SAFETY: the supervisor mapped a page read-only at this address. When
        // it takes the mapping away this read faults, which is what this domain
        // is for; nothing after the fault runs.
        let word = unsafe { core::ptr::read_volatile(k3::PROBE_VADDR as *const u64) };
        if round % 64 == 0 {
            k2::note(report::PAGE_READ, word);
        }
        publish(config.slot, round);
        round += 1;
        if round > 1_000_000 {
            // The mapping was never withdrawn. Ending here rather than looping
            // for ever makes that a reported outcome instead of a hang.
            k2::note(report::UNEXPECTED, round);
            rt::exit(1)
        }
    }
}

/// Writes a page until the object underneath it is sealed.
fn writer(config: WorkerConfig) -> ! {
    let mut round = 0u64;
    loop {
        // SAFETY: the supervisor mapped a page writable at this address. The
        // seal withdraws that mapping, and the next store faults.
        unsafe { core::ptr::write_volatile(k3::PROBE_VADDR as *mut u64, round) };
        if round % 64 == 0 {
            k2::note(report::WORK_INTERVAL, round);
        }
        publish(config.slot, round);
        round += 1;
        if round > 1_000_000 {
            k2::note(report::UNEXPECTED, round);
            rt::exit(1)
        }
    }
}

/// Calls an endpoint until the barrier stops it.
fn caller(config: WorkerConfig) -> ! {
    let endpoint = boot_handle(worker_slot::ENDPOINT);
    let mut admitted = 0u64;
    let mut round = 0u64;
    while round < config.rounds {
        match k2::endpoint_call(endpoint, round, b"k3", &[], 0, false) {
            Ok(_) => {
                admitted += 1;
                publish(config.slot, admitted);
            }
            Err(code) => {
                // A closed origin is the expected end. Anything else is
                // reported as itself rather than folded into the same outcome.
                k2::note(report::REFUSED_AFTER_FENCE, code as u64);
                if code == status::SCOPE_CLOSED || code == status::CANCELLED {
                    break;
                }
                if code != status::TIMED_OUT && code != status::WOULD_BLOCK {
                    k2::note(report::UNEXPECTED, code as u64);
                    break;
                }
            }
        }
        round += 1;
    }
    k2::note(report::ADMITTED_BEFORE_FENCE, admitted);
    k2::note(report::DONE, round);
    rt::exit(0)
}

fn run() -> ! {
    let pattern = rt::establish(FP_BASE);
    let config = config();
    if let Ok(limits) = k2::limits() {
        k2::note(report::CPUS_ONLINE, u64::from(limits.cpus_online));
    }
    let stop = boot_handle(worker_slot::STOP);
    let _ = stop;
    let _ = bit::STOP;
    match config.role {
        role::SPIN => spin(config, pattern),
        role::PROBE => probe(config),
        role::WRITER => writer(config),
        role::CALLER => caller(config),
        other => {
            k2::note(report::UNEXPECTED, other);
            rt::exit(2)
        }
    }
}

rt::entry!(run);
