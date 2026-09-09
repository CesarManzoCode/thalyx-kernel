//! K2 user domain `k2server`.
//!
//! The server is the domain that holds work. It receives one request, admits an
//! effect against it, and then deliberately keeps that obligation open while
//! the origin of the request is closed underneath it. That is the whole point:
//! a barrier must not be able to make a retained obligation disappear, and the
//! server must be able to finish it afterwards.
//!
//! It also tests the transferred capability in both directions. The client sent
//! a read-only capability over its own buffer. Reading through it must work;
//! writing through it must not. A server that could widen a capability it was
//! handed would make derivation decorative.
//!
//! After the fence the same capability must stop working entirely, because its
//! lineage runs through the grant that was just closed. The obligation ticket
//! must keep working, because it does not: it is sponsored by this domain's own
//! scope. Those two facts together are what "the barrier reaches derived
//! authority, not retained obligations" means in practice.
//!
//! It also binds itself to the ticket. A server that holds work does not do it
//! for free: binding makes the time attributable, because the worker adopts the
//! effective scope the kernel stamped on the invocation and competes against
//! that scope's budget rather than adding a second one. After the barrier the
//! same binding becomes a recovery binding, charged to the service's own
//! closure reserve -- which is the accounting exception the authority contract
//! grants and the only way an obligation to a closed client can be finished.
//!
//! Last, with its obligations discharged, it fills its own capability table
//! until the kernel refuses. A domain that can accumulate handles for free can
//! make the kernel's tables grow without ever exceeding a limit it was given,
//! so the interesting result is the refusal, not the successes before it.

#![no_std]
#![no_main]

use thalyx_abi::generated::{outcome, receipt_kind, right, status};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report, slot};

/// Nanoseconds the server reserves in its own scope to close the effect it
/// admits. Reserved before the effect is permitted, so finishing never depends
/// on the budget of the client being closed.
const CLOSURE_RESERVE_NS: u64 = 200_000;

/// Effect classification the server announces. Opaque to the kernel, which
/// records it rather than interpreting it.
const EFFECT_KIND: u32 = 1;

/// Detail recorded with the resolution, so the receipt says why it aborted.
const ABORT_DETAIL: u64 = 0xAB07_0001;

/// Work between checks of the invocation's cancellation state. Small, because
/// the state the server is waiting for is one another domain sets and then
/// stops existing: the origin is fenced first and dies shortly after, and a
/// server that only looked occasionally would see the second fact and miss the
/// first. The spin is what lets the supervisor run and fence in between.
const POLL_WORK: u64 = 200;

