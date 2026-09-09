//! K2 user domain `k2client`.
//!
//! The point of this program is what it *cannot* do. It starts with two
//! capabilities and nothing else: one facet of an endpoint, and one scratch
//! memory object of its own. There is no naming service to ask, no syscall that
//! opens a resource by name, and no way to enumerate what it does not hold. If
//! it reaches anything outside itself, it is because someone handed it the
//! authority to.
//!
//! What it does with that:
//!
//! 1. Writes a request into its own buffer and narrows a capability over it to
//!    read-only, then hands that narrowed capability to the server with the
//!    call. This is authority derived and delegated by a program that has none
//!    to spare, and the server can be watched failing to widen it back.
//! 2. Exercises the rest of what a capability is: copying one, closing it,
//!    watching the freed table slot come back under a different handle, and
//!    holding one past its deadline. The recycled slot is the case that matters
//!    most -- a handle whose slot has been reused must not name its new
//!    occupant, or every closed capability is a capability waiting to be
//!    reinterpreted.
//! 3. Attempts several things it has no right to do, and sends several
//!    descriptors that are deliberately malformed. All must be refused. A
//!    vertical where only the permitted operations are exercised shows that the
//!    kernel can say yes.
//! 4. Reads the sealed page its supervisor published and mapped into it. The
//!    read goes through the mapping, with an ordinary load, because the mapping
//!    is the thing being tested. Writing it is refused twice over: the object is
//!    sealed and the capability carries no write right. A sealed object nobody
//!    can write is the only kind whose contents a reader may rely on without
//!    copying them first.
//!
//!    The page-level half of that -- a store to a read-only mapping faulting in
//!    hardware -- is [K1's probe](../../../vault/evidence/k1-protected-boot.md)
//!    and is not repeated here: a domain that killed itself proving it would not
//!    be around for the rest of this run, which is what the run is about.
//! 5. Calls, and blocks. It is still blocked when its scope is fenced, which is
//!    the situation the whole run exists to produce: the client is closed while
//!    the server is holding admitted work.
//! 6. Reports what its call returned, then tries to call again. The second call
//!    is the observable half of the barrier: the origin is closed, so admission
//!    must refuse it.

#![no_std]
#![no_main]

use thalyx_abi::generated::{cap_op, right, status};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, malformed, report, slot};

/// Bytes the client writes into its own buffer and expects the server to read
/// back through the narrowed capability.
const REQUEST_BYTES: &[u8] = b"k2-client-request";

/// Payload of the call itself, distinct from the buffer contents so a server
/// that echoed the payload could not be mistaken for one that read the memory.
const CALL_PAYLOAD: &[u8] = b"read-my-buffer";

/// Correlation value. Not authorisation: the kernel stamps the real origin.
const COOKIE: u64 = 0xC11E_0001;

/// Rounds the client keeps running after being cancelled, and the work in each.
/// Sized so the server gets scheduled while this domain is still alive.
const LINGER_ROUNDS: u64 = 64;
const LINGER_WORK: u64 = 20_000;

/// Where the supervisor maps the sealed page it published.
const PUBLISHED_VADDR: u64 = 0x0000_0000_5000_0000;

