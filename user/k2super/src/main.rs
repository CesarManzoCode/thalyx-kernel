//! K2 user domain `k2super`: the first supervisor.
//!
//! The kernel builds exactly one domain and hands it exactly four kinds of
//! capability: itself, the scope its children will be carved out of, the
//! control log, and one sealed image per remaining boot module. Everything else
//! that exists in a K2 run is built from here, through the interface, with no
//! syscall that opens a resource by name. That is the whole claim of
//! "creation without ambient privilege", and it is either true of this program
//! or it is not true at all.
//!
//! The script it runs is the first vertical:
//!
//! 1. Carve two child scopes out of its own — one for the server, one for the
//!    client — with limits it chooses and cannot exceed.
//! 2. Build the server, give it the endpoint's receive side, a way to append
//!    receipts, and a signal to speak on. Build the client, and give it one
//!    facet of the same endpoint and one buffer of its own. Nothing else.
//! 3. Wait until the server says it is holding admitted work.
//! 4. Fill the control log with ordinary receipts until it starts losing them,
//!    then read the client scope's drain report, fence that scope, and read the
//!    report again. Fencing is not draining: the counters after the barrier
//!    must still show the obligation that is genuinely outstanding. And the
//!    fence's own receipt must be written even though the log is full, because
//!    the cells that carry it were reserved for exactly this and ordinary
//!    traffic can never occupy them.
//! 5. Wait for the server to discharge it, then read a third report and retire
//!    the scope. Retirement must be refused while anything is still pending and
//!    permitted once nothing is, which is the difference between a barrier and
//!    a drain being real.
//!
//! The supervisor has no supervisor. Its fault ends the run.

#![no_std]
#![no_main]

use thalyx_abi::generated::{
    DrainReport, ScopeLimits, memory_state, receipt_kind, right, scope_state, status,
};
use thalyx_abi::{boot_handle, boot_slot};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, name16, report, slot};

/// Boot slots the kernel may have filled with sealed images.
const MAX_IMAGES: u32 = 8;

/// Queue cells of the work endpoint. Small on purpose: a bounded queue is the
/// mechanism, and a large one would only hide when it is reached.
const QUEUE_CAPACITY: u32 = 4;

/// Queue cells of the supervision endpoint. One reserved cell per supervised
/// domain, plus room for the ordinary traffic that never arrives on it.
const SUPERVISION_CAPACITY: u32 = 4;

/// Pages of the client's scratch buffer.
const CLIENT_BUFFER_PAGES: u64 = 1;

/// Handles the supervisor gives the server and the client. The slot numbers are
/// the shared convention in `rt::k2::slot`; these are the handles they resolve
/// to in a table whose slots have never been used before.
const SERVER_ENDPOINT: u32 = slot::SERVER_ENDPOINT;
const SERVER_LOG: u32 = slot::SERVER_LOG;
const SERVER_SIGNAL: u32 = slot::SERVER_SIGNAL;
const CLIENT_ENDPOINT: u32 = slot::CLIENT_ENDPOINT;
const CLIENT_BUFFER: u32 = slot::CLIENT_BUFFER;

/// Work between polls while waiting for a state only another domain can reach.
const POLL_WORK: u64 = 20_000;
/// Polls before the supervisor gives up on a state that should have arrived.
const POLL_LIMIT: u64 = 4096;

fn fail(step: u64, code: i64) -> ! {
    k2::note(report::BUILD_FAILED, step);
    k2::note(report::UNEXPECTED, code as u64);
    k2::exit(step)
}

