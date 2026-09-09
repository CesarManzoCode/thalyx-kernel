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
//! 5. Wait for the server to discharge it, then keep asking for the scope to be
//!    retired. Retirement must be refused while anything is still pending and
//!    permitted once nothing is, which is the difference between a barrier and
//!    a drain being real.
//! 6. Arm a timer against the monotonic clock and wait for it to raise its bits
//!    on a signal. Every deadline in this interface is an absolute monotonic
//!    reading, so a program that could not read the clock could not state one,
//!    and none of them would be usable.
//! 7. Publish a sealed page: fill a scratch object, copy it into a second one,
//!    map that one writable into a domain being built, then seal it. Sealing
//!    must withdraw the writable mapping before it can promise anything, and a
//!    writable mapping of the sealed result must afterwards be refused. This is
//!    the conservative route the memory contract asks for -- copy, withdraw the
//!    writer, publish the seal -- rather than handing out a page and calling it
//!    immutable.
//! 8. Read its own scope and a child's, and check that what the child spent is
//!    also counted against the parent. A budget that only bound the leaf would
//!    be no budget at all: a domain could carve children until the sum of their
//!    allowances exceeded anything it was given.
//!
//! The supervisor has no supervisor. Its fault ends the run.

#![no_std]
#![no_main]

use thalyx_abi::generated::{
    DrainReport, ScopeLimits, cap_lineage, domain_state, memory_state, receipt_kind, right,
    scope_state, status,
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

/// The published page: how big, where it lands, and what it says.
const PUBLISHED_PAGES: u64 = 1;
const PUBLISHED_VADDR: u64 = 0x0000_0000_5000_0000;
const PUBLISHED_BYTES: &[u8] = b"k2-published-immutable";

/// The spare domain's one page of its own, and where it lands in that domain.
/// A thread needs an entry the domain has mapped and a stack it could push on,
/// and this is the cheapest way to give it both without assuming an address out
/// of an image this program never parsed.
const SPARE_PAGES: u64 = 1;
const SPARE_VADDR: u64 = 0x0000_0000_6000_0000;

/// The `srv` scope's page ceiling at creation, and the one it is narrowed to.
/// Creating it wider than it ends up is what makes the narrowing a narrowing
/// rather than a restatement of what it already had; the narrowed value is the
/// one the rest of the run is built inside.
const SERVER_PAGES_AT_CREATION: u64 = 128;
const SERVER_PAGES_NARROWED: u64 = 96;

/// The origin the supervisor's first receipt claims, and the value that marks
/// which receipt that is. The claim is false on purpose: the kernel stamps the
/// real origin over it, and a reader finding this value in an origin field
/// would mean the payload had chosen who acted.
const FORGED_ORIGIN: u64 = 0xFACE_F00D;
const FORGED_NOTE: u64 = 0x5417_0001;

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
/// Signal bit the timer raises. Distinct from the bits the server speaks on, so
/// waiting for one cannot be satisfied by the other.
const TIMER_BIT: u64 = 1 << 8;
/// How far ahead the timer is armed. Longer than a scheduling quantum, so the
/// wait is a real wait rather than a deadline that had already passed.
const TIMER_DELAY_NS: u64 = 5_000_000;
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
    // a false one here is how that gets observed rather than assumed, and
    // `receipts` later goes looking for this exact record to see what the
    // origin field ended up saying.
    let _ = k2::log_append(
        log,
        receipt_kind::SERVICE_NOTE,
        FORGED_NOTE,
        0,
        FORGED_ORIGIN,
    );

    // --- scopes ------------------------------------------------------------
    // The server gets closure reserve, because it is the domain that will owe a
    // discharge. The client gets a deliberately small CPU budget: the run has
    // to be able to show a budget being consumed rather than assumed infinite.
    let server_scope = match k2::scope_create_child(
        own_scope,
        child_limits(
            limits.cpu_window_ns / 4,
            1_000_000,
            SERVER_PAGES_AT_CREATION,
        ),
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

    // Narrowed before anything is charged to it, so the server is built inside
    // the ceiling the supervisor settled on rather than the one it asked for
    // first.
    ceilings(
        own_scope,
        server_scope,
        child_limits(limits.cpu_window_ns / 4, 1_000_000, SERVER_PAGES_NARROWED),
    );

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
    // The supervisor keeps more on its own handle than it gives away: DERIVE,
    // because the authority it hands out is minted by narrowing this one, and
    // ADMIN, because withdrawing a delegation is the supervisor's job and a
    // holder that could fence its own grant could not be fenced by anyone else.
    // The client's copy below carries neither.
    let facet = match k2::endpoint_bind_facet(
        endpoint,
        0,
        right::INSPECT | right::DERIVE | right::TRANSFER | right::ADMIN | right::ENDPOINT_CALL,
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
    publish(client_scope, client, client_buffer);

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

    expiry(own_scope, signal);
    budget(own_scope, server_scope);
    receipts(log);
    grant_barrier(facet);
    spare_domain(own_scope, client_image, limits.page_size);

    let _ = k2::log_append(log, receipt_kind::SERVICE_NOTE, 0x5417_0002, spins, 0);
    k2::note(report::DONE, 5);
    k2::exit(0)
}

/// Builds a sealed page and hands it to a domain being built.
///
/// The order is the memory contract's and each step is refusable. Writing needs
/// a mutable object; copying needs authority over both ends; mapping writable
/// needs the object to still be mutable; sealing needs every writable mapping
/// gone, which is why it withdraws them rather than asking the holder to. Only
/// then is the object published, and only then is a read-only mapping of it
/// something a reader can rely on.
fn publish(scope: u64, target: u64, source: u64) {
    // The ceiling includes execute and seal on purpose. Execute so that the
    // write-and-execute mapping below is refused by the W^X rule rather than by
    // the ceiling -- a control that fires for the wrong reason tests nothing --
    // and seal because publishing is what this object is for.
    let published = match k2::scope_create_memory(
        scope,
        PUBLISHED_PAGES,
        right::MEMORY_READ
            | right::MEMORY_WRITE
            | right::MEMORY_EXECUTE
            | right::MEMORY_MAP
            | right::MEMORY_SEAL,
        name16("published"),
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };

    if let Err(code) = k2::memory_write(source, 0, PUBLISHED_BYTES) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }
    match k2::memory_copy(published, source, 0, 0, PUBLISHED_BYTES.len() as u64) {
        Ok(_) => k2::note(report::COPIED_BYTES, PUBLISHED_BYTES.len() as u64),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Writable and executable at once is not a mapping this kernel will make,
    // whatever the object's ceiling says, and a mapping with neither read nor
    // anything else is not a mapping at all.
    k2::expect_refusal(
        k2::domain_map(
            target,
            published,
            PUBLISHED_VADDR,
            0,
            PUBLISHED_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_EXECUTE,
        ),
        status::INVALID_ARGUMENT,
    );

    // A writable mapping, which sealing will have to take away again.
    match k2::domain_map(
        target,
        published,
        PUBLISHED_VADDR,
        0,
        PUBLISHED_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    ) {
        Ok(_) => k2::note(report::MAPPED, PUBLISHED_VADDR),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Taking the mapping away by hand, and putting it back, before the seal
    // does the same thing on its own account. The two paths have to agree that
    // a mapping is a record the kernel keeps rather than a page table entry it
    // forgot about.
    match k2::domain_unmap(target, PUBLISHED_VADDR, PUBLISHED_PAGES as u32) {
        Ok(_) => {}
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }
    k2::expect_refusal(
        k2::domain_unmap(target, PUBLISHED_VADDR, PUBLISHED_PAGES as u32),
        status::INVALID_ARGUMENT,
    );
    if let Err(code) = k2::domain_map(
        target,
        published,
        PUBLISHED_VADDR,
        0,
        PUBLISHED_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    ) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }

    match k2::memory_seal(published) {
        Ok(info) if info.state == memory_state::SEALED && info.writable_maps == 0 => {
            k2::note(report::SEALED, info.pages);
        }
        Ok(info) => {
            k2::note(report::UNEXPECTED, u64::from(info.writable_maps));
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Sealed means sealed: a writable mapping of it is refused from here on.
    k2::expect_refusal(
        k2::domain_map(
            target,
            published,
            PUBLISHED_VADDR,
            0,
            PUBLISHED_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        ),
        status::STATE_CONFLICT,
    );

    // The reader gets the sealed bytes, read-only, at a fixed address.
    if let Err(code) = k2::domain_map(
        target,
        published,
        PUBLISHED_VADDR,
        0,
        PUBLISHED_PAGES as u32,
        right::MEMORY_READ,
    ) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }
    if let Err(code) = k2::domain_install_cap(
        target,
        published,
        slot::CLIENT_PUBLISHED,
        right::INSPECT | right::MEMORY_READ,
        0,
    ) {
        k2::note(report::UNEXPECTED, code as u64);
    }
}

/// Checks that a child's spending is also charged to its ancestors.
///
/// The tree is the mechanism: a scope's budget bounds everything beneath it,
/// not just the threads it owns directly. If a child could spend without its
/// parent's totals moving, a domain would only have to create children to
/// escape whatever it was given.
fn budget(parent: u64, child: u64) {
    let (above, below) = match (k2::scope_query(parent), k2::scope_query(child)) {
        (Ok(above), Ok(below)) => (above, below),
        (Err(code), _) | (_, Err(code)) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    if below.cpu_total_ns == 0 {
        k2::note(report::UNEXPECTED, 0);
        return;
    }
    if above.cpu_total_ns < below.cpu_total_ns {
        k2::note(report::UNEXPECTED, above.cpu_total_ns);
        return;
    }
    if above.depth >= below.depth {
        k2::note(report::UNEXPECTED, u64::from(above.depth));
        return;
    }
    k2::note(
        report::BUDGET_AGGREGATED,
        above.cpu_total_ns - below.cpu_total_ns,
    );

    // Debt is what a window that closed over budget carried forward. Reporting
    // it whether or not it happened is the point: a limit whose overrun nobody
    // can read is not being enforced, it is being hoped for.
    k2::note(report::DEBT_CARRIED, above.cpu_debt_ns);
}

/// Changes a scope's ceilings after the fact, and shows both ways it is bounded.
///
/// A ceiling is only a ceiling if it can be brought down later: a supervisor
/// that could only set limits at creation would have to predict everything a
/// child will ever need and grant that much up front. Bringing one down is
/// bounded in both directions, and by two different rules that are easy to
/// mistake for one. Upwards, nothing may exceed what the parent holds, or the
/// tree would stop being a bound on the subtree. Downwards, nothing may fall
/// below what the subtree has already been charged for, or an accounted charge
/// would silently become a debt nobody agreed to.
fn ceilings(parent: u64, child: u64, narrowed: ScopeLimits) {
    if let Err(code) = k2::scope_set_limits(child, narrowed) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }
    match k2::scope_query(child) {
        Ok(info) if info.limits.memory_pages == narrowed.memory_pages => {
            k2::note(report::LIMITS_NARROWED, info.limits.memory_pages);
        }
        Ok(info) => {
            k2::note(report::UNEXPECTED, info.limits.memory_pages);
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Upwards, past anything the parent could be holding.
    let mut wider = narrowed;
    wider.memory_pages = u64::MAX / 2;
    k2::expect_refusal(k2::scope_set_limits(child, wider), status::LIMIT_EXHAUSTED);

    // Downwards, below what is already charged -- asked of the parent, because
    // that is the scope in this run which has actually spent something. Asked of
    // the child, which has been charged for nothing yet, it would be asking
    // whether zero is below a limit, and any answer would pass.
    match k2::scope_query(parent) {
        Ok(info) => {
            let mut under = info.limits;
            under.memory_pages = 1;
            k2::expect_refusal(k2::scope_set_limits(parent, under), status::STATE_CONFLICT);
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Reads the control log through a capability, and releases what it read.
///
/// The observability contract makes two claims about this plane, and until
/// something reads it both are the kernel's word against nothing: that the log
/// is reachable by capability, and that a receipt says who acted because the
/// kernel stamped it rather than because the caller asked for it. The second is
/// why this goes looking for one particular record -- the note the script
/// appended at the top with a deliberately false origin. The kernel logs that it
/// overrode the claim; a program reading the stamped origin back is the
/// independent half of that.
///
/// Reading and acknowledging are separate rights, and the end of this checks
/// they are separable in practice and not only in the table.
fn receipts(log: u64) {
    let own = match k2::domain_query(boot_handle(boot_slot::SELF_DOMAIN)) {
        Ok(info) if info.state == domain_state::RUNNABLE && info.faults == 0 => info,
        Ok(info) => {
            k2::note(report::UNEXPECTED, u64::from(info.state));
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    k2::note(report::DOMAIN_OBSERVED, u64::from(own.state));

    let before = match k2::log_query(log) {
        Ok(info) => info,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };

    let mut read_total = 0u64;
    let mut acknowledged = 0u64;
    let mut previous = 0u64;
    let mut schema = 0u32;
    let mut stamped = false;

    // A batch is bounded by the interface, so reading the ring means reading a
    // batch and then releasing it: acknowledging is the only thing that moves
    // the oldest record on. One pass per cell is more rounds than the ring can
    // need, so a read that stopped making progress ends this instead of
    // spinning on it.
    for _ in 0..=before.capacity {
        let batch = match k2::log_read(log) {
            Ok(batch) => batch,
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return;
            }
        };
        if batch.count == 0 {
            break;
        }
        read_total += u64::from(batch.count);
        let mut highest = previous;
        for record in batch.records.iter().take(batch.count as usize) {
            // One schema for the whole log, and not zero. A reader that did not
            // check this would be reading fields by position and hoping.
            if schema == 0 {
                schema = record.schema;
            }
            if record.schema == 0 || record.schema != schema {
                k2::note(report::UNEXPECTED, u64::from(record.schema));
                return;
            }
            // Strictly increasing, which is what lets a reader tell a gap in its
            // coverage from a log that simply had nothing to say.
            if record.sequence <= previous {
                k2::note(report::UNEXPECTED, record.sequence);
                return;
            }
            previous = record.sequence;
            highest = record.sequence;
            if record.origin_domain_id == FORGED_ORIGIN {
                k2::note(report::UNEXPECTED, record.origin_domain_id);
                return;
            }
            if record.kind == receipt_kind::SERVICE_NOTE && record.a == FORGED_NOTE {
                if record.origin_domain_id != own.domain_id {
                    k2::note(report::UNEXPECTED, record.origin_domain_id);
                    return;
                }
                stamped = true;
            }
        }
        match k2::log_acknowledge(log, highest) {
            Ok(dropped) => acknowledged += dropped,
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return;
            }
        }
    }

    if read_total == 0 || !stamped {
        k2::note(report::UNEXPECTED, read_total);
        return;
    }
    k2::note(report::RECEIPTS_READ, read_total);
    k2::note(report::ORIGIN_STAMPED, own.domain_id);

    // The log let go of exactly what was acknowledged, and a ring with nothing
    // left in it says its oldest and its next sequence are the same.
    match k2::log_query(log) {
        Ok(after)
            if after.used == 0
                && acknowledged == read_total
                && after.oldest_sequence == after.next_sequence =>
        {
            k2::note(report::RECEIPTS_ACKED, acknowledged);
        }
        Ok(after) => {
            k2::note(
                report::UNEXPECTED,
                (u64::from(after.used) << 32) | acknowledged,
            );
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Acknowledging something already released drops nothing. It is not an
    // error: a reader that crashed between reading and acknowledging will ask
    // again, and answering that with a refusal would make the retry the
    // dangerous path.
    match k2::log_acknowledge(log, before.next_sequence) {
        Ok(0) => {}
        Ok(dropped) => k2::note(report::UNEXPECTED, dropped),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    // Reading the log and releasing its cells are different rights, so a
    // narrowed handle must be able to do the first and not the second. Without
    // this, "a domain that may record what it did is not thereby allowed to read
    // what everyone else did" would be a statement about a bit nobody had ever
    // held separately.
    match k2::derive(log, right::INSPECT | right::LOG_READ, 0, 0) {
        Ok(reader) => {
            if let Err(code) = k2::log_read(reader) {
                k2::note(report::UNEXPECTED, code as u64);
            }
            k2::expect_refusal(
                k2::log_acknowledge(reader, before.next_sequence),
                status::INSUFFICIENT_RIGHTS,
            );
            if let Err(code) = k2::cap_close(reader) {
                k2::note(report::UNEXPECTED, code as u64);
            }
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Fences one grant without closing the scope that sponsors it.
///
/// A scope barrier and a grant barrier are different instruments and the
/// contract keeps them apart. Closing a perimeter stops everything charged to
/// it, which is the right answer when a whole tenant has to go. Withdrawing one
/// delegation has to stop that delegation and everything derived from it and
/// nothing else, which is the right answer when one client misbehaves and the
/// service it was calling must keep running. The vertical above exercised the
/// first. This exercises the second, and the two checks that make it a
/// different instrument are that the barrier reached the derivation below the
/// grant and stopped at the grant above it, while the scope stayed open
/// throughout.
fn grant_barrier(facet: u64) {
    // Two generations, because a single child would not distinguish "this grant"
    // from "everything under it". Both keep DERIVE so that the refusal further
    // down is a refusal about the barrier rather than about a missing right.
    let child = match k2::derive(
        facet,
        right::INSPECT | right::DERIVE | right::ADMIN | right::ENDPOINT_CALL,
        0,
        0,
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    let grandchild = match k2::derive(
        child,
        right::INSPECT | right::DERIVE | right::ENDPOINT_CALL,
        0,
        0,
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };

    // Alive before the barrier, or nothing that follows would mean anything.
    for handle in [facet, child, grandchild] {
        match k2::cap_inspect(handle) {
            Ok(info) if info.lineage_state == cap_lineage::LIVE => {}
            Ok(info) => {
                k2::note(report::UNEXPECTED, u64::from(info.lineage_state));
                return;
            }
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return;
            }
        }
    }

    // The grant and its one derivation: two nodes, and the count says so.
    match k2::cap_fence(child) {
        Ok(2) => k2::note(report::GRANT_FENCED, 2),
        Ok(changed) => {
            k2::note(report::UNEXPECTED, changed);
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // Downwards it reached; upwards it stopped.
    match (
        k2::cap_inspect(child),
        k2::cap_inspect(grandchild),
        k2::cap_inspect(facet),
    ) {
        (Ok(fenced), Ok(below), Ok(above)) => {
            if fenced.lineage_state != cap_lineage::FENCED
                || below.lineage_state != cap_lineage::FENCED
                || above.lineage_state != cap_lineage::LIVE
            {
                k2::note(
                    report::UNEXPECTED,
                    (u64::from(fenced.lineage_state) << 16)
                        | (u64::from(below.lineage_state) << 8)
                        | u64::from(above.lineage_state),
                );
                return;
            }
        }
        _ => {
            k2::note(report::UNEXPECTED, 0);
            return;
        }
    }

    // Fenced authority does not work. Deriving is what is attempted because the
    // handle still carries DERIVE, so the only thing left to refuse it is the
    // barrier.
    k2::expect_refusal(
        k2::derive(grandchild, right::INSPECT, 0, 0),
        status::SCOPE_CLOSED,
    );

    // And the grant above the barrier still does, which is the half that makes
    // this a withdrawal of one delegation and not of the object.
    match k2::derive(facet, right::INSPECT, 0, 0) {
        Ok(sibling) => {
            if let Err(code) = k2::cap_close(sibling) {
                k2::note(report::UNEXPECTED, code as u64);
            }
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    // What the fenced lineage still holds. Nothing, by now: the call this facet
    // carried was resolved long before. The point is that the question can be
    // asked at all once the barrier is in place -- an operation whose whole
    // purpose is to report on a fenced lineage is worth nothing if the barrier
    // it reports on is what stops it being called -- and that the answer
    // describes the grant and not the scope, which is wide open.
    match k2::cap_drain_status(child) {
        Ok(held) if held.state == scope_state::FENCED => {
            k2::note(report::GRANT_DRAINED, packed(&held));
        }
        Ok(held) => {
            k2::note(report::UNEXPECTED, packed(&held));
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // A handle whose lineage has been fenced is still the holder's to drop.
    // Authority that stopped working and cannot be let go of is a table entry
    // nobody can reclaim.
    for handle in [grandchild, child] {
        if let Err(code) = k2::cap_close(handle) {
            k2::note(report::UNEXPECTED, code as u64);
        }
    }
}

/// Builds a domain, asks it what it is, gives it a thread, and stops it.
///
/// It is never activated. A domain that never runs is the honest way to reach
/// the three operations that are about construction and ending rather than about
/// work: the run's own two domains are busy being the vertical, and stopping
/// either of them from outside would be stopping the thing under test.
fn spare_domain(scope: u64, image: u64, page_size: u64) {
    let spare = match k2::scope_create_domain(scope, image, name16("spare")) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    match k2::domain_query(spare) {
        Ok(info)
            if info.state == domain_state::BUILDING && info.threads == 1 && info.faults == 0 =>
        {
            k2::note(report::DOMAIN_OBSERVED, u64::from(info.state));
        }
        Ok(info) => {
            k2::note(
                report::UNEXPECTED,
                (u64::from(info.state) << 32) | u64::from(info.threads),
            );
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // An entry point the domain has not mapped is refused. The supervisor holds
    // DOMAIN_BUILD over this domain, so what is observed here is the address
    // being checked and not the authority being missing.
    k2::expect_refusal(
        k2::domain_add_thread(spare, 0, 0, 0),
        status::INVALID_ARGUMENT,
    );

    // One page of its own, so the thread below has an entry the domain has
    // mapped and a stack it could actually push on. The kernel checks both
    // against this domain's own mappings rather than taking them on trust,
    // which is the only reason passing them is safe at all.
    let scratch = match k2::scope_create_memory(
        scope,
        SPARE_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("spare"),
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    if let Err(code) = k2::domain_map(
        spare,
        scratch,
        SPARE_VADDR,
        0,
        SPARE_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    ) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }

    let stack_top = SPARE_VADDR + SPARE_PAGES * page_size;
    match k2::domain_add_thread(spare, SPARE_VADDR, stack_top, 0) {
        Ok(id) => k2::note(report::THREAD_ADDED, id),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }
    match k2::domain_query(spare) {
        Ok(info) if info.threads == 2 => {}
        Ok(info) => {
            k2::note(report::UNEXPECTED, u64::from(info.threads));
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    match k2::domain_terminate(spare) {
        Ok(id) => k2::note(report::DOMAIN_STOPPED, id),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }
    // Stopping what has already stopped is a state conflict, not a second stop.
    k2::expect_refusal(k2::domain_terminate(spare), status::STATE_CONFLICT);
    // And a domain that has stopped is no longer under construction. The
    // refusal has to say that, rather than blaming an entry point: an address
    // space is the first thing a stopped domain no longer has, so asking about
    // the entry first would report the wrong reason for every one of these.
    k2::expect_refusal(
        k2::domain_add_thread(spare, SPARE_VADDR, stack_top, 0),
        status::STATE_CONFLICT,
    );

    match k2::domain_query(spare) {
        Ok(info) if info.state != domain_state::BUILDING => {
            k2::note(report::DOMAIN_OBSERVED, u64::from(info.state));
        }
        Ok(info) => k2::note(report::UNEXPECTED, u64::from(info.state)),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Arms a timer against the monotonic clock and waits for it.
///
/// The clock read is the point as much as the timer is. Six operations of this
/// interface take a deadline and every one of them means an absolute monotonic
/// nanosecond; a program with no way to read the clock could only ever pass
/// zero or a value already in the past, and the whole parameter would be
/// decorative.
fn expiry(scope: u64, signal: u64) {
    let start = k2::now_ns();
    if start == 0 {
        k2::note(report::UNEXPECTED, 0);
        return;
    }

    let timer = match k2::scope_create_timer(scope, signal, TIMER_BIT) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };

    // A deadline in the past is a valid deadline; a deadline of zero is not,
    // because zero is how the interface spells "no deadline at all".
    k2::expect_refusal(k2::timer_arm(timer, 0), status::INVALID_ARGUMENT);

    // Read without waiting, which is the only way to tell "nothing has been
    // raised" from "something was raised and I consumed it". The bit must not
    // be set yet: a wait that was already satisfied before the timer was armed
    // would say nothing about the timer.
    let quiet = match k2::signal_query(signal) {
        Ok(info) if info.bits & TIMER_BIT == 0 => info,
        Ok(info) => {
            k2::note(report::UNEXPECTED, info.bits);
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };

    if let Err(code) = k2::timer_arm(timer, start + TIMER_DELAY_NS) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }
    match k2::signal_wait(signal, TIMER_BIT, 0) {
        Ok(_) => {}
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    // The sequence moved, and the bit the wait observed is gone: waiting
    // consumes, so a second waiter is not handed an expiry that already
    // happened. Bits coalesce and sequences do not, which is why the sequence
    // is the part a reader can count on.
    match k2::signal_query(signal) {
        Ok(info) if info.sequence > quiet.sequence && info.bits & TIMER_BIT == 0 => {
            k2::note(report::SIGNAL_OBSERVED, info.sequence);
        }
        Ok(info) => {
            k2::note(report::UNEXPECTED, (info.sequence << 32) | info.bits);
            return;
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }

    let after = k2::now_ns();
    if after <= start {
        k2::note(report::UNEXPECTED, after);
        return;
    }
    k2::note(report::CLOCK_ADVANCED, after - start);

    match k2::timer_query(timer) {
        Ok(info) if info.fired > 0 && info.armed == 0 => {
            k2::note(report::TIMER_FIRED, info.fired);
        }
        Ok(info) => k2::note(report::UNEXPECTED, info.fired),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    // Disarming one that has already fired is not an error; arming and
    // cancelling are the two halves a service needs to withdraw a deadline it
    // no longer wants.
    if let Err(code) = k2::timer_cancel(timer) {
        k2::note(report::UNEXPECTED, code as u64);
    }
}

rt::entry!(run);
