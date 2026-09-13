//! K4 user domain `k4store`: the managed state service.
//!
//! The engine that decides what is on the medium is in [`state`]; this is the
//! protocol in front of it, and the two are separate on purpose. The engine
//! knows what a valid prefix is and what a prepare obliges it to; this file
//! knows who asked, what they are allowed to ask for, and what the kernel has
//! to admit before a publication may begin.
//!
//! **The principal is not in the request.** It is the facet the kernel
//! authenticated on the message. A client that wrote another principal's
//! number into its own request would have changed nothing, which is why no
//! field of `StoreRequest` carries one.
//!
//! **Nothing is published on a state the service only assumes.** Every write
//! goes through the block driver, which answers whether it reached the medium;
//! a refusal, a latch or a flush that did not happen stops the publication
//! where it is and leaves the prepare durable and unresolved rather than
//! asserting an outcome for it.
//!
//! **A run that is cut is cut here.** The engine records that a fault point
//! asked for the run to end; only this loop holds the signal that ends it, so
//! the decision and the mechanism stay apart.
//!
//! The protocol, field by field. Everything not named for an operation is
//! zero, and a reply's `status` is the same value the call's result carries.
//!
//! | Operation | Request | Reply |
//! |---|---|---|
//! | `QUERY` | — | `generation`, `digest0` root, `durable_through`, `free_blocks`, `value` epoch |
//! | `PUT_OBJECT` | `arg0` type, `arg1` length, one lent memory capability | `digest0` content digest, `value` length |
//! | `READ` | `digest0` object, `arg0` offset, `arg1` length, one lent memory capability | `value` bytes copied, `detail` type |
//! | `FORK` | `arg0` base generation | `value` workspace, `detail` entries |
//! | `WRITE` | `arg0` workspace, `arg1` name length, `arg2` flags, `name`, `digest0` object | `value` entries |
//! | `FREEZE` | `arg0` workspace, `digest0` policy, `digest1` validation | `digest0` candidate root, `digest1` tree, `value` bytes |
//! | `DISCARD` | `arg0` workspace | `value` workspaces live |
//! | `PUBLISH` | `request_sequence`, `arg0` expected generation, `arg1` outbox target, `digest0` root, `digest1` policy, `digest2` validation, `name` outbox payload digest | `generation`, `digest0` root, `sequence`, `outcome` |
//! | `RESULT` | `request_sequence` | `outcome`, `outbox_status`, `generation`, `digest0` root, `digest1` request identity |
//! | `RETAIN` | `digest0` root, `arg0` expiry | `value` retained |
//! | `RELEASE` | `digest0` root | `value` retained |
//! | `COMPACT` | — | `value` objects copied, `generation` |
//! | `HARNESS` | — | `value` point with mode above bit 8, `sequence` scenario, `generation` leg |
//! | `STATS` | — | counters |

#![no_std]
#![no_main]

mod disk;
mod state;

use thalyx_abi::boot_handle;
use thalyx_abi::generated::{outcome, receipt_kind};
use thalyx_user_k4fmt as k4;
use thalyx_user_k4fmt::pkg::{
    IOBUF_PAGES, OutboxAnswer, OutboxIntent, STORE_CONFIG_VADDR, StoreConfig, bit, facet, note,
    store_slot,
};
use thalyx_user_k4fmt::{
    Binding, Manifest, Pod, StoreReply, StoreRequest, object_type, outbox_status, result_outcome,
    store_op, store_status,
};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report};

use crate::disk::{Disk, iobuf_bytes};
use crate::state::{Fault, MAX_BINDINGS, OBJECT_MAX, Store};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0xD91E_4444_5555_5001;

/// Workspaces the service holds at once. One per principal that can fork, plus
/// one, so a client cannot deny another the ability to start one by holding
/// its own open.
const MAX_WORKSPACES: usize = 5;

/// Objects one principal may have staged and not yet named. A candidate needs
/// three -- its policy, its content and its evidence -- and one spare covers a
/// client that stages a second piece of content before it freezes.
const OFFERED: usize = 4;

/// What a publication reserves for its own closure.
const CLOSURE_RESERVE_NS: u64 = 400_000;

/// The effect kind a publication announces.
const EFFECT_PUBLISH: u32 = 4;

/// How long the service waits for a broker to answer an intent.
const BROKER_DEADLINE_NS: u64 = 2_000_000_000;

/// One slice of a parked service's wait to be replaced.
const PARK_SLICE_NS: u64 = 1_000_000_000;

/// How many slices before a service that was told it would be replaced stops
/// waiting for the supervisor that said so.
const PARK_ROUNDS: u32 = 10;

/// Bytes handed to or taken from a client's lent memory in one call.
const CHUNK: usize = 256;

/// One object's bytes, on their way in or out.
static mut TRANSFER: [u8; OBJECT_MAX] = [0u8; OBJECT_MAX];
/// One tree's canonical bytes, under construction.
static mut TREE: [u8; OBJECT_MAX] = [0u8; OBJECT_MAX];

