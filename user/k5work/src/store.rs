//! The managed-state client, speaking K4's protocol unchanged.
//!
//! K5's obligation is to use the durable service that exists, not to grow a
//! second one. So this is the K4 store protocol, byte for byte, with the K4
//! service on the other end and the K4 medium underneath. What is new is who is
//! calling and what for: a piece of Thalyx work, against a version it names.
//!
//! The narrowing on every call is deliberate and is done per call rather than
//! once: a call that needs the service to read lends a handle that cannot
//! write, and one that needs it to write lends a handle that cannot read. The
//! service gets the authority the operation needs and not the authority the
//! buffer has.

use thalyx_abi::{cap_op, right, status};
use thalyx_user_k4fmt::generated::{StoreReply, StoreRequest, store_op, store_status};
use thalyx_user_k4fmt::{self as k4, Pod};
use thalyx_user_k5pkg::proto::{note, work_addr};
use thalyx_user_rt::k2;

/// How long a call to the state service may take before the caller stops
/// waiting. A service that was cut mid-publication does not answer, and the
/// caller has to be able to say so rather than wait for ever.
pub const CALL_DEADLINE_NS: u64 = 8_000_000_000;

/// Bytes of the staging buffer.
pub const STAGE_BYTES: usize = (work_addr::STAGE_PAGES as usize) * 4096;

/// The staging buffer, at the address the package fixes.
///
/// # Safety
///
/// The supervisor maps `STAGE_PAGES` writable pages there before activating
/// this domain, and maps them nowhere else in it.
pub fn stage() -> &'static mut [u8] {
    // SAFETY: as the doc comment says.
    unsafe { core::slice::from_raw_parts_mut(work_addr::STAGE as *mut u8, STAGE_BYTES) }
}

/// What a call to the service answered, including the ways it did not.
pub enum Answer {
    /// The service replied.
    Reply(StoreReply),
    /// The service resolved the invocation without replying: the effect did
    /// not commit, or whether it did is exactly what nobody can say.
    Outcome(i64),
    /// The service is not there.
    Gone(i64),
}

/// A client of the state service.
pub struct Store {
    /// The facet. This is the principal: nothing in a request says who sent it.
    pub facet: u64,
    /// The staging buffer's capability, narrowed per call.
    pub stage_cap: u64,
    /// Calls made, which is also the cookie each one carries.
    pub calls: u64,
    /// The sequence of this principal's next publication.
    pub sequence: u64,
}

impl Store {
    /// One request, optionally lending the staging buffer with `lend` rights.
    pub fn call(&mut self, request: &StoreRequest, lend: u32) -> Answer {
        self.calls += 1;
        let deadline = k2::now_ns() + CALL_DEADLINE_NS;
        let result = if lend == 0 {
            k2::endpoint_call(
                self.facet,
                self.calls,
                request.as_bytes(),
                &[],
                deadline,
                false,
            )
        } else {
            // `TRANSFER` is what sending a capability costs: without it the
            // handle cannot cross the call at all. Everything else is dropped,
            // so what the service receives can do one thing to this buffer.
            let narrowed = match k2::derive(
                self.stage_cap,
                right::INSPECT | right::TRANSFER | lend,
                0,
                0,
            ) {
                Ok(handle) => handle,
                Err(code) => return Answer::Gone(code),
            };
            k2::endpoint_call(
                self.facet,
                self.calls,
                request.as_bytes(),
                &[(narrowed, cap_op::MOVE)],
                deadline,
                false,
            )
        };
        match result {
            Ok(reply) => match StoreReply::read_from(&reply.payload, 0) {
                Some(decoded) => Answer::Reply(decoded),
                None => Answer::Gone(status::INVALID_ARGUMENT),
            },
            Err(code @ (status::CANCELLED | status::PENDING)) => Answer::Outcome(code),
            Err(code) => Answer::Gone(code),
        }
    }

    /// A call whose answer is only interesting when the service said `OK`.
    pub fn ok(&mut self, request: &StoreRequest, lend: u32) -> Option<StoreReply> {
        match self.call(request, lend) {
            Answer::Reply(reply) if reply.status == store_status::OK => Some(reply),
            Answer::Reply(reply) => {
                k2::note(note::STORE_REFUSED, u64::from(reply.status));
                None
            }
            Answer::Outcome(code) => {
                k2::note(note::STORE_REFUSED, (-code) as u64 | (1 << 32));
                None
            }
            Answer::Gone(code) => {
                k2::note(note::STORE_REFUSED, (-code) as u64 | (2 << 32));
                None
            }
        }
    }

