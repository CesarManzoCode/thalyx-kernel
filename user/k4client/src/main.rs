//! K4 user domain `k4client`: the programs that use the state service.
//!
//! One image, four roles. The supervisor writes a role into a page and maps it
//! read-only, so a domain cannot choose to be a different one; building four
//! images instead would have made "the role is not the program's choice" a
//! claim about the build rather than about the kernel.
//!
//! * **Publisher** — the ordinary sequence: stage the objects a version needs,
//!   fork a workspace over the published root, bind names in it, freeze it into
//!   a candidate, and publish against the generation it was forked from. Then
//!   the refusals that matter, each one asked for on purpose: the same request
//!   again, the same sequence with different inputs, a sequence with a gap, a
//!   stale expectation, and evidence that names other inputs.
//! * **Rival** — the same publication against the same generation, so two
//!   publishers race for one transition and exactly one of them may win.
//! * **Reader** — reads what is published and what is retained, asks what
//!   became of requests it never made, and tries the two things a reader is not
//!   allowed to do.
//! * **Broker** — answers outbox intents, sometimes with a result nobody can
//!   assert anything from, which is the case the service has to record rather
//!   than resolve.
//!
//! The client computes the canonical digest of the tree it is about to publish
//! itself, because evidence has to name the inputs it is about *before* the
//! version exists. Its encoder and the service's are the same code over the
//! same bindings; if they ever disagreed, the manifest and the validation would
//! name different trees and admission would refuse the publication.

#![no_std]
#![no_main]

use thalyx_abi::boot_handle;
use thalyx_abi::generated::{cap_op, right};
use thalyx_user_k4fmt as k4;
use thalyx_user_k4fmt::pkg::{
    CONFIG_VADDR, ClientConfig, OutboxAnswer, OutboxIntent, STAGE_PAGES, STAGE_VADDR, bit,
    client_slot, note, role,
};
use thalyx_user_k4fmt::{
    Binding, Pod, Policy, StoreReply, StoreRequest, Validation, object_type, outbox_status,
    result_outcome, store_op, store_status,
};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0xD91E_4444_5555_6001;

/// How long a client waits for the service to answer.
const CALL_DEADLINE_NS: u64 = 8_000_000_000;

/// The validation tool the policy names.
const TOOL_ID: u64 = 0x4B34_1001;

/// Coverage the policy requires, in parts per million.
const MIN_COVERAGE_PPM: u64 = 900_000;

/// Coverage the evidence claims.
const COVERAGE_PPM: u64 = 950_000;

/// The instant evidence claims to have been produced at.
///
/// Fixed rather than read from the clock, so the same publication attempted in
/// two runs is the same request. See `validation_for`.
const EVIDENCE_EPOCH_NS: u64 = 1_700_000_000_000_000_000;

/// Bytes of the staging buffer.
const STAGE_BYTES: usize = (STAGE_PAGES as usize) * 4096;

/// Intents a broker remembers, so a repeated key is answered as one.
const BROKER_KEYS: usize = 8;

/// How long a broker waits for an intent before deciding the run is over.
const BROKER_IDLE_NS: u64 = 3_000_000_000;

/// Idle waits a broker takes before it stops.
const BROKER_IDLE_ROUNDS: u32 = 4;

/// The staging buffer this client lends by capability.
fn stage() -> &'static mut [u8] {
    // SAFETY: the supervisor mapped `STAGE_PAGES` writable pages at
    // `STAGE_VADDR` before this domain was activated, and this domain is
    // single-threaded, so no other borrow of them exists.
    unsafe { core::slice::from_raw_parts_mut(STAGE_VADDR as *mut u8, STAGE_BYTES) }
}

/// The configuration the supervisor wrote and mapped read-only.
fn configuration() -> ClientConfig {
    // SAFETY: one page, mapped read-only at this address before the domain ran,
    // holding exactly this structure written by the supervisor.
    let bytes = unsafe { core::slice::from_raw_parts(CONFIG_VADDR as *const u8, 4096) };
    ClientConfig::read_from(bytes, 0).unwrap_or_default()
}

/// A handle on the service, and the buffer this client lends it.
struct Client {
    store: u64,
    stage_cap: u64,
    calls: u64,
}