// As in `state`: the raw form is what the edition requires, and clippy's
// redundant-dereference reading of it is wrong here.
#[allow(clippy::deref_addrof)]
/// The configuration the supervisor wrote and mapped read-only.
fn store_configuration() -> StoreConfig {
    // SAFETY: one page, mapped read-only at this address before this domain
    // ran, holding exactly this structure written by the supervisor.
    let bytes = unsafe { core::slice::from_raw_parts(STORE_CONFIG_VADDR as *const u8, 4096) };
    StoreConfig::read_from(bytes, 0).unwrap_or_default()
}

/// The transfer buffer.
///
/// `&raw mut` then dereference, as in `state.rs`: taking a reference to a
/// `static mut` is what the 2024 edition refuses, and clippy reads the pair as
/// a redundant dereference, which it is not.
#[allow(clippy::deref_addrof)]
fn transfer() -> &'static mut [u8; OBJECT_MAX] {
    // SAFETY: this domain is single-threaded and every caller drops the borrow
    // before the next takes it; no two are alive at once.
    unsafe { &mut *(&raw mut TRANSFER) }
}

/// The tree buffer.
#[allow(clippy::deref_addrof)]
fn tree() -> &'static mut [u8; OBJECT_MAX] {
    // SAFETY: as `transfer`.
    unsafe { &mut *(&raw mut TREE) }
}

/// A private workspace over a version: bindings that are not a version yet.
#[derive(Clone, Copy)]
struct Workspace {
    live: bool,
    owner: u64,
    base_generation: u64,
    count: usize,
    bindings: [Binding; MAX_BINDINGS],
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            live: false,
            owner: 0,
            base_generation: 0,
            count: 0,
            bindings: [Binding::default(); MAX_BINDINGS],
        }
    }
}

impl Workspace {
    /// Binds `name`, keeping the entries in the order a tree is canonical in.
    ///
    /// Order is maintained here rather than sorted at freeze time because the
    /// encoder refuses unordered entries: a caller that hands over a name out
    /// of order has a bug about what a version is, and sorting it away would
    /// change the digest of what it published without telling it.
    fn bind(&mut self, binding: Binding) -> Result<usize, u32> {
        let mut at = self.count;
        for index in 0..self.count {
            match self.bindings[index].name().cmp(binding.name()) {
                core::cmp::Ordering::Equal => {
                    self.bindings[index] = binding;
                    return Ok(self.count);
                }
                core::cmp::Ordering::Greater => {
                    at = index;
                    break;
                }
                core::cmp::Ordering::Less => {}
            }
        }
        if self.count == MAX_BINDINGS {
            return Err(store_status::EXHAUSTED);
        }
        let mut slot = self.count;
        while slot > at {
            self.bindings[slot] = self.bindings[slot - 1];
            slot -= 1;
        }
        self.bindings[at] = binding;
        self.count += 1;
        Ok(self.count)
    }
}

/// Whether a principal may ask for this operation at all.
///
/// This is authority the package assigns, before any policy on the medium
/// narrows it further. A reader may build a candidate — that is what makes the
/// refusal below about publishing rather than about reaching the service.
fn permitted(principal: u64, op: u32) -> bool {
    match op {
        store_op::PUBLISH => may_publish(principal),
        store_op::COMPACT => principal == facet::PUBLISHER,
        _ => principal != 0,
    }
}

/// Which principals the package lets publish.
///
/// K4's package names two, and the reader's refusal is one of its criteria.
/// Under `every-principal-publishes` every bound principal may, which is what
/// a service shared by several independent consumers needs; the policy object
/// of each publication still narrows it further, and the facet is still the
/// only identity.
#[cfg(not(feature = "every-principal-publishes"))]
fn may_publish(principal: u64) -> bool {
    principal == facet::PUBLISHER || principal == facet::RIVAL
}

#[cfg(feature = "every-principal-publishes")]
fn may_publish(principal: u64) -> bool {
    principal != 0 && (principal as usize) < state::MAX_PRINCIPALS
}

/// A reply carrying nothing but a status.
fn refused(status: u32) -> StoreReply {
    StoreReply {
        status,
        ..StoreReply::zeroed()
    }
}

/// Copies `length` bytes out of a capability a client lent, in bounded pieces.
fn take_from(memory: u64, length: usize, out: &mut [u8]) -> bool {
    if length > out.len() {
        return false;
    }
    let mut at = 0usize;
    while at < length {
        let piece = (length - at).min(CHUNK);
        if k2::memory_read(memory, at as u64, &mut out[at..at + piece]).is_err() {
            return false;
        }
        at += piece;
    }
    true
}

/// Copies `length` bytes into a capability a client lent, in bounded pieces.
fn give_to(memory: u64, bytes: &[u8]) -> bool {
    let mut at = 0usize;
    while at < bytes.len() {
        let piece = (bytes.len() - at).min(CHUNK);
        if k2::memory_write(memory, at as u64, &bytes[at..at + piece]).is_err() {
            return false;
        }
        at += piece;
    }
    true
}