fn run() -> ! {
    let endpoint = thalyx_abi::boot_handle(slot::SERVER_ENDPOINT);
    let log = thalyx_abi::boot_handle(slot::SERVER_LOG);
    let signal = thalyx_abi::boot_handle(slot::SERVER_SIGNAL);

    // A claimed origin the kernel is expected to overwrite with the real one.
    // If it ever appears in a receipt as written here, origin is forgeable.
    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5E70_0001, 0, 0xDEAD_BEEF);

    let (message, invocation) = match k2::endpoint_receive(endpoint, 0, false) {
        Ok(pair) => pair,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            k2::exit(1);
        }
    };
    k2::note(report::ORIGIN, message.header.sender_domain_id);
    k2::note(report::BUILT, u64::from(message.cap_count));

    // The capability the client narrowed on its way in. Reading is what it
    // permits; writing is what it does not, and the kernel is what says so.
    if message.cap_count == 1 {
        let transferred = message.caps[0];
        let mut buffer = [0u8; 32];
        match k2::memory_read(transferred, 0, &mut buffer) {
            Ok(_) => k2::note(report::BUILT, 2),
            Err(code) => k2::note(report::UNEXPECTED, code as u64),
        }
        k2::expect_refusal(
            k2::memory_write(transferred, 0, b"server-was-here"),
            status::INSUFFICIENT_RIGHTS,
        );
        // Widening a received capability past what it carries is the amplifying
        // move the whole derivation rule exists to refuse.
        k2::expect_refusal(
            k2::derive(transferred, right::MEMORY_READ | right::MEMORY_WRITE, 0, 0),
            status::INSUFFICIENT_RIGHTS,
        );
    } else {
        k2::note(report::UNEXPECTED, u64::from(message.cap_count));
    }

    // Charge this thread's execution to whoever the work came from, before
    // announcing the effect. The client pays for the service it asked for.
    match k2::invocation_bind_worker(invocation) {
        Ok(_) => k2::note(report::BOUND, message.header.sender_scope_id),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            k2::exit(7);
        }
    }

    // Announce the effect and hold it. From here until the resolution below,
    // this domain owes the system a discharge that no barrier may cancel.
    if let Err(code) = k2::invocation_begin_effect(invocation, EFFECT_KIND, CLOSURE_RESERVE_NS) {
        k2::note(report::UNEXPECTED, code as u64);
        k2::exit(2);
    }
    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5E70_0002, invocation, 0);

    // Tell the supervisor the work is held, then wait to be closed.
    if let Err(code) = k2::signal_raise(signal, slot::HOLDING_WORK) {
        k2::note(report::UNEXPECTED, code as u64);
        k2::exit(3);
    }

    let mut spins = 0u64;
    let observed;
    loop {
        match k2::invocation_query(invocation) {
            Ok(info) => {
                if info.cancel_state != thalyx_abi::generated::cancel_state::LIVE {
                    observed = info.cancel_state;
                    break;
                }
            }
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                k2::exit(4);
            }
        }
        let _ = rt::burn(POLL_WORK, spins | 1);
        spins += 1;
        if spins > 4096 {
            k2::note(report::UNEXPECTED, u64::MAX);
            k2::exit(5);
        }
    }
    k2::note(report::CANCEL_STATE, u64::from(observed));

    // The origin is fenced. Authority the client derived must be dead with it,
    // even though this domain still physically holds the handle.
    if message.cap_count == 1 {
        let mut buffer = [0u8; 32];
        k2::expect_refusal(
            k2::memory_read(message.caps[0], 0, &mut buffer),
            status::SCOPE_CLOSED,
        );
    }

    // The origin is closed, so the work left to do is recovery. Rebinding says
    // so explicitly: the time from here is charged to this service's closure
    // reserve rather than to the budget of a client that no longer has one.
    if let Err(code) = k2::invocation_unbind_worker(invocation) {
        k2::note(report::UNEXPECTED, code as u64);
    }
    match k2::invocation_bind_worker(invocation) {
        Ok(_) => k2::note(report::BOUND, 0),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    // The obligation, however, is this domain's own, sponsored by this domain's
    // scope. It survives the barrier and can still be discharged.
    match k2::invocation_resolve(invocation, outcome::ABORTED, ABORT_DETAIL) {
        Ok(_) => k2::note(report::BUILT, 3),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            k2::exit(6);
        }
    }
    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5E70_0003, invocation, 0);

    bounded_table(log);

    let _ = k2::signal_raise(signal, slot::WORK_RESOLVED);

    k2::note(report::DONE, 3);
    k2::exit(0)
}

/// Fills the domain's capability table until the kernel refuses to grow it.
///
/// The table is a fixed capacity charged as metadata to the domain's scope, so
/// there are two ways this can end and both are correct: the slots run out, or
/// the scope's metadata budget does. What must not happen is that it never
/// ends.
fn bounded_table(log: u64) {
    let mut admitted = 0u64;
    let mut last = log;
    loop {
        match k2::cap_copy(last) {
            Ok(handle) => {
                admitted += 1;
                last = handle;
                if admitted > 1024 {
                    k2::note(report::UNEXPECTED, admitted);
                    return;
                }
            }
            Err(code) => {
                if code == status::LIMIT_EXHAUSTED {
                    k2::note(report::BOUNDED, admitted);
                } else {
                    k2::note(report::UNEXPECTED, code as u64);
                }
                return;
            }
        }
    }
}

rt::entry!(run);