impl Client {
    /// Sends one request, optionally lending the staging buffer.
    ///
    /// The narrowing is done here and not once at startup: a call that needs
    /// the service to read gives it a handle that cannot write, and one that
    /// needs it to write gives a handle that cannot read. The service gets the
    /// authority the operation needs and not the authority the buffer has.
    fn call(&mut self, request: &StoreRequest, lend: u32) -> Option<StoreReply> {
        self.calls += 1;
        let deadline = k2::now_ns() + CALL_DEADLINE_NS;
        let result = if lend == 0 {
            k2::endpoint_call(
                self.store,
                self.calls,
                request.as_bytes(),
                &[],
                deadline,
                false,
            )
        } else {
            let narrowed = k2::derive(self.stage_cap, right::INSPECT | lend, 0, 0).ok()?;
            k2::endpoint_call(
                self.store,
                self.calls,
                request.as_bytes(),
                &[(narrowed, cap_op::MOVE)],
                deadline,
                false,
            )
        };
        match result {
            Ok(reply) => StoreReply::read_from(&reply.payload, 0),
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                None
            }
        }
    }

    fn query(&mut self) -> Option<StoreReply> {
        self.call(
            &StoreRequest {
                op: store_op::QUERY,
                ..StoreRequest::zeroed()
            },
            0,
        )
    }

    /// Stages an object out of the lent buffer and returns its digest.
    fn put_object(&mut self, object_type: u32, bytes: &[u8]) -> Option<[u8; 32]> {
        if bytes.len() > STAGE_BYTES {
            return None;
        }
        stage()[..bytes.len()].copy_from_slice(bytes);
        let reply = self.call(
            &StoreRequest {
                op: store_op::PUT_OBJECT,
                arg0: u64::from(object_type),
                arg1: bytes.len() as u64,
                ..StoreRequest::zeroed()
            },
            right::MEMORY_READ,
        )?;
        if reply.status != store_status::OK {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
            return None;
        }
        // The service says what it computed; this client computed the same
        // thing from the same bytes. A disagreement is not a detail to paper
        // over: the identity of an object is what everything else is decided
        // from.
        if reply.digest0 != k4::object_digest(object_type, bytes) {
            k2::note(report::UNEXPECTED, 0x0BEC_7001);
            return None;
        }
        Some(reply.digest0)
    }

    /// Reads an object back into the lent buffer, returning how many bytes.
    fn read_object(&mut self, digest: [u8; 32], length: u64) -> Option<u64> {
        let reply = self.call(
            &StoreRequest {
                op: store_op::READ,
                digest0: digest,
                arg0: 0,
                arg1: length,
                ..StoreRequest::zeroed()
            },
            right::MEMORY_WRITE,
        )?;
        if reply.status != store_status::OK {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
            return None;
        }
        k2::note(note::CLIENT_READ, reply.value);
        Some(reply.value)
    }

    fn fork(&mut self, generation: u64) -> Option<u64> {
        let reply = self.call(
            &StoreRequest {
                op: store_op::FORK,
                arg0: generation,
                ..StoreRequest::zeroed()
            },
            0,
        )?;
        if reply.status != store_status::OK {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
            return None;
        }
        Some(reply.value)
    }

    fn bind(&mut self, workspace: u64, name: &[u8], digest: [u8; 32]) -> Option<u64> {
        let mut padded = [0u8; 32];
        padded[..name.len()].copy_from_slice(name);
        let reply = self.call(
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
        if reply.status != store_status::OK {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
            return None;
        }
        Some(reply.value)
    }

    fn freeze(
        &mut self,
        workspace: u64,
        policy: [u8; 32],
        validation: [u8; 32],
    ) -> Option<StoreReply> {
        let reply = self.call(
            &StoreRequest {
                op: store_op::FREEZE,
                arg0: workspace,
                digest0: policy,
                digest1: validation,
                ..StoreRequest::zeroed()
            },
            0,
        )?;
        if reply.status != store_status::OK {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
            return None;
        }
        Some(reply)
    }

    fn discard(&mut self, workspace: u64) {
        let _ = self.call(
            &StoreRequest {
                op: store_op::DISCARD,
                arg0: workspace,
                ..StoreRequest::zeroed()
            },
            0,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        &mut self,
        sequence: u64,
        generation: u64,
        root: [u8; 32],
        policy: [u8; 32],
        validation: [u8; 32],
        outbox_target: u64,
        outbox_payload: [u8; 32],
    ) -> Option<StoreReply> {
        let reply = self.call(
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
        )?;
        if reply.status == store_status::OK {
            k2::note(note::CLIENT_PUBLISHED, reply.generation);
        } else {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
        }
        Some(reply)
    }

    fn result(&mut self, sequence: u64) -> Option<StoreReply> {
        let reply = self.call(
            &StoreRequest {
                op: store_op::RESULT,
                request_sequence: sequence,
                ..StoreRequest::zeroed()
            },
            0,
        )?;
        k2::note(
            note::CLIENT_RESULT,
            u64::from(reply.outcome) | (sequence << 8),
        );
        Some(reply)
    }
}

/// The policy every version in this run is published under.
fn policy_bytes() -> Policy {
    let mut config_digest = [0u8; 32];
    for (index, byte) in config_digest.iter_mut().enumerate() {
        *byte = 0x40 ^ (index as u8);
    }
    Policy {
        format: 1,
        requires_validation: 1,
        // The publisher and the rival may publish. The reader may not, and the
        // policy says so as well as the package does: authority narrowed twice
        // is refused twice.
        publish_principals: (1 << 1) | (1 << 2),
        validation_tool_id: TOOL_ID,
        min_coverage_ppm: MIN_COVERAGE_PPM,
        tool_config_digest: config_digest,
    }
}

/// Evidence about exactly these inputs and this base version.
fn validation_for(tree: [u8; 32], generation: u64, result: u32) -> Validation {
    let policy = policy_bytes();
    Validation {
        format: 1,
        result,
        input_tree_digest: tree,
        base_generation: generation,
        tool_id: policy.validation_tool_id,
        tool_config_digest: policy.tool_config_digest,
        coverage_ppm: COVERAGE_PPM,
        // Not the clock. The identity of a request covers every input that
        // decides what it means, evidence included, so a request repeated
        // after a crash has to hash to the same thing it did before the
        // crash. A timestamp here would make every retry a different request,
        // and deduplication would never once be exercised.
        produced_ns: EVIDENCE_EPOCH_NS + generation,
    }
}

/// The canonical digest of a tree with one binding, computed here.
fn tree_digest(bindings: &[Binding], scratch: &mut [u8]) -> Option<([u8; 32], usize)> {
    let written = k4::encode_tree(scratch, bindings).ok()?;
    Some((
        k4::object_digest(object_type::TREE, &scratch[..written]),
        written,
    ))
}

/// One publication, from staging its content to the answer.
struct Attempt {
    root: [u8; 32],
    policy: [u8; 32],
    validation: [u8; 32],
    tree: [u8; 32],
    content: [u8; 32],
}

/// Builds a candidate version binding `name` to `content`.
fn candidate(
    client: &mut Client,
    generation: u64,
    name: &[u8],
    content: &[u8],
    valid: bool,
) -> Option<Attempt> {
    let policy = policy_bytes();
    let policy_digest = client.put_object(object_type::POLICY, policy.as_bytes())?;
    let content_digest = client.put_object(object_type::BYTES, content)?;

    let binding = Binding::new(
        name,
        object_type::BYTES,
        0,
        content.len() as u64,
        content_digest,
    )
    .ok()?;
    let mut scratch = [0u8; 1024];
    let (tree, _) = tree_digest(&[binding], &mut scratch)?;

    // Evidence names the tree it is about, and the version it was computed
    // against. `valid` false makes it name a different tree, which is the
    // control: evidence does not become a claim about these inputs by being
    // presented alongside them.
    let named = if valid {
        tree
    } else {
        k4::object_digest(object_type::TREE, b"not these inputs")
    };
    let validation = validation_for(named, generation, 1);
    let validation_digest = client.put_object(object_type::VALIDATION, validation.as_bytes())?;

    let workspace = client.fork(generation)?;
    client.bind(workspace, name, content_digest)?;
    let frozen = client.freeze(workspace, policy_digest, validation_digest)?;
    client.discard(workspace);
    if frozen.digest1 != tree {
        // The service encoded the same bindings and got other bytes. Nothing
        // after this would mean anything.
        k2::note(report::UNEXPECTED, 0x0BEC_7002);
        return None;
    }
    Some(Attempt {
        root: frozen.digest0,
        policy: policy_digest,
        validation: validation_digest,
        tree,
        content: content_digest,
    })
}

/// Bytes of one version's content, derived from the round.
fn content_for(round: u64, buffer: &mut [u8; 64]) -> usize {
    let text = b"thalyx-k4 version ";
    buffer[..text.len()].copy_from_slice(text);
    let mut at = text.len();
    let mut value = round;
    let mut digits = [0u8; 20];
    let mut count = 0usize;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    while count > 0 {
        count -= 1;
        buffer[at] = digits[count];
        at += 1;
    }
    buffer[at] = b'\n';
    at + 1
}

fn run_publisher(client: &mut Client, config: &ClientConfig) {
    let Some(state) = client.query() else {
        return;
    };
    let mut generation = state.generation;
    let mut sequence = 0u64;
    let mut first_root = [0u8; 32];
    let mut first_content = [0u8; 32];

    for round in 1..=config.rounds {
        let mut buffer = [0u8; 64];
        let length = content_for(round, &mut buffer);
        let Some(attempt) = candidate(client, generation, b"data", &buffer[..length], true) else {
            return;
        };
        sequence += 1;
        // An outbox intent rides with the second publication, so the record of
        // what a broker said is part of a version rather than beside it.
        let target = if round == 2 { 0x4B34_B001 } else { 0 };
        let Some(reply) = client.publish(
            sequence,
            generation,
            attempt.root,
            attempt.policy,
            attempt.validation,
            target,
            attempt.tree,
        ) else {
            return;
        };
        if reply.status != store_status::OK {
            // A refusal here is either the rival winning the race or the run
            // being cut; both are things the log records rather than things
            // this client works around.
            return;
        }
        if reply.outcome == result_outcome::COMMITTED && reply.generation != 0 {
            generation = reply.generation;
        } else {
            // The store answered for a request it had already resolved, and
            // resolved as an abort: recovery found it prepared and unresolved
            // and would not assert it. The request is answered; what is
            // published is whatever the store says it is.
            k2::note(
                note::CLIENT_RESULT,
                u64::from(reply.outcome) | (sequence << 8),
            );
            let Some(state) = client.query() else {
                return;
            };
            generation = state.generation;
        }
        if round == 1 {
            first_root = attempt.root;
            first_content = attempt.content;
        }
    }

    if config.rounds == 0 {
        return;
    }

    // The same request again, byte for byte. One request, one generation: the
    // answer has to be the one already durable rather than a second one.
    let mut buffer = [0u8; 64];
    let length = content_for(config.rounds, &mut buffer);
    let base = generation - 1;
    if let Some(attempt) = candidate(client, base, b"data", &buffer[..length], true)
        && let Some(reply) = client.publish(
            sequence,
            base,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::OK && reply.generation == generation {
            k2::note(
                note::CLIENT_RESULT,
                u64::from(result_outcome::COMMITTED) | (sequence << 8),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // The same sequence, different inputs. That is not a repeat, and calling it
    // one would let a client change what a request meant after the fact.
    let mut other = [0u8; 64];
    let length = content_for(config.rounds + 100, &mut other);
    if let Some(attempt) = candidate(client, generation, b"data", &other[..length], true)
        && let Some(reply) = client.publish(
            sequence,
            generation,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::CONFLICT {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::CONFLICT),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // A sequence with a gap in front of it.
    if let Some(attempt) = candidate(client, generation, b"data", b"gap\n", true)
        && let Some(reply) = client.publish(
            sequence + 3,
            generation,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::SEQUENCE_GAP {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::SEQUENCE_GAP),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // An expectation about a version that is no longer the published one.
    sequence += 1;
    if generation > 1
        && let Some(attempt) = candidate(client, generation, b"data", b"stale\n", true)
        && let Some(reply) = client.publish(
            sequence,
            generation - 1,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::GENERATION_STALE {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::GENERATION_STALE),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // Evidence that names other inputs, offered for these ones.
    if let Some(attempt) = candidate(client, generation, b"data", b"unevidenced\n", false)
        && let Some(reply) = client.publish(
            sequence,
            generation,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::VALIDATION_MISMATCH {
            k2::note(
                note::VALIDATION_REFUSED,
                u64::from(store_status::VALIDATION_MISMATCH),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // The ABA case. The first version's content is published again, so the
    // content digest of the published version repeats while the generation does
    // not. An expectation on content would be satisfied by the wrong version;
    // an expectation on generation is refused, and that is the difference.
    if first_content != [0u8; 32] {
        let mut buffer = [0u8; 64];
        let length = content_for(1, &mut buffer);
        if let Some(attempt) = candidate(client, generation, b"data", &buffer[..length], true)
            && attempt.root == first_root
            && let Some(reply) = client.publish(
                sequence,
                1,
                attempt.root,
                attempt.policy,
                attempt.validation,
                0,
                attempt.tree,
            )
        {
            if reply.status == store_status::GENERATION_STALE {
                k2::note(note::CLIENT_ABA, u64::from(store_status::GENERATION_STALE));
            } else {
                k2::note(report::NOT_REFUSED, u64::from(reply.status));
            }
        }
    }

    // Hold the published version, then read it back through the retention.
    let held = client.call(
        &StoreRequest {
            op: store_op::RETAIN,
            digest0: first_root,
            arg0: k2::now_ns() + 60_000_000_000,
            ..StoreRequest::zeroed()
        },
        0,
    );
    if let Some(reply) = held {
        k2::note(note::CLIENT_RESULT, reply.value);
    }
    let _ = client.read_object(first_root, 4096);

    // Maintenance, once the arena has something worth copying.
    let compacted = client.call(
        &StoreRequest {
            op: store_op::COMPACT,
            ..StoreRequest::zeroed()
        },
        0,
    );
    if let Some(reply) = compacted {
        if reply.status == store_status::OK {
            k2::note(note::COMPACTED, reply.value);
        } else {
            k2::note(note::CLIENT_REFUSED, u64::from(reply.status));
        }
    }

    // What became of every request this client made.
    for asked in 1..=sequence {
        let _ = client.result(asked);
    }
}

fn run_rival(client: &mut Client, config: &ClientConfig) {
    let Some(state) = client.query() else {
        return;
    };
    let generation = state.generation;
    let mut buffer = [0u8; 64];
    let length = content_for(config.leg + 900, &mut buffer);
    let Some(attempt) = candidate(client, generation, b"data", &buffer[..length], true) else {
        return;
    };
    // The same transition the publisher is asking for. One of the two has to
    // lose, and which one is not this program's business.
    let Some(reply) = client.publish(
        1,
        generation,
        attempt.root,
        attempt.policy,
        attempt.validation,
        0,
        attempt.tree,
    ) else {
        return;
    };
    match reply.status {
        store_status::OK => k2::note(note::CLIENT_PUBLISHED, reply.generation),
        store_status::GENERATION_STALE => k2::note(
            report::REFUSED_AS_EXPECTED,
            u64::from(store_status::GENERATION_STALE),
        ),
        other => k2::note(note::CLIENT_REFUSED, u64::from(other)),
    }
    let _ = client.result(1);
}

fn run_reader(client: &mut Client) {
    let Some(state) = client.query() else {
        return;
    };
    k2::note(note::CLIENT_READ, state.generation);
    if state.generation != 0 {
        let _ = client.read_object(state.digest0, 4096);
    }

    // A version nobody published and nobody holds.
    let absent = k4::object_digest(object_type::BYTES, b"never staged");
    if let Some(reply) = client.call(
        &StoreRequest {
            op: store_op::READ,
            digest0: absent,
            arg1: 64,
            ..StoreRequest::zeroed()
        },
        right::MEMORY_WRITE,
    ) {
        if reply.status == store_status::NOT_FOUND {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::NOT_FOUND),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // What became of a request this client never made.
    if let Some(reply) = client.result(1)
        && reply.outcome != result_outcome::NEVER_SEEN
    {
        k2::note(report::NOT_REFUSED, u64::from(reply.outcome));
    }

    // A reader may build a candidate. Publishing it is what it may not do, and
    // the refusal is about publishing rather than about reaching the service.
    let mut buffer = [0u8; 64];
    let length = content_for(777, &mut buffer);
    let attempt = candidate(client, state.generation, b"data", &buffer[..length], true);
    if let Some(attempt) = attempt
        && let Some(reply) = client.publish(
            1,
            state.generation,
            attempt.root,
            attempt.policy,
            attempt.validation,
            0,
            attempt.tree,
        )
    {
        if reply.status == store_status::FORBIDDEN {
            k2::note(note::PUBLISH_FORBIDDEN, u64::from(store_status::FORBIDDEN));
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }

    // Maintenance is not a reader's either.
    if let Some(reply) = client.call(
        &StoreRequest {
            op: store_op::COMPACT,
            ..StoreRequest::zeroed()
        },
        0,
    ) {
        if reply.status == store_status::FORBIDDEN {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::FORBIDDEN),
            );
        } else {
            k2::note(report::NOT_REFUSED, u64::from(reply.status));
        }
    }
}

/// Answers outbox intents on the endpoint the supervisor gave this domain.
///
/// The key is the publication's request identity. The same key twice is one
/// intent seen twice, and the answer says so; a scenario that asks for it gets
/// `UNKNOWN`, which the service has to record as a result rather than retry
/// into a second delivery.
fn run_broker(endpoint: u64, config: &ClientConfig) {
    let mut keys = [[0u8; 32]; BROKER_KEYS];
    let mut attempts = [0u32; BROKER_KEYS];
    let mut held = 0usize;
    let mut idle = 0u32;

    while idle < BROKER_IDLE_ROUNDS {
        let deadline = k2::now_ns() + BROKER_IDLE_NS;
        let Ok((message, invocation)) = k2::endpoint_receive(endpoint, deadline, false) else {
            idle += 1;
            continue;
        };
        idle = 0;
        let Some(intent) = OutboxIntent::read_from(&message.payload, 0) else {
            let answer = OutboxAnswer {
                status: outbox_status::REFUSED,
                attempts: 0,
                reserved0: 0,
            };
            let _ = k2::invocation_reply(invocation, 0, answer.as_bytes());
            continue;
        };
        let slot = match keys.iter().take(held).position(|key| *key == intent.key) {
            Some(slot) => slot,
            None if held < BROKER_KEYS => {
                keys[held] = intent.key;
                held += 1;
                held - 1
            }
            None => 0,
        };
        attempts[slot] += 1;
        // Scenario 3 is the one where the broker cannot say. It is not a
        // failure to answer: it is the answer.
        let status = if config.scenario == 3 {
            outbox_status::UNKNOWN
        } else {
            outbox_status::DELIVERED
        };
        k2::note(note::BROKER_ANSWERED, u64::from(status));
        let answer = OutboxAnswer {
            status,
            attempts: attempts[slot],
            reserved0: 0,
        };
        let _ = k2::invocation_reply(invocation, u64::from(status), answer.as_bytes());
    }
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let config = configuration();
    k2::note(note::CLIENT_ROLE, config.role | (config.principal << 8));

    let done = boot_handle(client_slot::DONE);
    if config.role == role::BROKER {
        run_broker(boot_handle(client_slot::SERVICE), &config);
    } else {
        let mut client = Client {
            store: boot_handle(client_slot::STORE),
            stage_cap: boot_handle(client_slot::STAGE),
            calls: 0,
        };
        match config.role {
            role::PUBLISHER => run_publisher(&mut client, &config),
            role::RIVAL => run_rival(&mut client, &config),
            role::READER => run_reader(&mut client),
            other => k2::note(report::UNEXPECTED, other),
        }
    }

    if let Err(code) = k2::signal_raise(done, bit::CLIENT_DONE) {
        k2::note(report::UNEXPECTED, code as u64);
    }
    k2::note(report::DONE, config.role);
    rt::exit(0)
}

rt::entry!(run);