/// The whole service.
struct Service {
    store: Store,
    workspaces: [Workspace; MAX_WORKSPACES],
    /// The candidate each principal last froze and has not yet published. A
    /// freeze and the publication that names it are two calls, and between
    /// them the only thing keeping the candidate's objects staged is this.
    candidates: [[u8; 32]; state::MAX_PRINCIPALS],
    /// The last few objects each principal staged and has not yet named in a
    /// candidate. A policy and a piece of evidence are staged in one call and
    /// named in another, and between the two nothing else refers to them: a
    /// sweep in the middle would leave the candidate naming objects that are
    /// gone, which the service would then have to refuse as `NOT_FOUND` for a
    /// publication that was never wrong.
    offered: [[[u8; 32]; OFFERED]; state::MAX_PRINCIPALS],
    offered_next: [usize; state::MAX_PRINCIPALS],
    /// Set when serving a request already discharged the invocation, so the
    /// loop does not answer twice. An invocation resolved without a reply is
    /// answered; replying afterwards is refused, and the refusal would be the
    /// service reporting a fault of its own making.
    discharged: bool,
    log: u64,
    broker: u64,
    crash: u64,
    answered: u64,
    control_lost: u64,
}

impl Service {
    /// Appends one diagnostic note of this service to its control plane, and
    /// only while the plane has ordinary room for it.
    ///
    /// A note says what this service did, for a reader that drains the log. It
    /// is not the evidence of anything: the receipts the kernel writes are. So
    /// when nobody is draining and the log is full, writing is not a way to be
    /// heard -- the note cannot be kept, and the attempt would make the kernel
    /// declare a hole in coverage that the work itself did not cause. What such
    /// a run has to show is the refusal of the admissions the receipts covered,
    /// not this.
    fn service_note(&self, code: u64, value: u64) {
        let room = k2::log_query(self.log).map_or(0, |info| {
            info.capacity
                .saturating_sub(info.reserved_cells)
                .saturating_sub(info.used)
        });
        if room > 0 {
            let _ = k2::log_append(self.log, receipt_kind::SERVICE_NOTE, code, value, 0);
        }
    }

    /// Tells the engine which staged digests are still nameable.
    ///
    /// Two things are: what a live workspace has bound, which is content
    /// staged and not yet inside any tree, and the candidate a principal
    /// froze and has not published. Everything else staged is the residue of
    /// something nobody is going to finish.
    fn remember(&mut self, principal: u64, digest: [u8; 32]) {
        let Some(slot) = usize::try_from(principal)
            .ok()
            .filter(|index| *index < state::MAX_PRINCIPALS)
        else {
            return;
        };
        let at = self.offered_next[slot];
        self.offered[slot][at] = digest;
        self.offered_next[slot] = (at + 1) % OFFERED;
    }

    fn protect(&mut self) {
        self.store.unpin();
        let mut count = 0usize;
        let mut push = |digest: [u8; 32], store: &mut Store| {
            if digest == [0u8; 32] || count == state::MAX_PROTECTED {
                return;
            }
            if store.protected[..count].contains(&digest) {
                return;
            }
            store.protected[count] = digest;
            count += 1;
        };
        for index in 0..state::MAX_PRINCIPALS {
            push(self.candidates[index], &mut self.store);
            for offered in self.offered[index] {
                push(offered, &mut self.store);
            }
        }
        for space in &self.workspaces {
            if !space.live {
                continue;
            }
            for binding in &space.bindings[..space.count] {
                push(binding.digest, &mut self.store);
            }
        }
        self.store.protected_count = count;
    }

    /// Whether `target` is reachable from a root this store still serves.
    ///
    /// Reading is bounded by what the store publishes or has been asked to
    /// hold: an object that no served version names is not readable because it
    /// happens to still be on the medium.
    fn served(&mut self, target: &[u8; 32]) -> bool {
        let mut roots = [[0u8; 32]; state::MAX_RETAINED + 1];
        let mut count = 0usize;
        if self.store.published_generation != 0 {
            roots[count] = self.store.published_root;
            count += 1;
        }
        for index in 0..self.store.retained_count {
            roots[count] = self.store.retained[index].root_digest;
            count += 1;
        }
        let mut reachable = [0usize; state::MAX_OBJECTS];
        for root in roots.iter().take(count) {
            if root == target {
                return true;
            }
            let mut found = 0usize;
            if !self.store.closure(root, &mut reachable, &mut found) {
                continue;
            }
            if reachable[..found]
                .iter()
                .any(|index| self.store.objects[*index].digest == *target)
            {
                return true;
            }
        }
        false
    }

    /// A free workspace slot, or none.
    fn free_workspace(&self) -> Option<usize> {
        self.workspaces.iter().position(|space| !space.live)
    }

    /// The workspace `id` names, if this principal owns it.
    fn workspace(&mut self, id: u64, principal: u64) -> Option<usize> {
        let index = (id as usize).checked_sub(1)?;
        if index >= MAX_WORKSPACES {
            return None;
        }
        let space = &self.workspaces[index];
        if !space.live || space.owner != principal {
            return None;
        }
        Some(index)
    }