fn run() -> ! {
    let endpoint = thalyx_abi::boot_handle(slot::CLIENT_ENDPOINT);
    let buffer = thalyx_abi::boot_handle(slot::CLIENT_BUFFER);

    let limits = match k2::limits() {
        Ok(limits) => limits,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            k2::exit(1);
        }
    };
    k2::note(
        report::LIMITS,
        (u64::from(limits.major) << 48) | (u64::from(limits.minor) << 32) | limits.page_size,
    );

    // The buffer is the client's own object, so writing it is permitted.
    if let Err(code) = k2::memory_write(buffer, 0, REQUEST_BYTES) {
        k2::note(report::UNEXPECTED, code as u64);
        k2::exit(2);
    }

    // Two things this domain has no authority for. A handle it was never given
    // names nothing in its own table, and sealing is a right its grant over its
    // own buffer does not carry. Neither may be reinterpreted as something it
    // can do.
    let unheld = thalyx_abi::boot_handle(slot::CLIENT_BUFFER + 9);
    k2::expect_refusal(k2::cap_close(unheld), status::INVALID_HANDLE);
    k2::expect_refusal(
        k2::memory_seal(buffer).map(|_| 0),
        status::INSUFFICIENT_RIGHTS,
    );

    capability_lifecycle(buffer);
    malformed_requests(buffer, endpoint);
    published_page();

    // Narrow the buffer to read-only and hand that, not the original, to the
    // server. `DERIVE` only ever removes rights, so the capability the server
    // receives cannot be widened back into the one this domain holds.
    let readable = match k2::derive(
        buffer,
        right::INSPECT | right::TRANSFER | right::MEMORY_READ,
        0,
        0,
    ) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            k2::exit(3);
        }
    };
    k2::note(report::BUILT, 1);

    // The call blocks. The run is arranged so that this domain's scope is
    // fenced while it is still here, with the server holding the work.
    let outcome = k2::endpoint_call(
        endpoint,
        COOKIE,
        CALL_PAYLOAD,
        &[(readable, cap_op::MOVE)],
        0,
        false,
    );
    let code = match outcome {
        Ok(_) => status::OK,
        Err(code) => code,
    };
    k2::note(report::CALL_RESULT, code as u64);

    // The origin is closed now. Admission must refuse a new request rather than
    // queue one nobody will ever answer.
    k2::expect_refusal(
        k2::endpoint_call(endpoint, COOKIE + 1, CALL_PAYLOAD, &[], 0, false).map(|_| 0),
        status::SCOPE_CLOSED,
    );

    // A cancelled client does not vanish. It keeps running until it decides to
    // stop, which is the whole reason a barrier and a drain are different
    // things -- and it is also what lets the server observe the origin as
    // fenced rather than only as gone. A domain that exited the instant it was
    // cancelled would erase the distinction the run exists to show.
    let mut linger = 0u64;
    while linger < LINGER_ROUNDS {
        let _ = rt::burn(LINGER_WORK, linger | 1);
        linger += 1;
    }

    k2::note(report::DONE, 4);
    k2::exit(0)
}

