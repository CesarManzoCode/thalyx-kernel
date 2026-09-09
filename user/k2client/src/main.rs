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
//! 2. Attempts, before the call, two things it has no right to do. Both must be
//!    refused. A vertical where only the permitted operations are exercised
//!    shows that the kernel can say yes.
//! 3. Calls, and blocks. It is still blocked when its scope is fenced, which is
//!    the situation the whole run exists to produce: the client is closed while
//!    the server is holding admitted work.
//! 4. Reports what its call returned, then tries to call again. The second call
//!    is the observable half of the barrier: the origin is closed, so admission
//!    must refuse it.

#![no_std]
#![no_main]

use thalyx_abi::generated::{cap_op, right, status};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report, slot};

/// Bytes the client writes into its own buffer and expects the server to read
/// back through the narrowed capability.
const REQUEST_BYTES: &[u8] = b"k2-client-request";

/// Payload of the call itself, distinct from the buffer contents so a server
/// that echoed the payload could not be mistaken for one that read the memory.
const CALL_PAYLOAD: &[u8] = b"read-my-buffer";

/// Correlation value. Not authorisation: the kernel stamps the real origin.
const COOKIE: u64 = 0xC11E_0001;

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

    k2::note(report::DONE, 4);
    k2::exit(0)
}

rt::entry!(run);