    /// Asks the broker what became of an intent, and records the answer.
    ///
    /// What is recorded is what the broker said. `UNKNOWN` is written down as
    /// `UNKNOWN`: an attempt that neither succeeded nor is known to have
    /// failed is a result, and turning it into either would be the service
    /// inventing one.
    fn resolve_outbox(
        &mut self,
        principal: u64,
        sequence: u64,
        request: [u8; 32],
        target: u64,
        generation: u64,
    ) -> u32 {
        let intent = OutboxIntent {
            target,
            principal,
            request_sequence: sequence,
            generation,
            key: request,
        };
        let answer = match k2::endpoint_call(
            self.broker,
            0,
            intent.as_bytes(),
            &[],
            k2::now_ns() + BROKER_DEADLINE_NS,
            false,
        ) {
            Ok(result) => OutboxAnswer::read_from(&result.payload, 0).unwrap_or(OutboxAnswer {
                status: outbox_status::UNKNOWN,
                attempts: 1,
                reserved0: 0,
            }),
            Err(_) => OutboxAnswer {
                status: outbox_status::UNKNOWN,
                attempts: 1,
                reserved0: 0,
            },
        };
        if self.store.fault_at(k4::fault_point::AFTER_OUTBOX_SEND, 0) != Fault::None {
            // The answer arrived and the record of it did not: the run is cut
            // between the two, which is the case a retry has to survive.
            return answer.status;
        }
        if !self.store.record_outbox(
            principal,
            sequence,
            request,
            target,
            answer.status,
            answer.attempts,
        ) {
            return outbox_status::UNKNOWN;
        }
        answer.status
    }

    /// Carries out one request from `principal`.
    fn serve(
        &mut self,
        principal: u64,
        invocation: u64,
        request: &StoreRequest,
        lent: u64,
    ) -> StoreReply {
        if !permitted(principal, request.op) {
            if request.op == store_op::PUBLISH {
                k2::note(note::PUBLISH_FORBIDDEN, store_status::FORBIDDEN as u64);
            }
            return refused(store_status::FORBIDDEN);
        }
        if self.store.integrity_failed {
            k2::note(
                note::INTEGRITY_REFUSED,
                store_status::INTEGRITY_FAILED as u64,
            );
            return refused(store_status::INTEGRITY_FAILED);
        }
        if !self.store.ready && request.op != store_op::HARNESS {
            return refused(store_status::UNAVAILABLE);
        }
        // What must survive a sweep, decided here because the engine does not
        // know what a workspace is and should not be taught.
        self.protect();
        self.discharged = false;
        match request.op {
            store_op::QUERY => StoreReply {
                status: store_status::OK,
                generation: self.store.published_generation,
                value: self.store.store_epoch,
                digest0: self.store.published_root,
                digest1: self.store.published_policy,
                sequence: self.store.next_sequence,
                durable_through: self.store.durable_through,
                free_blocks: self.store.free_blocks(),
                ..StoreReply::zeroed()
            },
            store_op::PUT_OBJECT => self.put_object(principal, request, lent),
            store_op::READ => self.read(request, lent),
            store_op::FORK => self.fork(principal, request),
            store_op::WRITE => self.write(principal, request),
            store_op::FREEZE => self.freeze(principal, request),
            store_op::DISCARD => self.discard(principal, request),
            store_op::PUBLISH => self.publish(principal, invocation, request),
            store_op::RESULT => self.result(principal, request),
            store_op::RETAIN => match self.store.retain(request.digest0, request.arg0) {
                Ok(count) => StoreReply {
                    status: store_status::OK,
                    value: count,
                    ..StoreReply::zeroed()
                },
                Err(status) => refused(status),
            },
            store_op::RELEASE => match self.store.release(request.digest0) {
                Ok(count) => StoreReply {
                    status: store_status::OK,
                    value: count,
                    ..StoreReply::zeroed()
                },
                Err(status) => refused(status),
            },
            store_op::COMPACT => match self.store.compact() {
                Ok(copied) => StoreReply {
                    status: store_status::OK,
                    value: copied,
                    generation: self.store.published_generation,
                    digest0: self.store.published_root,
                    durable_through: self.store.durable_through,
                    free_blocks: self.store.free_blocks(),
                    ..StoreReply::zeroed()
                },
                Err(status) => refused(status),
            },
            store_op::HARNESS => StoreReply {
                status: store_status::OK,
                value: u64::from(self.store.harness.point)
                    | (u64::from(self.store.harness.mode) << 8)
                    | (self.store.harness.arg << 16),
                generation: self.store.harness.leg,
                sequence: self.store.harness.scenario,
                detail: u32::from(self.store.harness.present),
                ..StoreReply::zeroed()
            },
            store_op::STATS => {
                // The medium's own counters, asked for rather than remembered,
                // so a gate can hold the service's account of what it wrote
                // against the driver's account of what it issued.
                let medium = self.store.disk.stats();
                let issued = medium.map(|reply| reply.writes_issued).unwrap_or(0);
                let suppressed = medium.map(|reply| reply.writes_suppressed).unwrap_or(0);
                StoreReply {
                    status: store_status::OK,
                    value: self.store.publications,
                    detail: self.store.refusals as u32,
                    generation: self.store.published_generation,
                    sequence: self.store.scanned,
                    durable_through: self.store.durable_through,
                    free_blocks: self.store.free_blocks(),
                    digest0: self.store.published_root,
                    outcome: self.store.recovery_aborts as u32,
                    outbox_status: self.store.compactions as u32,
                    digest1: self.store.published_policy,
                    reserved0: issued | (suppressed << 32),
                }
            }
            _ => refused(store_status::INVALID_REQUEST),
        }
    }

