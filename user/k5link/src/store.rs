//! The K4 state service, as the link calls it: K4's protocol, unchanged.
//!
//! This is `user/k5work/src/store.rs` again, because it is the same client of
//! the same service; what differs is who is behind it. A work domain of K5
//! called for itself; the link calls on behalf of a consumer outside the
//! machine, through the facet that is that consumer's principal, and through a
//! grant whose life the consumer's work scope bounds -- so that a fenced work's
//! next call is refused by the kernel and not by a rule in this program.

use thalyx_abi::{cap_op, right, status};
use thalyx_user_k4fmt::generated::{StoreReply, StoreRequest, store_op, store_status};
use thalyx_user_k4fmt::{self as k4, Pod};
use thalyx_user_k5pkg::link::{link_addr, note};
use thalyx_user_rt::k2;

/// How long a call to the state service may take before the caller stops
/// waiting. A service that was cut mid-publication does not answer.
pub const CALL_DEADLINE_NS: u64 = 8_000_000_000;

/// Bytes of the staging buffer.
pub const STAGE_BYTES: usize = (link_addr::STAGE_PAGES as usize) * 4096;

/// The staging buffer, at the address the package fixes.
///
/// # Safety
///
/// The supervisor maps `STAGE_PAGES` writable pages there before activating
/// this domain, and maps them nowhere else in it.
pub fn stage() -> &'static mut [u8] {
    // SAFETY: as the doc comment says; this domain is single-threaded.
    unsafe { core::slice::from_raw_parts_mut(link_addr::STAGE as *mut u8, STAGE_BYTES) }
}

/// What a call to the service answered, including the ways it did not.
pub enum Answer {
    Reply(StoreReply),
    /// The service resolved the invocation without replying: the effect did
    /// not commit, or whether it did is exactly what nobody can say.
    Outcome(i64),
    /// The kernel refused the call before the service saw it. `SCOPE_CLOSED`
    /// here is a fenced work: the grant the call went through died with the
    /// scope that bounded its life.
    Gone(i64),
}

/// A client of the state service through one grant.
pub struct Store {
    /// The facet, or a grant derived from it under a work's life scope.
    pub handle: u64,
    /// The staging buffer's capability, narrowed per call.
    pub stage_cap: u64,
    /// Calls made through this client.
    pub calls: u64,
    /// The last status the service refused with, for a caller that wants to
    /// say why.
    pub last_status: u32,
    /// The last kernel status a call was refused with.
    pub last_code: i64,
}

impl Store {
    pub const fn new(handle: u64, stage_cap: u64) -> Self {
        Store {
            handle,
            stage_cap,
            calls: 0,
            last_status: 0,
            last_code: 0,
        }
    }

    /// One request, optionally lending the staging buffer with `lend` rights.
    pub fn call(&mut self, request: &StoreRequest, lend: u32) -> Answer {
        self.calls += 1;
        let deadline = k2::now_ns() + CALL_DEADLINE_NS;
        let result = if lend == 0 {
            k2::endpoint_call(
                self.handle,
                self.calls,
                request.as_bytes(),
                &[],
                deadline,
                false,
            )
        } else {
            let narrowed = match k2::derive(
                self.stage_cap,
                right::INSPECT | right::TRANSFER | lend,
                0,
                0,
            ) {
                Ok(handle) => handle,
                Err(code) => {
                    self.last_code = code;
                    return Answer::Gone(code);
                }
            };
            k2::endpoint_call(
                self.handle,
                self.calls,
                request.as_bytes(),
                &[(narrowed, cap_op::MOVE)],
                deadline,
                false,
            )
        };
        match result {
            Ok(reply) => match StoreReply::read_from(&reply.payload, 0) {
                Some(decoded) => {
                    self.last_status = decoded.status;
                    Answer::Reply(decoded)
                }
                None => Answer::Gone(status::INVALID_ARGUMENT),
            },
            Err(code @ (status::CANCELLED | status::PENDING)) => {
                self.last_code = code;
                Answer::Outcome(code)
            }
            Err(code) => {
                self.last_code = code;
                Answer::Gone(code)
            }
        }
    }

    /// A call whose answer is only interesting when the service said `OK`.
    pub fn ok(&mut self, request: &StoreRequest, lend: u32) -> Option<StoreReply> {
        match self.call(request, lend) {
            Answer::Reply(reply) if reply.status == store_status::OK => Some(reply),
            Answer::Reply(reply) => {
                k2::note(
                    note::STORE_REFUSED,
                    u64::from(reply.status) | (u64::from(request.op) << 8),
                );
                None
            }
            Answer::Outcome(code) => {
                k2::note(
                    note::STORE_REFUSED,
                    (-code) as u64 | (u64::from(request.op) << 8) | (1 << 32),
                );
                None
            }
            Answer::Gone(code) => {
                k2::note(
                    note::STORE_REFUSED,
                    (-code) as u64 | (u64::from(request.op) << 8) | (2 << 32),
                );
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
            k2::note(note::LINK_UNEXPECTED, 0x0BEC_7001);
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

    /// Releases a workspace.
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

    /// What became of one of this principal's request identities.
    pub fn result(&mut self, sequence: u64) -> Option<StoreReply> {
        match self.call(
            &StoreRequest {
                op: store_op::RESULT,
                request_sequence: sequence,
                ..StoreRequest::zeroed()
            },
            0,
        ) {
            Answer::Reply(reply) => Some(reply),
            Answer::Outcome(_) | Answer::Gone(_) => None,
        }
    }

    /// Asks for a conditioned transition of the published root.
    pub fn publish(
        &mut self,
        sequence: u64,
        generation: u64,
        root: [u8; 32],
        policy: [u8; 32],
        validation: [u8; 32],
    ) -> Answer {
        self.call(
            &StoreRequest {
                op: store_op::PUBLISH,
                request_sequence: sequence,
                arg0: generation,
                arg1: 0,
                digest0: root,
                digest1: policy,
                digest2: validation,
                name: [0u8; 32],
                ..StoreRequest::zeroed()
            },
            0,
        )
    }

    /// Maintenance: copies the reachable set to the other arena.
    pub fn compact(&mut self) -> Option<StoreReply> {
        self.ok(
            &StoreRequest {
                op: store_op::COMPACT,
                ..StoreRequest::zeroed()
            },
            0,
        )
    }
}