/// Reads the sealed page the supervisor mapped, and confirms it is read-only.
///
/// The read goes through the mapping rather than through `MEMORY_READ`, because
/// the point is the mapping: a page the kernel installed in this domain's
/// address space with the rights it was told to. The write is the control. It
/// is announced first so the fault that follows can be matched to an intent,
/// and the domain does not survive it -- a store to a read-only mapping is a
/// protection failure, not an error code.
fn published_page() {
    let handle = thalyx_abi::boot_handle(slot::CLIENT_PUBLISHED);
    let info = match k2::memory_query(handle) {
        Ok(info) => info,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    if info.state != thalyx_abi::generated::memory_state::SEALED {
        k2::note(report::UNEXPECTED, u64::from(info.state));
        return;
    }

    // SAFETY: the supervisor mapped this address readable in this domain. A
    // load through a mapping that is genuinely present is an ordinary load;
    // if it were not present, the fault would be contained like any other.
    let word = unsafe { rt::probe::read_u64(PUBLISHED_VADDR) };
    if word == 0 {
        k2::note(report::UNEXPECTED, word);
        return;
    }
    k2::note(report::READ_MAPPED, word);

    // Two independent reasons this domain cannot write it, and the interface
    // refuses on the first it reaches: the capability carries no write right.
    k2::expect_refusal(
        k2::memory_write(handle, 0, b"tampered"),
        status::INSUFFICIENT_RIGHTS,
    );
    // Nor can it widen the capability into one that could.
    k2::expect_refusal(
        k2::derive(handle, right::MEMORY_READ | right::MEMORY_WRITE, 0, 0),
        status::INSUFFICIENT_RIGHTS,
    );
}

/// Copy, close, recycle and expire, on the domain's own buffer.
///
/// The copy has to carry the same rights, because a copy that quietly narrowed
/// or widened would not be one. The closed handle has to stop working. And the
/// slot the close freed has to come back with a different generation, so the
/// handle that used to name it names nothing -- the ABA case, checked against a
/// table that really did reuse the slot rather than against an assumption that
/// it might.
fn capability_lifecycle(buffer: u64) {
    let mut scratch = [0u8; 32];

    let copy = match k2::cap_copy(buffer) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    let same_rights = match (k2::cap_inspect(buffer), k2::cap_inspect(copy)) {
        (Ok(original), Ok(duplicate)) => original.rights == duplicate.rights,
        _ => false,
    };
    if !same_rights || copy == buffer {
        k2::note(report::UNEXPECTED, copy);
        return;
    }
    if k2::memory_read(copy, 0, &mut scratch).is_err() {
        k2::note(report::UNEXPECTED, copy);
        return;
    }
    k2::note(report::COPIED, copy);

    if let Err(code) = k2::cap_close(copy) {
        k2::note(report::UNEXPECTED, code as u64);
        return;
    }
    k2::expect_refusal(
        k2::memory_read(copy, 0, &mut scratch),
        status::INVALID_HANDLE,
    );

    // The next copy takes the lowest free slot, which is the one just freed.
    // Its handle must differ from the one that named the same slot before.
    let reused = match k2::cap_copy(buffer) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    if thalyx_abi::handle_slot(reused) == thalyx_abi::handle_slot(copy) && reused != copy {
        k2::note(report::SLOT_RECYCLED, reused);
    } else {
        k2::note(report::UNEXPECTED, reused);
    }
    k2::expect_refusal(
        k2::memory_read(copy, 0, &mut scratch),
        status::INVALID_HANDLE,
    );
    let _ = k2::cap_close(reused);

    // A deadline of one nanosecond after boot is already in the past by the
    // time any user code runs, so the derived capability is born expired. That
    // is a deterministic way to reach the expiry path without a clock the
    // interface does not yet expose.
    match k2::derive(buffer, right::INSPECT | right::MEMORY_READ, 1, 0) {
        Ok(expired) => {
            k2::expect_refusal(k2::memory_read(expired, 0, &mut scratch), status::EXPIRED);
            let _ = k2::cap_close(expired);
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
}

/// Requests that are structurally wrong, sent on the path that accepts the
/// right ones.
///
/// The last three are transfers. If any of them had a partial effect, the
/// capability they name would be gone afterwards; the check at the end is what
/// turns "it was refused" into "it was refused and nothing moved".
fn malformed_requests(buffer: u64, endpoint: u64) {
    k2::expect_refusal(malformed::short_length(buffer), status::INVALID_ARGUMENT);
    k2::expect_refusal(malformed::wrong_opcode(buffer), status::INVALID_ARGUMENT);
    k2::expect_refusal(malformed::dirty_reserved(buffer), status::INVALID_ARGUMENT);
    k2::expect_refusal(
        malformed::future_version(buffer),
        status::INCOMPATIBLE_VERSION,
    );
    k2::expect_refusal(
        malformed::unreachable_descriptor(buffer),
        status::INVALID_ADDRESS,
    );
    k2::expect_refusal(malformed::unknown_flag(buffer), status::INVALID_ARGUMENT);
    k2::expect_refusal(
        malformed::unassigned_operation(buffer),
        status::NOT_SUPPORTED,
    );

    let transferable = match k2::derive(buffer, right::INSPECT | right::TRANSFER, 0, 0) {
        Ok(handle) => handle,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    };
    // `ENDPOINT_CALL` is the operation this domain holds a right to. A body
    // check runs after the rights check, so probing it through `ENDPOINT_SEND`
    // would only ever observe the refusal for the missing right.
    let call = thalyx_abi::generated::op::ENDPOINT_CALL;
    k2::expect_refusal(
        malformed::overlong_cap_count(endpoint, call, transferable),
        status::INVALID_ARGUMENT,
    );
    k2::expect_refusal(
        malformed::overlong_payload(endpoint, call),
        status::INVALID_ARGUMENT,
    );
    k2::expect_refusal(
        malformed::unknown_cap_op(endpoint, call, transferable),
        status::INVALID_ARGUMENT,
    );
    k2::expect_refusal(
        malformed::duplicate_move(endpoint, call, transferable),
        status::INVALID_ARGUMENT,
    );

    // Nothing moved: the handle every refused transfer named is still here.
    match k2::cap_inspect(transferable) {
        Ok(_) => k2::note(report::BUILT, 9),
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }
    let _ = k2::cap_close(transferable);
}

rt::entry!(run);