    fn put_object(&mut self, principal: u64, request: &StoreRequest, lent: u64) -> StoreReply {
        let length = request.arg1 as usize;
        if length > OBJECT_MAX
            || request.arg0 == 0
            || request.arg0 > u64::from(object_type::RECEIPT)
        {
            return refused(store_status::TOO_LARGE);
        }
        if lent == 0 {
            return refused(store_status::INVALID_REQUEST);
        }
        let bytes = transfer();
        if !take_from(lent, length, &mut bytes[..]) {
            return refused(store_status::INVALID_REQUEST);
        }
        match self.store.stage(request.arg0 as u32, &bytes[..length]) {
            Ok(digest) => {
                // Remembered as this principal's, so a sweep between staging it
                // and naming it in a candidate cannot take it away.
                self.remember(principal, digest);
                StoreReply {
                    status: store_status::OK,
                    value: length as u64,
                    digest0: digest,
                    free_blocks: self.store.free_blocks(),
                    ..StoreReply::zeroed()
                }
            }
            Err(status) => refused(status as u32),
        }
    }

    fn read(&mut self, request: &StoreRequest, lent: u64) -> StoreReply {
        if !self.served(&request.digest0) {
            // Two different answers, and conflating them is the mistake this
            // distinction exists to stop. `NOT_FOUND` says the store has
            // looked and this object is not part of anything it serves;
            // `UNAVAILABLE` says the store could not look, because reading the
            // medium was refused. Answering the first when the second is true
            // tells a caller that a version it published does not exist.
            if self.store.closure_incomplete {
                return refused(store_status::UNAVAILABLE);
            }
            return refused(store_status::NOT_FOUND);
        }
        let bytes = transfer();
        let Some((kind, length)) = self.store.read_object(&request.digest0, &mut bytes[..]) else {
            return refused(store_status::INTEGRITY_FAILED);
        };
        let offset = request.arg0 as usize;
        if offset > length {
            return refused(store_status::INVALID_REQUEST);
        }
        let want = (request.arg1 as usize).min(length - offset);
        if lent == 0 {
            return refused(store_status::INVALID_REQUEST);
        }
        if !give_to(lent, &bytes[offset..offset + want]) {
            return refused(store_status::FORBIDDEN);
        }
        StoreReply {
            status: store_status::OK,
            detail: kind,
            value: want as u64,
            generation: self.store.published_generation,
            sequence: length as u64,
            ..StoreReply::zeroed()
        }
    }

    fn fork(&mut self, principal: u64, request: &StoreRequest) -> StoreReply {
        if request.arg0 != self.store.published_generation {
            return refused(store_status::GENERATION_STALE);
        }
        let Some(slot) = self.free_workspace() else {
            // Which principals are holding them, because "no workspace" says
            // nothing about whose it is and a leak is always somebody's.
            let mut owners = 0u64;
            for (index, space) in self.workspaces.iter().enumerate() {
                owners |= (space.owner & 0xF) << (index * 4);
            }
            k2::note(note::WORKSPACES_EXHAUSTED, owners);
            return refused(store_status::EXHAUSTED);
        };
        let mut space = Workspace {
            live: true,
            owner: principal,
            base_generation: request.arg0,
            count: 0,
            bindings: [Binding::default(); MAX_BINDINGS],
        };
        if self.store.published_generation != 0 {
            let root = self.store.published_root;
            let bytes = transfer();
            let Some((kind, length)) = self.store.read_object(&root, &mut bytes[..]) else {
                return refused(store_status::INTEGRITY_FAILED);
            };
            if kind != object_type::MANIFEST {
                return refused(store_status::INTEGRITY_FAILED);
            }
            let Some(manifest) = Manifest::read_from(&bytes[..length], 0) else {
                return refused(store_status::INTEGRITY_FAILED);
            };
            let bytes = transfer();
            let Some((kind, length)) = self
                .store
                .read_object(&manifest.tree_digest, &mut bytes[..])
            else {
                return refused(store_status::INTEGRITY_FAILED);
            };
            if kind != object_type::TREE {
                return refused(store_status::INTEGRITY_FAILED);
            }
            match k4::decode_tree(&bytes[..length], &mut space.bindings[..]) {
                Ok(count) => space.count = count,
                Err(_) => return refused(store_status::INTEGRITY_FAILED),
            }
        }
        self.workspaces[slot] = space;
        StoreReply {
            status: store_status::OK,
            value: (slot + 1) as u64,
            detail: space.count as u32,
            generation: request.arg0,
            ..StoreReply::zeroed()
        }
    }

    fn write(&mut self, principal: u64, request: &StoreRequest) -> StoreReply {
        let Some(slot) = self.workspace(request.arg0, principal) else {
            return refused(store_status::NOT_FOUND);
        };
        let name_len = request.arg1 as usize;
        if name_len == 0 || name_len > request.name.len() {
            return refused(store_status::INVALID_REQUEST);
        }
        let Some(index) = self.store.find(&request.digest0) else {
            return refused(store_status::NOT_FOUND);
        };
        let entry = self.store.objects[index];
        let binding = match Binding::new(
            &request.name[..name_len],
            entry.object_type,
            request.arg2 as u32,
            u64::from(entry.length),
            entry.digest,
        ) {
            Ok(binding) => binding,
            Err(_) => return refused(store_status::INVALID_REQUEST),
        };
        match self.workspaces[slot].bind(binding) {
            Ok(count) => StoreReply {
                status: store_status::OK,
                value: count as u64,
                ..StoreReply::zeroed()
            },
            Err(status) => refused(status),
        }
    }