    /// The published generation, root and free capacity.
    pub fn query(&mut self) -> Option<StoreReply> {
        self.ok(
            &StoreRequest {
                op: store_op::QUERY,
                ..StoreRequest::zeroed()
            },
            0,
        )
    }

    /// Stages an immutable object out of the staging buffer.
    ///
    /// The service says what it computed and this client computes the same
    /// thing from the same bytes. A disagreement is not a detail to paper over:
    /// the identity of an object is what everything else is decided from.
    pub fn put_object(&mut self, object_type: u32, bytes: &[u8]) -> Option<[u8; 32]> {
        if bytes.len() > STAGE_BYTES {
            return None;
        }
        stage()[..bytes.len()].copy_from_slice(bytes);
        let reply = self.ok(
            &StoreRequest {
                op: store_op::PUT_OBJECT,
                arg0: u64::from(object_type),
                arg1: bytes.len() as u64,
                ..StoreRequest::zeroed()
            },
            right::MEMORY_READ,
        )?;
        if reply.digest0 != k4::object_digest(object_type, bytes) {
            k2::note(note::WORK_UNEXPECTED, 0x0BEC_7001);
            return None;
        }
        Some(reply.digest0)
    }

    /// Reads an object back into the staging buffer; answers how many bytes.
    pub fn read_object(&mut self, digest: [u8; 32], length: u64) -> Option<u64> {
        let reply = self.ok(
            &StoreRequest {
                op: store_op::READ,
                digest0: digest,
                arg0: 0,
                arg1: length,
                ..StoreRequest::zeroed()
            },
            right::MEMORY_WRITE,
        )?;
        Some(reply.value)
    }

    /// A private workspace over a version.
    pub fn fork(&mut self, generation: u64) -> Option<u64> {
        let reply = self.ok(
            &StoreRequest {
                op: store_op::FORK,
                arg0: generation,
                ..StoreRequest::zeroed()
            },
            0,
        )?;
        Some(reply.value)
    }

    /// Binds a name to an object inside a workspace.
    pub fn bind(&mut self, workspace: u64, name: &[u8], digest: [u8; 32]) -> Option<u64> {
        let mut padded = [0u8; 32];
        padded[..name.len()].copy_from_slice(name);
        let reply = self.ok(
            &StoreRequest {
                op: store_op::WRITE,
                arg0: workspace,
                arg1: name.len() as u64,
                digest0: digest,
                name: padded,
                ..StoreRequest::zeroed()
            },
            0,
        )?;
        Some(reply.value)
    }

    /// Turns a workspace into an immutable candidate root.
    pub fn freeze(
        &mut self,
        workspace: u64,
        policy: [u8; 32],
        validation: [u8; 32],
    ) -> Option<StoreReply> {
        self.ok(
            &StoreRequest {
                op: store_op::FREEZE,
                arg0: workspace,
                digest0: policy,
                digest1: validation,
                ..StoreRequest::zeroed()
            },
            0,
        )
    }

    /// Releases a workspace. Abandoning is this, and nothing published moves.
    pub fn discard(&mut self, workspace: u64) -> bool {
        self.ok(
            &StoreRequest {
                op: store_op::DISCARD,
                arg0: workspace,
                ..StoreRequest::zeroed()
            },
            0,
        )
        .is_some()
    }

    /// Asks for a conditioned transition of the published root.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        sequence: u64,
        generation: u64,
        root: [u8; 32],
        policy: [u8; 32],
        validation: [u8; 32],
        outbox_target: u64,
        outbox_payload: [u8; 32],
    ) -> Answer {
        self.call(
            &StoreRequest {
                op: store_op::PUBLISH,
                request_sequence: sequence,
                arg0: generation,
                arg1: outbox_target,
                digest0: root,
                digest1: policy,
                digest2: validation,
                name: outbox_payload,
                ..StoreRequest::zeroed()
            },
            0,
        )
    }
}