/// Appends ordinary receipts until the log starts losing them.
///
/// A bounded ring loses the oldest records rather than growing, and says how
/// many it lost. That is the honest behaviour: a reader is told its coverage
/// has a hole instead of being left to assume completeness. What must survive
/// is the reserve, which this fills right up to but never into.
fn flood_log(log: u64) {
    let capacity = match k2::log_query(log) {
        Ok(info) => info.capacity,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    let mut appended = 0u32;
    while appended < capacity * 2 {
        if k2::log_append(
            log,
            receipt_kind::SERVICE_NOTE,
            0x5417_00FF,
            u64::from(appended),
            0,
        )
        .is_err()
        {
            break;
        }
        appended += 1;
    }
    match k2::log_query(log) {
        Ok(info) => {
            if info.lost > 0 && info.used <= info.capacity - info.reserved_cells {
                k2::note(report::LOG_LOSS, u64::from(info.lost));
            } else {
                k2::note(
                    report::UNEXPECTED,
                    (u64::from(info.used) << 32) | u64::from(info.lost),
                );
            }
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Fills the supervision endpoint's queue until admission refuses.
///
/// This is the endpoint both children report faults on, so it is the one where
/// a flood of ordinary traffic would be worth attempting: if it could consume
/// every cell, a domain could be made to die silently by keeping its
/// supervisor's queue busy. The cells reserved when each fault channel was
/// installed are what stops that, and the check afterwards is that they are
/// still free once ordinary admission has been refused.
fn flood_queue(endpoint: u64) {
    let mut admitted = 0u64;
    let bounded = loop {
        match k2::endpoint_send(endpoint, 0xF100_0000 + admitted, b"flood", &[], 0) {
            Ok(_) => {
                admitted += 1;
                if admitted > 64 {
                    k2::note(report::UNEXPECTED, admitted);
                    return;
                }
            }
            Err(status::QUEUE_FULL) => break true,
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return;
            }
        }
    };

    match k2::endpoint_query(endpoint) {
        Ok(info) => {
            let reserve_intact = info.queued + info.reserved_cells <= info.queue_capacity;
            if bounded && reserve_intact && admitted > 0 {
                k2::note(report::BOUNDED, admitted);
            } else {
                k2::note(
                    report::UNEXPECTED,
                    (u64::from(info.queued) << 32) | u64::from(info.reserved_cells),
                );
            }
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// With no receipt cell left to reserve, a covered admission must be refused.
///
/// The audited profile buys its coverage in advance: an operation that would
/// have to be recorded is refused before it has an effect rather than happening
/// unrecorded. That is the trade the profile makes, and it is only real if it
/// can be observed happening.
fn audited_admission_refused(endpoint: u64) {
    k2::expect_refusal(
        k2::endpoint_send(endpoint, 0xF200_0000, b"unrecordable", &[], 0),
        status::LIMIT_EXHAUSTED,
    );
}

/// Packs a drain report into one value a diagnostic record can carry.
fn packed(report: &DrainReport) -> u64 {
    u64::from(report.state) << 56
        | u64::from(report.threads_running & 0xFF) << 48
        | u64::from(report.invocations_pending & 0xFF) << 40
        | u64::from(report.effects_pending & 0xFF) << 32
        | u64::from(report.maps_pending & 0xFF) << 24
        | u64::from(report.undelivered_cancelled & 0xFF) << 16
        | u64::from(report.external_unknown & 0xFF) << 8
}

/// Finds the boot slot whose sealed image carries `label`.
///
/// The supervisor is not told which module is which. It asks each image object
/// what it is, which is the only honest way for it to find out: the label is a
/// property of the object the kernel sealed, not of a slot number this program
/// could have assumed.
fn image_named(label: &str) -> Option<u64> {
    let wanted = name16(label);
    for offset in 0..MAX_IMAGES {
        let handle = boot_handle(boot_slot::FIRST_MODULE + offset);
        let Ok(info) = k2::memory_query(handle) else {
            continue;
        };
        if info.state == memory_state::SEALED && info.label == wanted {
            return Some(handle);
        }
    }
    None
}

fn child_limits(cpu_budget_ns: u64, closure_reserve_ns: u64, memory_pages: u64) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 24,
        cpu_budget_ns,
        queue_bytes: 4096,
        closure_reserve_ns,
        parallelism: 2,
        reserved0: 0,
    }
}

fn run() -> ! {
    let own_scope = boot_handle(boot_slot::SELF_SCOPE);
    let log = boot_handle(boot_slot::CONTROL_LOG);

    let limits = match k2::limits() {
        Ok(limits) => limits,
        Err(code) => fail(1, code),
    };
    k2::note(
        report::LIMITS,
        (u64::from(limits.major) << 48) | (u64::from(limits.minor) << 32) | limits.page_size,
    );

    // The kernel stamps the real origin over whatever a caller claims. Claiming
    // a false one here is how that gets observed rather than assumed.
    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5417_0001, 0, 0xFACE_F00D);

    // --- scopes ------------------------------------------------------------
    // The server gets closure reserve, because it is the domain that will owe a
    // discharge. The client gets a deliberately small CPU budget: the run has
    // to be able to show a budget being consumed rather than assumed infinite.
    let server_scope = match k2::scope_create_child(
        own_scope,
        child_limits(limits.cpu_window_ns / 4, 1_000_000, 96),
        name16("srv"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(2, code),
    };
    let client_scope = match k2::scope_create_child(
        own_scope,
        child_limits(limits.cpu_window_ns / 8, 0, 96),
        name16("cli"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(3, code),
    };
    k2::note(report::BUILT, 1);

    // A child may not exceed its parent. Asking for more than the supervisor
    // itself holds has to be refused, or the tree bounds nothing.
    k2::expect_refusal(
        k2::scope_create_child(
            client_scope,
            child_limits(limits.cpu_window_ns * 16, 0, u64::MAX / 2),
            name16("toobig"),
        ),
        status::LIMIT_EXHAUSTED,
    );

    // --- objects -----------------------------------------------------------
    // The supervision endpoint is the supervisor's own, charged to its own
    // scope: a channel a supervisor hears its children die on must not be paid
    // for out of the budget of the domain that is dying.
    //
    // Receiving on it once, without blocking, is what claims the supervisor as
    // its receiver. There is nothing queued yet -- `WOULD_BLOCK` is the
    // expected answer -- but the endpoint has to have somewhere to deliver to
    // before a fault channel may point at it.
    let supervision =
        match k2::scope_create_endpoint(own_scope, SUPERVISION_CAPACITY, name16("faults")) {
            Ok(handle) => handle,
            Err(code) => fail(4, code),
        };
    k2::expect_refusal(
        k2::endpoint_receive(supervision, 0, true).map(|_| 0),
        status::WOULD_BLOCK,
    );

    let endpoint = match k2::scope_create_endpoint(server_scope, QUEUE_CAPACITY, name16("work")) {
        Ok(handle) => handle,
        Err(code) => fail(5, code),
    };
    let signal = match k2::scope_create_signal(server_scope) {
        Ok(handle) => handle,
        Err(code) => fail(6, code),
    };
    let client_buffer = match k2::scope_create_memory(
        client_scope,
        CLIENT_BUFFER_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("clibuf"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(7, code),
    };
    k2::note(report::BUILT, 2);

    // --- server ------------------------------------------------------------
    let server_image = match image_named("k2server") {
        Some(handle) => handle,
        None => fail(8, status::INVALID_HANDLE),
    };
    let server = match k2::scope_create_domain(server_scope, server_image, name16("k2server")) {
        Ok(handle) => handle,
        Err(code) => fail(9, code),
    };
    if let Err(code) = k2::domain_install_cap(
        server,
        endpoint,
        SERVER_ENDPOINT,
        right::INSPECT | right::ENDPOINT_RECEIVE,
        0,
    ) {
        fail(10, code);
    }
    if let Err(code) = k2::domain_install_cap(
        server,
        log,
        SERVER_LOG,
        right::INSPECT | right::LOG_APPEND,
        0,
    ) {
        fail(11, code);
    }
    if let Err(code) = k2::domain_install_cap(
        server,
        signal,
        SERVER_SIGNAL,
        right::INSPECT | right::SIGNAL_RAISE,
        0,
    ) {
        fail(12, code);
    }
    // A managed domain may not be activated until its faults have somewhere to
    // go. Trying it first is how that gate is observed holding rather than
    // assumed.
    k2::expect_refusal(k2::domain_activate(server), status::STATE_CONFLICT);
    if let Err(code) = k2::domain_set_fault_channel(server, supervision) {
        fail(13, code);
    }
    if let Err(code) = k2::domain_activate(server) {
        fail(14, code);
    }
    k2::note(report::BUILT, 3);

    // --- client ------------------------------------------------------------
    // The client receives a facet, not the endpoint. A facet is what lets the
    // server distinguish who is calling without trusting the payload, and what
    // lets this authority be withdrawn on its own.
    let facet = match k2::endpoint_bind_facet(
        endpoint,
        0,
        right::INSPECT | right::TRANSFER | right::ENDPOINT_CALL,
        0,
    ) {
        Ok(handle) => handle,
        Err(code) => fail(15, code),
    };

    let client_image = match image_named("k2client") {
        Some(handle) => handle,
        None => fail(16, status::INVALID_HANDLE),
    };
    let client = match k2::scope_create_domain(client_scope, client_image, name16("k2client")) {
        Ok(handle) => handle,
        Err(code) => fail(17, code),
    };

    // Installing more rights than the source grant carries would be
    // amplification by delegation. It is refused here for the same reason the
    // client's own derivation is refused when it tries to widen.
    k2::expect_refusal(
        k2::domain_install_cap(
            client,
            facet,
            CLIENT_ENDPOINT,
            right::ENDPOINT_CALL | right::ENDPOINT_RECEIVE,
            0,
        ),
        status::INSUFFICIENT_RIGHTS,
    );

    if let Err(code) = k2::domain_install_cap(
        client,
        facet,
        CLIENT_ENDPOINT,
        right::INSPECT | right::TRANSFER | right::ENDPOINT_CALL,
        0,
    ) {
        fail(18, code);
    }
    if let Err(code) = k2::domain_install_cap(
        client,
        client_buffer,
        CLIENT_BUFFER,
        right::INSPECT | right::DERIVE | right::TRANSFER | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    ) {
        fail(19, code);
    }
    if let Err(code) = k2::domain_set_fault_channel(client, supervision) {
        fail(20, code);
    }
    if let Err(code) = k2::domain_activate(client) {
        fail(21, code);
    }
    k2::note(report::BUILT, 4);

    // --- the close ---------------------------------------------------------
    if let Err(code) = k2::signal_wait(signal, slot::HOLDING_WORK, 0) {
        fail(22, code);
    }
    k2::note(report::HOLDING, 1);

    // Order matters, and it is the kernel's order, not a convenience. The
    // queue is filled first, because once the control log has no reservable
    // cell left every covered admission is refused before it has an effect --
    // which is the audited profile working, and would make a queue-full result
    // impossible to reach.
    flood_queue(supervision);
    flood_log(log);
    // On an endpoint with room, so the refusal that is observed is the one the
    // exhausted receipt log causes and not the queue's own bound.
    audited_admission_refused(endpoint);

    // Before the barrier: the client scope is open and owes one invocation.
    let before = match k2::scope_drain_status(client_scope) {
        Ok(report) => report,
        Err(code) => fail(23, code),
    };
    k2::note(report::DRAIN, packed(&before));

    // Retiring an open scope is not a way to skip the barrier. It is refused
    // as a state conflict rather than an incomplete drain: nothing has been
    // asked to stop yet, so there is no drain to be incomplete.
    k2::expect_refusal(k2::scope_retire(client_scope).0, status::STATE_CONFLICT);

    let before_fence = match k2::log_query(log) {
        Ok(info) => info,
        Err(code) => fail(24, code),
    };

    if let Err(code) = k2::scope_fence(client_scope) {
        fail(25, code);
    }

    // The log was full of ordinary receipts when the barrier was placed. The
    // fence's receipt is a closing one, so it goes in a cell ordinary traffic
    // could never have taken. If this did not advance, a revocation could be
    // silenced by flooding the log, which is the failure the reserved cells
    // exist to prevent.
    match k2::log_query(log) {
        Ok(after) => {
            if after.next_sequence > before_fence.next_sequence {
                k2::note(report::CLOSURE_RECORDED, after.next_sequence);
            } else {
                k2::note(report::UNEXPECTED, after.next_sequence);
            }
        }
        Err(code) => fail(26, code),
    }

    // After the barrier and before the drain: the scope is closed to new
    // admissions, and the obligation the server holds is still counted. A
    // report that went to zero here would be the failure this step exists to
    // catch.
    let fenced = match k2::scope_drain_status(client_scope) {
        Ok(report) => report,
        Err(code) => fail(30, code),
    };
    k2::note(report::DRAIN, packed(&fenced));
    if fenced.state != scope_state::FENCED {
        k2::note(report::UNEXPECTED, u64::from(fenced.state));
    }

    // Asked for straight after the barrier, while the server has not run yet
    // and the effect it admitted is certainly still outstanding. The answer and
    // the report come from one hold of the lock, so they cannot disagree about
    // a perimeter that changed in between.
    let (early, early_report) = k2::scope_retire(client_scope);
    let early_outstanding = early_report.threads_running
        + early_report.invocations_pending
        + early_report.effects_pending
        + early_report.maps_pending;
    match early {
        Err(status::DRAIN_INCOMPLETE) if early_outstanding != 0 => {
            k2::note(report::REFUSED_AS_EXPECTED, status::DRAIN_INCOMPLETE as u64);
            k2::note(report::DRAIN, packed(&early_report));
        }
        Err(status::DRAIN_INCOMPLETE) => k2::note(report::UNEXPECTED, packed(&early_report)),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
        Ok(_) => k2::note(report::NOT_REFUSED, status::DRAIN_INCOMPLETE as u64),
    }

    // --- the drain ---------------------------------------------------------
    if let Err(code) = k2::signal_wait(signal, slot::WORK_RESOLVED, 0) {
        fail(28, code);
    }

    // Retirement is asked for repeatedly rather than only once the drain looks
    // complete, and each answer is checked against the report that came with
    // it. Reading the report and then retiring would be two operations with a
    // gap between them, and the interesting claim -- retirement is refused
    // exactly while something is outstanding -- would be a claim about that
    // gap. The kernel writes the report and decides under one lock, so asking
    // this way makes the two agree by construction or not at all.
    let mut spins = 0u64;
    let final_report = loop {
        let (outcome, report) = k2::scope_retire(client_scope);
        let outstanding = report.threads_running
            + report.invocations_pending
            + report.effects_pending
            + report.maps_pending;
        match outcome {
            Ok(_) => {
                if outstanding != 0 {
                    k2::note(report::UNEXPECTED, packed(&report));
                }
                break report;
            }
            Err(status::DRAIN_INCOMPLETE) => {
                if outstanding == 0 {
                    k2::note(report::UNEXPECTED, packed(&report));
                }
                k2::note(report::DRAIN, packed(&report));
            }
            Err(code) => fail(29, code),
        }
        let _ = rt::burn(POLL_WORK, spins | 1);
        spins += 1;
        if spins > POLL_LIMIT {
            fail(30, status::DRAIN_INCOMPLETE);
        }
    };
    k2::note(report::DRAIN, packed(&final_report));
    k2::note(report::BUILT, 5);

    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5417_0002, spins, 0);
    k2::note(report::DONE, 5);
    k2::exit(0)
}

rt::entry!(run);