    fn freeze(&mut self, principal: u64, request: &StoreRequest) -> StoreReply {
        let Some(slot) = self.workspace(request.arg0, principal) else {
            return refused(store_status::NOT_FOUND);
        };
        let space = self.workspaces[slot];
        let bytes = tree();
        let written = match k4::encode_tree(&mut bytes[..], &space.bindings[..space.count]) {
            Ok(written) => written,
            Err(_) => return refused(store_status::INVALID_REQUEST),
        };
        let tree_digest = match self.store.stage(object_type::TREE, &bytes[..written]) {
            Ok(digest) => digest,
            Err(status) => return refused(status as u32),
        };
        let total: u64 = space.bindings[..space.count]
            .iter()
            .map(|binding| binding.length)
            .sum();
        let manifest = Manifest {
            format: 1,
            reserved0: 0,
            tree_digest,
            policy_digest: request.digest0,
            validation_digest: request.digest1,
            entry_count: space.count as u64,
            total_bytes: total,
        };
        match self.store.stage(object_type::MANIFEST, manifest.as_bytes()) {
            Ok(root) => {
                if (principal as usize) < state::MAX_PRINCIPALS {
                    self.candidates[principal as usize] = root;
                }
                StoreReply {
                    status: store_status::OK,
                    digest0: root,
                    digest1: tree_digest,
                    value: total,
                    generation: space.base_generation,
                    free_blocks: self.store.free_blocks(),
                    ..StoreReply::zeroed()
                }
            }
            Err(status) => refused(status as u32),
        }
    }

    fn discard(&mut self, principal: u64, request: &StoreRequest) -> StoreReply {
        let Some(slot) = self.workspace(request.arg0, principal) else {
            return refused(store_status::NOT_FOUND);
        };
        self.workspaces[slot] = Workspace::default();
        StoreReply {
            status: store_status::OK,
            value: self.workspaces.iter().filter(|space| space.live).count() as u64,
            ..StoreReply::zeroed()
        }
    }

    fn result(&mut self, principal: u64, request: &StoreRequest) -> StoreReply {
        let Some(slot) = self.store.principal_slot(principal) else {
            return refused(store_status::FORBIDDEN);
        };
        let entry = self.store.principals[slot];
        let sequence = request.request_sequence;
        if entry.pending_sequence == sequence && sequence != 0 {
            // Durably prepared and not resolved. Neither outcome may be
            // asserted, and saying so is the answer rather than a failure to
            // produce one.
            k2::note(
                note::RESULT_ANSWERED,
                u64::from(result_outcome::UNKNOWN) | (sequence << 8),
            );
            return StoreReply {
                status: store_status::OK,
                outcome: result_outcome::UNKNOWN,
                sequence,
                ..StoreReply::zeroed()
            };
        }
        if sequence > entry.high_water {
            k2::note(
                note::RESULT_ANSWERED,
                u64::from(result_outcome::NEVER_SEEN) | (sequence << 8),
            );
            return StoreReply {
                status: store_status::OK,
                outcome: result_outcome::NEVER_SEEN,
                sequence,
                ..StoreReply::zeroed()
            };
        }
        match self.store.result_for(principal, sequence) {
            Some(held) => {
                k2::note(
                    note::RESULT_ANSWERED,
                    u64::from(held.outcome) | (sequence << 8),
                );
                StoreReply {
                    status: store_status::OK,
                    outcome: held.outcome,
                    outbox_status: held.outbox_status,
                    generation: held.generation,
                    digest0: held.root_digest,
                    digest1: held.request_digest,
                    sequence,
                    ..StoreReply::zeroed()
                }
            }
            None => {
                k2::note(
                    note::RESULT_ANSWERED,
                    u64::from(result_outcome::RESULT_EXPIRED) | (sequence << 8),
                );
                StoreReply {
                    status: store_status::RESULT_EXPIRED,
                    outcome: result_outcome::RESULT_EXPIRED,
                    sequence,
                    ..StoreReply::zeroed()
                }
            }
        }
    }

    fn publish(&mut self, principal: u64, invocation: u64, request: &StoreRequest) -> StoreReply {
        let identity = match self.store.admit(
            principal,
            request.request_sequence,
            request.arg0,
            &request.digest0,
            &request.digest1,
            &request.digest2,
            request.arg1,
            &request.name,
        ) {
            Ok(identity) => identity,
            Err(store_status::OK) => {
                // The same request, again. The answer is the one already
                // durable: doing it twice would produce a second generation
                // for one request.
                let held = self.store.result_for(principal, request.request_sequence);
                return match held {
                    Some(entry) => StoreReply {
                        status: store_status::OK,
                        outcome: entry.outcome,
                        outbox_status: entry.outbox_status,
                        generation: entry.generation,
                        digest0: entry.root_digest,
                        digest1: entry.request_digest,
                        sequence: request.request_sequence,
                        ..StoreReply::zeroed()
                    },
                    None => refused(store_status::RESULT_EXPIRED),
                };
            }
            Err(status) => {
                self.store.refusals += 1;
                if status == store_status::CONFLICT {
                    k2::note(note::CONFLICT_REFUSED, request.request_sequence);
                }
                k2::note(note::PUBLISH_REFUSED, u64::from(status));
                return refused(status);
            }
        };

        // The kernel decides whether a publication may begin. A barrier that
        // won the race here is the difference between refusing to start and
        // starting something nobody is left to answer.
        if let Err(code) =
            k2::invocation_begin_effect(invocation, EFFECT_PUBLISH, CLOSURE_RESERVE_NS)
        {
            k2::note(note::EFFECT_REFUSED, code as u64);
            self.store.refusals += 1;
            return refused(store_status::EFFECT_REFUSED);
        }
        k2::note(note::EFFECT_ADMITTED, invocation);
        // Before the publication: this one says the service began something,
        // which is what the effect admission covers and what a reader needs to
        // see a publication that did not finish.
        self.service_note(0x4B34_0001, request.request_sequence);

        match self.store.publish(
            principal,
            request.request_sequence,
            request.arg0,
            request.digest0,
            request.digest1,
            request.digest2,
            identity,
            request.arg1,
            request.name,
            invocation,
        ) {
            Ok((generation, after)) => {
                let mut status = outbox_status::NONE;
                if request.arg1 != 0 {
                    status = self.resolve_outbox(
                        principal,
                        request.request_sequence,
                        identity,
                        request.arg1,
                        generation,
                    );
                }
                // No resolve here. Resolving discharges the invocation, and a
                // discharged invocation carries no payload: the caller would
                // be woken with `OK` and an empty reply, learning that
                // something committed but not which version. The reply is
                // itself a commit -- the kernel marks the outcome `COMMITTED`
                // when a responder answers -- so answering is the discharge,
                // and it is the only discharge that carries the answer.
                self.service_note(0x4B34_0002, generation);
                if after == Fault::LoseResponse {
                    self.store.demanded = Some(Fault::LoseResponse);
                }
                StoreReply {
                    status: store_status::OK,
                    outcome: result_outcome::COMMITTED,
                    outbox_status: status,
                    generation,
                    digest0: request.digest0,
                    digest1: identity,
                    sequence: request.request_sequence,
                    durable_through: self.store.durable_through,
                    free_blocks: self.store.free_blocks(),
                    ..StoreReply::zeroed()
                }
            }
            Err(status) => {
                // The prepare may be durable and unresolved. The effect is
                // resolved as UNKNOWN rather than aborted, because whether it
                // happened is exactly what this run cannot say.
                self.store.refusals += 1;
                let ended = self.store.pending.live;
                let _ = k2::invocation_resolve(
                    invocation,
                    if ended {
                        outcome::UNKNOWN
                    } else {
                        outcome::ABORTED
                    },
                    u64::from(status),
                );
                // Resolving is a discharge, so this request is answered and
                // the loop must not answer it again. What the caller gets is
                // the outcome without a payload, which is the honest shape of
                // "this did not commit, and asking me again will not tell you
                // more than the durable result will".
                self.discharged = true;
                k2::note(note::PUBLISH_REFUSED, u64::from(status));
                StoreReply {
                    status,
                    outcome: if ended {
                        result_outcome::UNKNOWN
                    } else {
                        result_outcome::ABORTED
                    },
                    sequence: request.request_sequence,
                    ..StoreReply::zeroed()
                }
            }
        }
    }

    /// Acts on a fault that asked for the run to end or the service to be
    /// replaced. Returns false when this domain must not answer.
    fn honour_demand(&mut self, invocation: u64, reply: &StoreReply) -> bool {
        let Some(demand) = self.store.demanded.take() else {
            return true;
        };
        match demand {
            Fault::Stop => {
                // Nothing further may reach the medium between deciding to end
                // the run and it ending.
                self.store.disk.latch();
                let _ = k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes());
                let _ = k2::signal_raise(self.crash, bit::CRASH);
                self.stop()
            }
            Fault::LoseResponse => {
                // The caller is told nothing at all. What it must not be told
                // is a result: an answer that never arrived is the case the
                // next leg has to survive, and replying would remove it.
                self.store.disk.latch();
                let _ = k2::signal_raise(self.crash, bit::CRASH);
                self.stop()
            }
            Fault::Kill => {
                let _ = k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes());
                let _ = k2::signal_raise(self.crash, bit::RESTART);
                self.park()
            }
            _ => true,
        }
    }

    /// Ends this service where it stands, having latched the medium first.
    ///
    /// A cut service that stays alive waiting is indistinguishable from a
    /// working one to everything except the medium, and it keeps the machine
    /// from winding down: a domain in a timed wait is a domain making
    /// progress, so nothing ever decides the run is over and the leg has to be
    /// killed from outside. Ending here is not a tidier crash -- the writes
    /// are already latched, so nothing this domain could still do would reach
    /// the medium -- and the caller learns what a caller learns when a service
    /// disappears, which is nothing.
    fn stop(&mut self) -> ! {
        k2::note(note::SERVICE_STOPPED, self.store.disk.writes_issued);
        rt::exit(0)
    }

    /// Waits to be replaced, without touching the medium again.
    ///
    /// The wait is on a bit nothing raises. Waiting on `RESTART` would consume
    /// the request this service just made of its supervisor, before the
    /// supervisor could see it, and a service that asks to be replaced and
    /// then eats its own request is never replaced.
    ///
    /// Bounded, because the only thing that ends this wait properly is the
    /// supervisor terminating the domain, and a service that waits forever for
    /// a supervisor that never comes is the hang this bound exists to refuse.
    fn park(&mut self) -> ! {
        for _ in 0..PARK_ROUNDS {
            let _ = k2::signal_wait(self.crash, bit::NEVER, k2::now_ns() + PARK_SLICE_NS);
        }
        self.stop()
    }
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let disk_endpoint = boot_handle(store_slot::DISK);
    let service = boot_handle(store_slot::SERVICE);
    let log = boot_handle(store_slot::LOG);
    let broker = boot_handle(store_slot::BROKER);
    let crash = boot_handle(store_slot::CRASH);

    // The guest's encoders against the golden bytes the host produced from the
    // same schema with a different implementation. A disagreement here is a
    // disagreement about the format, and there is no point writing a medium
    // the gate would decode differently.
    // The buffer this domain assumes is mapped, confirmed against the object
    // itself rather than assumed from the address it was mapped at. The handle
    // is closed afterwards: it has no further use, and a handle kept is a grant
    // nothing else can have.
    let iobuf = boot_handle(store_slot::IOBUF);
    match k2::memory_query(iobuf) {
        Ok(info) if info.pages >= IOBUF_PAGES => {
            k2::note(report::MAPPED, info.pages);
        }
        Ok(info) => {
            k2::note(report::UNEXPECTED, info.pages);
            rt::exit(1)
        }
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(1)
        }
    }
    let _ = k2::cap_close(iobuf);

    let report = k4::verify_golden(iobuf_bytes());
    k2::note(
        note::GOLDEN_VERIFIED,
        u64::from(report.checked) | (u64::from(report.failed) << 32),
    );
    if report.failed != 0 {
        rt::exit(1)
    }

    let configuration = store_configuration();
    k2::note(
        note::SUPER_BUILT,
        configuration.instance | (configuration.leg << 8) | (configuration.scenario << 16),
    );

    let mut store = Store::new(Disk::new(disk_endpoint));
    // The service's own view of the plane that records what it does, so it can
    // decline to write what it could not then account for.
    store.control_log = log;
    let mut service_state = Service {
        store,
        workspaces: [Workspace::default(); MAX_WORKSPACES],
        candidates: [[0u8; 32]; state::MAX_PRINCIPALS],
        offered: [[[0u8; 32]; OFFERED]; state::MAX_PRINCIPALS],
        offered_next: [0usize; state::MAX_PRINCIPALS],
        discharged: false,
        log,
        broker,
        crash,
        answered: 0,
        control_lost: 0,
    };

    // A replacement service does not re-apply the directive the run it
    // replaced was cut by. It is told which it is; working it out from the
    // medium would be the harness leaking into the store.
    if configuration.apply_directive != 0 {
        service_state.store.read_directive();
    }
    let seed = if service_state.store.harness.present {
        service_state.store.harness.seed
    } else {
        configuration.scenario
    };
    if !service_state.store.recover(seed) {
        // Refusing to serve is the answer when the prefix that was supposed to
        // be durable is not there. The run continues so the gate can see the
        // refusal rather than an absence.
        k2::note(
            note::INTEGRITY_REFUSED,
            store_status::INTEGRITY_FAILED as u64,
        );
    }
    k2::note(note::STORE_READY, service_state.store.published_generation);
    if let Err(code) = k2::signal_raise(crash, bit::READY) {
        k2::note(report::UNEXPECTED, code as u64);
    }

    loop {
        let (message, invocation) = match k2::endpoint_receive(service, 0, false) {
            Ok(pair) => pair,
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                rt::exit(2)
            }
        };
        let principal = message.header.facet;
        let request = match StoreRequest::read_from(&message.payload, 0) {
            Some(request) if message.header.payload_len as usize >= StoreRequest::SIZE => request,
            _ => {
                let reply = refused(store_status::INVALID_REQUEST);
                let _ = k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes());
                let _ = k2::cap_close(invocation);
                continue;
            }
        };
        // A lent capability is authority the caller passed, not a number it
        // wrote: it is taken from the message rather than from the request,
        // which is why no field of the request could name one.
        let lent = if message.cap_count > 0 {
            message.caps[0]
        } else {
            0
        };

        let reply = service_state.serve(principal, invocation, &request, lent);
        service_state.answered += 1;
        // The handle the caller lent is charged to this domain's table until
        // it is closed, and a service that kept one per call would run out of
        // table rather than out of store.
        if lent != 0 {
            let _ = k2::cap_close(lent);
        }

        // The control plane's own coverage. A service that reported receipts
        // it never had would be claiming more than the log promises.
        if let Ok(info) = k2::log_query(log)
            && u64::from(info.lost) > service_state.control_lost
        {
            service_state.control_lost = u64::from(info.lost);
            k2::note(note::CONTROL_INCOMPLETE, service_state.control_lost);
        }

        // The ticket outlives the reply on purpose -- an effect is admitted
        // against it and resolved after the answer -- but not the request. A
        // service that kept one per call would hold an invocation record for
        // every request it had already answered, and the machine's table, not
        // the medium, is what would run out first.
        let answered = service_state.honour_demand(invocation, &reply) && !service_state.discharged;
        if answered
            && let Err(code) =
                k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes())
        {
            k2::note(report::UNEXPECTED, code as u64);
        }
        let _ = k2::cap_close(invocation);
    }
}

rt::entry!(run);
