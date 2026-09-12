//! The kernel interface: dispatch, validation and the admission point.
//!
//! Every operation goes through the same three steps in the same order, and the
//! order is the contract's, not a convenience:
//!
//! 1. **Structure.** The operation code is looked up in the table the ABI
//!    schema generates. An unassigned number is refused, not ignored. The
//!    descriptor is copied once into kernel memory and then validated there:
//!    version, declared length against the register length, opcode against the
//!    operation, unknown flag bits, reserved words. A caller that keeps writing
//!    its buffer cannot change what was validated.
//! 2. **Authority.** The handle is resolved in the calling domain's own table,
//!    the object's type is checked against the operation, the grant's rights
//!    against the operation's requirement, and then the whole lineage is walked
//!    for barriers, deadlines and closed scopes. Rights can be read from one
//!    node because derivation only ever narrows them; liveness cannot, because a
//!    fence anywhere above has to stop the child.
//! 3. **Effect.** Everything that has to be reserved is reserved, and only then
//!    does anything become visible. A refusal at any point leaves no partial
//!    object, no partial transfer and no charge behind.
//!
//! Steps 2 and 3 happen inside one hold of the machine lock, which a fence also
//! takes. That is what makes the authority contract's admission point real: an
//! admission either wins the race and is recorded before the barrier, or loses
//! it and fails. Validating first and publishing later, outside one critical
//! section, is exactly the tempting separation the K2 groundwork identifies as
//! wrong.

pub mod capops;
pub mod devops;
pub mod domainops;
pub mod evtops;
pub mod ipcops;
pub mod logops;
pub mod memops;
pub mod scopeops;

use core::sync::atomic::{AtomicU64, Ordering};

use thalyx_abi::generated::{DescriptorHeader, OPERATIONS, OpSpec, flag, object_type, op, status};

use crate::arch::x86_64::trap::TrapFrame;
use crate::obj::{GrantId, NO_GRANT, ObjKind, ObjRef, ScopeId};
use crate::scope;
use crate::state::{MACHINE, Machine};
use crate::trace;
use crate::ucopy::{self, Staging};

/// Diagnostic records of refused operations emitted per domain before the
/// plane starts coalescing them into a counter.
pub const REFUSAL_RECORD_LIMIT: u32 = 64;

/// A resolved capability: what the handle named and what it still permits.
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    /// Table slot the handle named.
    pub slot: usize,
    /// Object the capability names.
    pub object: ObjRef,
    /// Grant carrying the rights and the lifetime.
    pub grant: GrantId,
    /// Effective rights of that grant.
    pub rights: u32,
    /// Immutable facet the grant carries, or zero.
    pub facet: u64,
}

/// Everything an operation handler needs about its caller.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// Calling domain.
    pub domain: usize,
    /// Calling thread.
    pub thread: usize,
    /// Handle the caller passed.
    pub handle: u64,
    /// Operation code.
    pub operation: u32,
    /// Flags register, already checked against the known bits.
    pub flags: u64,
    /// Monotonic deadline the caller passed, zero for none.
    pub deadline: u64,
    /// Monotonic time the operation was linearised at.
    pub now: u64,
    /// The resolved capability.
    pub cap: Resolved,
}

/// Monotonic time, or zero before the clock exists.
#[must_use]
pub fn now_ns() -> u64 {
    crate::time::monotonic_ns().unwrap_or(0)
}

/// True when `object` still names a live entry with the recorded generation.
#[must_use]
pub fn object_alive(machine: &Machine, object: ObjRef) -> bool {
    let index = object.index as usize;
    match object.kind {
        ObjKind::Scope => scope::table().get(index).is_some_and(|s| {
            s.state() != scope::State::Empty
                && s.generation.load(core::sync::atomic::Ordering::Relaxed) == object.generation
        }),
        ObjKind::Domain => machine.domains.get(index).is_some_and(|d| {
            d.state != crate::state::DomainState::Empty && d.generation == object.generation
        }),
        ObjKind::Memory => machine.memories.get(index).is_some_and(|m| {
            m.state != crate::memobj::State::Empty && m.generation == object.generation
        }),
        ObjKind::Endpoint => machine
            .endpoints
            .get(index)
            .is_some_and(|e| e.used && e.generation == object.generation),
        ObjKind::Invocation => machine.invocations.get(index).is_some_and(|i| {
            i.state != crate::ipc::State::Empty && i.generation == object.generation
        }),
        ObjKind::Signal => machine
            .signals
            .get(index)
            .is_some_and(|s| s.used && s.generation == object.generation),
        ObjKind::Timer => machine
            .timers
            .get(index)
            .is_some_and(|t| t.used && t.generation == object.generation),
        ObjKind::ControlLog => machine
            .logs
            .get(index)
            .is_some_and(|l| l.used && l.generation == object.generation),
        ObjKind::Device => machine
            .devices
            .get(index)
            .is_some_and(|d| d.used && d.generation == object.generation),
    }
}

/// Diagnostic identity of an object, or zero when it is gone.
#[must_use]
pub fn object_id(machine: &Machine, object: ObjRef) -> u64 {
    if !object_alive(machine, object) {
        return 0;
    }
    let index = object.index as usize;
    match object.kind {
        ObjKind::Scope => scope::table()[index].id(),
        ObjKind::Domain => machine.domains[index].id,
        ObjKind::Memory => machine.memories[index].id,
        ObjKind::Endpoint => machine.endpoints[index].id,
        ObjKind::Invocation => machine.invocations[index].id,
        ObjKind::Signal => machine.signals[index].id,
        ObjKind::Timer => machine.timers[index].id,
        ObjKind::ControlLog => machine.logs[index].id,
        ObjKind::Device => machine.devices[index].id,
    }
}

fn adjust_object_refs(machine: &mut Machine, object: ObjRef, delta: i32) {
    let index = object.index as usize;
    let apply = |value: &mut u32| {
        if delta >= 0 {
            *value = value.saturating_add(delta as u32);
        } else {
            *value = value.saturating_sub((-delta) as u32);
        }
    };
    match object.kind {
        ObjKind::Scope => {
            if delta >= 0 {
                scope::table()[index]
                    .refs
                    .fetch_add(delta as u32, core::sync::atomic::Ordering::Relaxed);
            } else {
                scope::subtract32(&scope::table()[index].refs, (-delta) as u32);
            }
        }
        ObjKind::Domain => apply(&mut machine.domains[index].refs),
        ObjKind::Memory => apply(&mut machine.memories[index].refs),
        ObjKind::Endpoint => apply(&mut machine.endpoints[index].refs),
        ObjKind::Invocation => apply(&mut machine.invocations[index].refs),
        ObjKind::Signal => apply(&mut machine.signals[index].refs),
        ObjKind::Timer => apply(&mut machine.timers[index].refs),
        ObjKind::ControlLog => apply(&mut machine.logs[index].refs),
        ObjKind::Device => apply(&mut machine.devices[index].refs),
    }
}

/// Installs a capability in a domain's table, charging the slot as metadata.
///
/// The charge is the point: a handle is kernel metadata, and a domain that can
/// accumulate handles for free can make the kernel's tables grow without ever
/// exceeding a limit it was given.
pub fn cap_install(
    machine: &mut Machine,
    domain: usize,
    object: ObjRef,
    grant: GrantId,
    slot: Option<usize>,
) -> Option<u64> {
    let owner = machine.domains[domain].owner_scope;
    if !scope::reserve(owner, scope::Resource::Metadata, 1) {
        return None;
    }
    let handle = match slot {
        Some(index) => machine.domains[domain]
            .caps
            .install_at(index, object, grant),
        None => machine.domains[domain]
            .caps
            .install(object, grant)
            .map(|(_, handle)| handle),
    };
    match handle {
        Some(handle) => {
            if grant != NO_GRANT {
                machine.grants[grant as usize].refs =
                    machine.grants[grant as usize].refs.saturating_add(1);
            }
            adjust_object_refs(machine, object, 1);
            Some(handle)
        }
        None => {
            scope::release(owner, scope::Resource::Metadata, 1);
            None
        }
    }
}

/// Releases a capability entry and everything its presence was keeping alive.
pub fn cap_release(machine: &mut Machine, domain: usize, handle: u64) -> bool {
    let Some(entry) = machine.domains[domain].caps.release(handle) else {
        return false;
    };
    release_entry(machine, domain, entry);
    true
}

/// Releases the entry in `slot` whatever generation it carries.
pub fn cap_release_slot(machine: &mut Machine, domain: usize, slot: usize) -> bool {
    let Some(entry) = machine.domains[domain].caps.release_slot(slot) else {
        return false;
    };
    release_entry(machine, domain, entry);
    true
}

fn release_entry(machine: &mut Machine, domain: usize, entry: crate::obj::CapEntry) {
    let owner = machine.domains[domain].owner_scope;
    scope::release(owner, scope::Resource::Metadata, 1);
    if entry.grant != NO_GRANT {
        let node = &mut machine.grants[entry.grant as usize];
        node.refs = node.refs.saturating_sub(1);
    }
    adjust_object_refs(machine, entry.object, -1);
    if entry.object.kind == ObjKind::Invocation {
        collect_invocation(machine, entry.object.index as usize);
    }
    // A memory object that nothing names and nothing maps can never be reached
    // again. Until K6 it stayed anyway, its pages charged and its table slot
    // held, until the scope that sponsored it retired: K6's first native run
    // created and closed objects in a loop and exhausted the kernel's table of
    // them after forty-two iterations.
    if entry.object.kind == ObjKind::Memory
        && machine
            .memories
            .get(entry.object.index as usize)
            .is_some_and(|object| object.generation == entry.object.generation)
    {
        memops::collect_memory(machine, entry.object.index as usize);
    }
    collect_grant(machine, entry.grant);
}

/// Frees an invocation record once it is discharged and nothing names it.
///
/// Discharge and the last reference happen in either order. A caller that
/// collects its reply discharges an invocation the receiver is still holding a
/// ticket for; a receiver that closes its ticket may be the last to let go. The
/// record can only be reused when both have happened, so both paths end here
/// and whichever is second is the one that reclaims it. Reclaiming only at
/// discharge is what made a service that keeps its tickets exhaust the table
/// with requests it had already answered.
pub fn collect_invocation(machine: &mut Machine, index: usize) {
    let Some(invocation) = machine.invocations.get(index) else {
        return;
    };
    if invocation.state != crate::ipc::State::Resolved || invocation.refs != 0 {
        return;
    }
    let generation = invocation.generation;
    machine.invocations[index] = crate::ipc::Invocation::empty();
    machine.invocations[index].generation = generation;
}

/// Frees a grant node once nothing refers to it and it has no children.
///
/// A fenced node stays as a tombstone while a descendant or a ticket still
/// needs to walk through it: dropping it early would make a lineage check pass
/// by forgetting the barrier rather than by observing it.
pub fn collect_grant(machine: &mut Machine, grant: GrantId) {
    let mut current = grant;
    let mut steps = 0;
    while current != NO_GRANT && steps < thalyx_abi::limit::MAX_DERIVE_DEPTH {
        steps += 1;
        let node = machine.grants[current as usize];
        if !node.used || node.refs > 0 || node.children > 0 {
            return;
        }
        let parent = node.parent;
        let sponsor = node.sponsor;
        machine.grants[current as usize] = crate::obj::Grant::empty();
        free_grant_hint(machine, current as usize);
        scope::release(sponsor, scope::Resource::Metadata, 1);
        if parent != NO_GRANT {
            let node = &mut machine.grants[parent as usize];
            node.children = node.children.saturating_sub(1);
        }
        current = parent;
    }
}

/// Notes that grant node `index` is free again, for the next allocation's
/// search to start no later than it.
pub fn free_grant_hint(machine: &mut Machine, index: usize) {
    if index < machine.grant_hint {
        machine.grant_hint = index;
    }
}

/// Allocates a grant node charged to `sponsor`.
///
/// Every parameter is a separate fact about the node and none is derivable from
/// the others: who pays for it, what it descends from, what it authorises, how
/// much, until when, what extra lifetime bounds it, and which facet it carries.
/// Bundling them into a struct would move the argument list rather than shorten
/// it, and would let a caller forget a field instead of being made to state it.
#[allow(clippy::too_many_arguments)]
pub fn grant_alloc(
    machine: &mut Machine,
    sponsor: ScopeId,
    parent: GrantId,
    object: ObjRef,
    rights: u32,
    deadline_ns: u64,
    life_scope: Option<ScopeId>,
    facet: u64,
) -> Option<GrantId> {
    let depth = if parent == NO_GRANT {
        0
    } else {
        machine.grants[parent as usize].depth + 1
    };
    if u64::from(depth) > thalyx_abi::limit::MAX_DERIVE_DEPTH {
        return None;
    }
    let total = machine.grants.len();
    let hint = machine.grant_hint.min(total);
    let free = (hint..total)
        .chain(0..hint)
        .find(|&index| !machine.grants[index].used);
    let Some(index) = free else {
        // A refusal that says which resource ran out. Without this the caller
        // sees only "exhausted" and has to guess between a scope ceiling it
        // set and a machine-wide table it did not.
        trace!(
            "k2.grants_exhausted",
            "used={} capacity={} sponsor={sponsor}",
            machine.grants.iter().filter(|node| node.used).count(),
            machine.grants.len()
        );
        return None;
    };
    if !scope::reserve(sponsor, scope::Resource::Metadata, 1) {
        return None;
    }
    let Some(id) = machine.next_id() else {
        scope::release(sponsor, scope::Resource::Metadata, 1);
        return None;
    };
    machine.grant_hint = index + 1;
    machine.grants[index] = crate::obj::Grant {
        used: true,
        id,
        parent,
        depth,
        rights,
        deadline_ns,
        life_scope,
        facet,
        object,
        fenced: false,
        refs: 0,
        children: 0,
        sponsor,
    };
    if parent != NO_GRANT {
        machine.grants[parent as usize].children += 1;
    }
    Some(index as GrantId)
}

/// Walks a grant lineage, refusing a barrier, an expiry or a closed scope.
///
/// The walk is bounded by the interface's derivation depth, so a corrupted
/// parent link ends it rather than looping.
pub fn lineage_status(machine: &Machine, grant: GrantId, now: u64) -> i64 {
    let mut current = grant;
    let mut steps = 0;
    while current != NO_GRANT {
        steps += 1;
        if steps > thalyx_abi::limit::MAX_DERIVE_DEPTH + 1 {
            return status::INVALID_HANDLE;
        }
        let Some(node) = machine.grants.get(current as usize) else {
            return status::INVALID_HANDLE;
        };
        if !node.used {
            return status::INVALID_HANDLE;
        }
        if node.fenced {
            return status::SCOPE_CLOSED;
        }
        if node.deadline_ns != 0 && now >= node.deadline_ns {
            return status::EXPIRED;
        }
        if let Some(life) = node.life_scope
            && !scope::is_open(life)
        {
            return status::SCOPE_CLOSED;
        }
        // The scope that sponsors a grant is the perimeter the barrier closes.
        // Fencing a scope has to reach the authority derived under it --
        // "including copied and derived capabilities" -- or a client could keep
        // acting through a handle it happens to still hold, and through every
        // handle it had already given away. This is the check that makes the
        // barrier reach a capability the closed domain moved to someone else.
        if !scope::is_open(node.sponsor) {
            return status::SCOPE_CLOSED;
        }
        current = node.parent;
    }
    status::OK
}

/// Resolves a handle into a capability the operation may use.
pub fn resolve(
    machine: &Machine,
    domain: usize,
    handle: u64,
    required_type: u32,
    required_rights: u32,
    now: u64,
) -> Result<Resolved, i64> {
    let resolved = resolve_entry(machine, domain, handle, required_type, required_rights)?;
    let lineage = lineage_status(machine, resolved.grant, now);
    if lineage != status::OK {
        return Err(lineage);
    }
    Ok(resolved)
}

/// Resolves a handle whose lineage may already be fenced, expired or orphaned.
///
/// [`resolve`] refuses a dead lineage, and that refusal is what makes a barrier
/// mean something: authority stops working through every handle that descends
/// from it. But three operations are *about* that state rather than acting
/// through it -- reading what a grant's lineage still permits, reading what a
/// fenced lineage still holds, and dropping the caller's own handle on it --
/// and holding them behind the same gate made each of them unreachable exactly
/// when it was needed. `CAP_DRAIN_STATUS` could never report on the
/// `CAP_FENCE` it documents, `CapInfo`'s fenced and expired lineage states
/// could never be observed, and a handle whose lineage died could never be
/// released by its holder.
///
/// Everything else still holds: the object type, the rights the operation
/// declares, and an object that still exists. Only the liveness of the lineage
/// stops being a precondition, and none of these three can act on the object.
pub fn resolve_observer(
    machine: &Machine,
    domain: usize,
    handle: u64,
    required_type: u32,
    required_rights: u32,
) -> Result<Resolved, i64> {
    resolve_entry(machine, domain, handle, required_type, required_rights)
}

/// The part of resolution that a barrier does not change.
fn resolve_entry(
    machine: &Machine,
    domain: usize,
    handle: u64,
    required_type: u32,
    required_rights: u32,
) -> Result<Resolved, i64> {
    let entry = *machine.domains[domain]
        .caps
        .lookup(handle)
        .ok_or(status::INVALID_HANDLE)?;
    if required_type != object_type::NONE && entry.object.kind.abi_type() != required_type {
        return Err(status::WRONG_TYPE);
    }
    let grant = entry.grant;
    let node = machine
        .grants
        .get(grant as usize)
        .filter(|node| node.used)
        .ok_or(status::INVALID_HANDLE)?;
    // A capability entry and its grant each name the object. They are written
    // together and must stay together: an entry pointing at one object through
    // a grant that authorises another would be authority over the wrong thing,
    // and it is the kind of mistake a table of indices makes silently. Checking
    // it here costs a comparison and turns redundant state into a checked one.
    if node.object != entry.object {
        return Err(status::INVALID_HANDLE);
    }
    if node.rights & required_rights != required_rights {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    if !object_alive(machine, entry.object) {
        return Err(status::PEER_DEAD);
    }
    Ok(Resolved {
        slot: thalyx_abi::handle_slot(handle) as usize,
        object: entry.object,
        grant,
        rights: node.rights,
        facet: node.facet,
    })
}

/// Records a refused operation on the diagnostic plane.
///
/// Every refusal is recorded up to a per-domain bound and counted afterwards,
/// so an adversarial domain cannot flood the plane and a gate can still check
/// that the refusal it expected is the refusal that happened.
pub fn note_refusal(machine: &mut Machine, domain: usize, operation: u32, code: i64) {
    if domain >= machine.domains.len() {
        return;
    }
    machine.domains[domain].refusals += 1;
    let name = operation_name(operation);
    let count = machine.domains[domain].refusals;
    if machine.domains[domain].refusal_records >= REFUSAL_RECORD_LIMIT {
        return;
    }
    machine.domains[domain].refusal_records += 1;
    let domain_name = machine.domains[domain].name_str();
    trace!(
        "k2.refused",
        "domain={domain} name={domain_name} op={name} op_code=0x{operation:x} \
         status={code} refusals={count}"
    );
}

/// Name of an operation, for diagnostic records.
#[must_use]
pub fn operation_name(operation: u32) -> &'static str {
    match spec(operation) {
        Some(spec) => spec.name,
        None => "unassigned",
    }
}

/// Validates the descriptor header the caller supplied.
fn validate_header(header: &DescriptorHeader, operation: u32, len: u64) -> Result<(), i64> {
    if header.major != thalyx_abi::VERSION_MAJOR {
        return Err(status::INCOMPATIBLE_VERSION);
    }
    if header.minor > thalyx_abi::VERSION_MINOR {
        return Err(status::INCOMPATIBLE_VERSION);
    }
    if header.opcode != operation {
        return Err(status::INVALID_ARGUMENT);
    }
    if u64::from(header.total_len) != len {
        return Err(status::INVALID_ARGUMENT);
    }
    if header.flags != 0 || header.reserved != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    Ok(())
}

/// Runs the operations that complete without waiting.
fn simple(
    machine: &mut Machine,
    ctx: &Ctx,
    staging: &mut Staging,
    operation: u32,
) -> Result<u64, i64> {
    match operation {
        op::CAP_INSPECT => capops::inspect(machine, ctx, staging),
        op::CAP_DERIVE => capops::derive(machine, ctx, staging),
        op::CAP_COPY => capops::copy(machine, ctx),
        op::CAP_CLOSE => capops::close(machine, ctx),
        op::CAP_FENCE => capops::fence(machine, ctx),
        op::CAP_DRAIN_STATUS => capops::drain_status(machine, ctx, staging),

        op::SCOPE_CREATE_CHILD => scopeops::create_child(machine, ctx, staging),
        op::SCOPE_QUERY => scopeops::query(machine, ctx, staging),
        op::SCOPE_SET_LIMITS => scopeops::set_limits(machine, ctx, staging),
        op::SCOPE_FENCE => scopeops::fence(machine, ctx),
        op::SCOPE_DRAIN_STATUS => scopeops::drain_status(machine, ctx, staging),
        op::SCOPE_RETIRE => scopeops::retire(machine, ctx, staging),
        op::SCOPE_CREATE_DOMAIN => domainops::create(machine, ctx, staging),
        op::SCOPE_CREATE_MEMORY => memops::create(machine, ctx, staging),
        op::SCOPE_CREATE_ENDPOINT => ipcops::create_endpoint(machine, ctx, staging),
        op::SCOPE_CREATE_SIGNAL => evtops::create_signal(machine, ctx),
        op::SCOPE_CREATE_TIMER => evtops::create_timer(machine, ctx, staging),

        op::DOMAIN_MAP => domainops::map(machine, ctx, staging),
        op::DOMAIN_INSTALL_CAP => domainops::install_cap(machine, ctx, staging),
        op::DOMAIN_ADD_THREAD => domainops::add_thread(machine, ctx, staging),
        op::DOMAIN_SET_FAULT_CHANNEL => domainops::set_fault_channel(machine, ctx, staging),
        op::DOMAIN_ACTIVATE => domainops::activate(machine, ctx),
        op::DOMAIN_TERMINATE => domainops::terminate(machine, ctx),
        op::DOMAIN_QUERY => domainops::query(machine, ctx, staging),

        op::MEMORY_QUERY => memops::query(machine, ctx, staging),
        op::MEMORY_COPY => memops::copy(machine, ctx, staging),
        op::MEMORY_WRITE => memops::write(machine, ctx, staging),
        op::MEMORY_READ => memops::read(machine, ctx, staging),

        op::ENDPOINT_BIND_FACET => ipcops::bind_facet(machine, ctx, staging),
        op::ENDPOINT_SEND => ipcops::send(machine, ctx, staging),
        op::ENDPOINT_QUERY => ipcops::query_endpoint(machine, ctx, staging),

        op::INVOCATION_REPLY => ipcops::reply(machine, ctx, staging),
        op::INVOCATION_BEGIN_EFFECT => ipcops::begin_effect(machine, ctx, staging),
        op::INVOCATION_RESOLVE => ipcops::resolve_invocation(machine, ctx, staging),
        op::INVOCATION_QUERY => ipcops::query_invocation(machine, ctx, staging),
        op::INVOCATION_BIND_WORKER => ipcops::bind_worker(machine, ctx),
        op::INVOCATION_UNBIND_WORKER => ipcops::unbind_worker(machine, ctx),

        op::SIGNAL_RAISE => evtops::raise(machine, ctx, staging),
        op::SIGNAL_QUERY => evtops::query_signal(machine, ctx, staging),

        op::TIMER_ARM => evtops::arm(machine, ctx, staging),
        op::TIMER_CANCEL => evtops::cancel(machine, ctx),
        op::TIMER_QUERY => evtops::query_timer(machine, ctx, staging),

        op::LOG_APPEND => logops::append(machine, ctx, staging),
        op::LOG_ACK => logops::acknowledge(machine, ctx, staging),
        op::LOG_QUERY => logops::query(machine, ctx, staging),

        op::DEVICE_QUERY => devops::query(machine, ctx, staging),
        op::DEVICE_MAP_REGION => devops::map_region(machine, ctx, staging),
        op::DEVICE_BIND_IRQ => devops::bind_irq(machine, ctx, staging),
        op::DEVICE_SET_MASTER => devops::set_master(machine, ctx, staging),
        op::DEVICE_DMA_MAP => devops::dma_map(machine, ctx, staging),
        op::DEVICE_DMA_UNMAP => devops::dma_unmap(machine, ctx, staging),

        _ => Err(status::NOT_SUPPORTED),
    }
}

/// True for the three operations that report on, or clean up after, a lineage
/// that may already be dead.
///
/// See [`resolve_observer`] for why these are not behind the liveness gate.
const fn observes_lineage(operation: u32) -> bool {
    matches!(
        operation,
        op::CAP_INSPECT | op::CAP_CLOSE | op::CAP_DRAIN_STATUS
    )
}

/// True for the operations that own their own locking because they can wait.
///
/// Three of them wait for another domain. The other two wait for the other
/// *processors*: withdrawing a mapping and publishing a seal are not complete
/// until every processor has retired the translations they removed, and that
/// acknowledgement cannot be waited for while holding the lock the
/// acknowledging processors need.
const fn waits(operation: u32) -> bool {
    matches!(
        operation,
        op::ENDPOINT_CALL
            | op::ENDPOINT_RECEIVE
            | op::SIGNAL_WAIT
            | op::LOG_READ
            | op::MEMORY_SEAL
            | op::DOMAIN_UNMAP
            | op::DEVICE_UNMAP_REGION
            | op::DEVICE_RESET
    )
}

/// Operation codes are a group in the upper half and a small ordinal in the
/// lower, which is what makes a two-level table of them possible: `SPEC_INDEX`
/// maps group and ordinal to the operation's position in `OPERATIONS`, or to
/// `NO_SPEC`. Built once, at compile time, from the table the schema
/// generates, and checked against it -- an operation whose code did not fit
/// the shape would fail the assertion rather than become unreachable.
const SPEC_GROUPS: usize = 16;
const SPEC_ORDINALS: usize = 16;
const NO_SPEC: u8 = u8::MAX;

const SPEC_INDEX: [[u8; SPEC_ORDINALS]; SPEC_GROUPS] = {
    let mut table = [[NO_SPEC; SPEC_ORDINALS]; SPEC_GROUPS];
    let mut index = 0;
    while index < OPERATIONS.len() {
        let code = OPERATIONS[index].code;
        let group = (code >> 16) as usize;
        let ordinal = (code & 0xFFFF) as usize;
        assert!(group < SPEC_GROUPS && ordinal < SPEC_ORDINALS);
        assert!(table[group][ordinal] == NO_SPEC);
        assert!(index < NO_SPEC as usize);
        table[group][ordinal] = index as u8;
        index += 1;
    }
    table
};

/// The specification of an operation code, or `None` for one the interface
/// does not assign.
///
/// Two loads and a bounds check, where the generated table's search was a
/// walk of up to fifty-nine entries on every entry of every processor.
#[inline]
#[must_use]
pub fn spec(code: u32) -> Option<&'static OpSpec> {
    let group = (code >> 16) as usize;
    let ordinal = (code & 0xFFFF) as usize;
    if group >= SPEC_GROUPS || ordinal >= SPEC_ORDINALS {
        return None;
    }
    let index = SPEC_INDEX[group][ordinal];
    if index == NO_SPEC {
        return None;
    }
    let spec = &OPERATIONS[index as usize];
    debug_assert!(spec.code == code);
    Some(spec)
}

/// One bit per assigned operation, set when the dispatch reached it.
///
/// A static word and not a field of the machine. The bit says the same thing
/// either way, and a field of the machine is a field written under the control
/// lock: every entry of every processor took that lock once before doing
/// anything, only to record that the interface had been used at all.
static OPERATIONS_REACHED: AtomicU64 = AtomicU64::new(0);

/// The operations this run has reached, one bit each.
#[must_use]
pub fn operations_reached() -> u64 {
    OPERATIONS_REACHED.load(Ordering::Relaxed)
}

/// Marks an operation as having been reached, for the run's own coverage.
///
/// A run that exercises a third of the interface and one that exercises all of
/// it produce the same shape of log, and the difference is exactly what a
/// reader needs to know before believing anything general about the whole.
/// Counting here, where every operation passes, is the only place the number
/// cannot be an estimate.
fn mark_reached(spec: &'static OpSpec) {
    // The index of the entry `spec` names, which is a reference into
    // `OPERATIONS` itself: a subtraction, rather than a second search of the
    // table on a path every entry of every processor takes.
    let base = OPERATIONS.as_ptr();
    let index = (core::ptr::from_ref(spec) as usize).wrapping_sub(base as usize)
        / core::mem::size_of::<OpSpec>();
    if index >= 64 {
        return;
    }
    let bit = 1u64 << index;
    // Read first. After the first entry of each operation the bit is set and
    // the line is shared, so the common case is a load of a line nobody is
    // writing instead of a locked read-modify-write every processor contends
    // for.
    if OPERATIONS_REACHED.load(Ordering::Relaxed) & bit == 0 {
        OPERATIONS_REACHED.fetch_or(bit, Ordering::Relaxed);
    }
}

/// Copies an operation's descriptor into kernel memory and validates it there.
///
/// Reading a domain's memory means walking that domain's page tables, so the
/// caller holds the control lock. The bytes are a message from an adversary
/// once they are here, which is why the header is checked after the copy and
/// never in the memory the caller can still write.
fn read_descriptor(
    machine: &Machine,
    domain: usize,
    operation: u32,
    spec: &OpSpec,
    frame: &TrapFrame,
    staging: &mut Staging,
) -> Result<(), i64> {
    if spec.descriptor_len == 0 {
        return Ok(());
    }
    let Some(space) = machine.domains[domain].space.as_ref() else {
        return Err(status::PEER_DEAD);
    };
    if let Err(fault) = ucopy::copy_in(
        space,
        frame.rdx,
        u64::from(spec.descriptor_len),
        &mut staging.bytes,
    ) {
        // Which way the range was wrong, not only that it was. A caller that
        // passed an unmapped pointer and one that passed a kernel address get
        // the same status, and telling them apart from the outside is
        // otherwise guesswork.
        trace!(
            "user.copy_refused",
            "domain={domain} op={} direction=in addr=0x{:x} len={} reason={}",
            operation_name(operation),
            frame.rdx,
            spec.descriptor_len,
            fault.name()
        );
        return Err(status::INVALID_ADDRESS);
    }
    let header: DescriptorHeader = staging.read(0);
    validate_header(&header, operation, u64::from(spec.descriptor_len))
}

/// Copies a response a handler wrote back into the caller's descriptor.
fn write_response(
    machine: &Machine,
    domain: usize,
    operation: u32,
    spec: &OpSpec,
    frame: &TrapFrame,
    staging: &Staging,
) -> Result<(), i64> {
    let Some(space) = machine.domains[domain].space.as_ref() else {
        return Err(status::PEER_DEAD);
    };
    let bytes = &staging.bytes[..spec.descriptor_len as usize];
    if let Err(fault) = ucopy::copy_out(space, frame.rdx, bytes) {
        // The operation happened. A failed copy of its result is a delivery
        // failure, not an undo, and the interface says so.
        trace!(
            "user.copy_refused",
            "domain={domain} op={} direction=out addr=0x{:x} len={} reason={} \
             note=operation_already_happened",
            operation_name(operation),
            frame.rdx,
            spec.descriptor_len,
            fault.name()
        );
        return Err(status::INVALID_ADDRESS);
    }
    Ok(())
}

/// Handles one `INVOKE` entry.
pub fn invoke(domain: usize, thread: usize, frame: &mut TrapFrame) -> (i64, u64) {
    let operation = match u32::try_from(frame.rsi) {
        Ok(value) => value,
        Err(_) => return refuse(domain, 0, status::INVALID_ARGUMENT),
    };
    let Some(spec) = spec(operation) else {
        return refuse(domain, operation, status::NOT_SUPPORTED);
    };
    mark_reached(spec);
    if frame.r8 & !flag::KNOWN != 0 {
        return refuse(domain, operation, status::INVALID_ARGUMENT);
    }
    if !waits(operation) && frame.r8 & flag::NONBLOCKING != 0 {
        return refuse(domain, operation, status::INVALID_ARGUMENT);
    }

    let mut staging = Staging::new();
    let now = now_ns();

    // The register length has to agree with the operation's before anything is
    // read, and an operation with no descriptor is passed none.
    if spec.descriptor_len != 0 {
        if frame.r10 != u64::from(spec.descriptor_len) {
            return refuse(domain, operation, status::INVALID_ARGUMENT);
        }
    } else if frame.rdx != 0 || frame.r10 != 0 {
        return refuse(domain, operation, status::INVALID_ARGUMENT);
    }

    let ctx_base = Ctx {
        domain,
        thread,
        handle: frame.rdi,
        operation,
        flags: frame.r8,
        deadline: frame.r9,
        now,
        cap: Resolved {
            slot: 0,
            object: ObjRef::new(ObjKind::Scope, 0, 0),
            grant: NO_GRANT,
            rights: 0,
            facet: 0,
        },
    };

    // A domain that has stopped is reaped on an idle turn of some processor.
    // On a busy machine that turn may be a long time coming, and the caller
    // asking whether a perimeter has drained is spinning on exactly that, so
    // the closing operations reap on the way in -- a retirement then sees the
    // quiescence its own termination produced -- and the termination on the
    // way out, once the domain it stopped is off every processor.
    let closing = matches!(operation, op::SCOPE_DRAIN_STATUS | op::SCOPE_RETIRE);
    if closing {
        crate::domain::reap_dead();
    }

    // Whether the answer has already been copied back, which the operations
    // that hold the lock for their whole run do on the way out of that hold.
    let mut answered = false;

    let outcome = if waits(operation) {
        // These own their locking: they may sleep, and a lock is never held
        // across a context switch, so the descriptor is copied in under a hold
        // of its own before the handler starts.
        {
            let machine = MACHINE.lock();
            if let Err(code) =
                read_descriptor(&machine, domain, operation, spec, frame, &mut staging)
            {
                drop(machine);
                return refuse(domain, operation, code);
            }
        }
        match operation {
            op::ENDPOINT_CALL => ipcops::call(&ctx_base, spec, &mut staging),
            op::ENDPOINT_RECEIVE => ipcops::receive(&ctx_base, spec, &mut staging),
            op::SIGNAL_WAIT => evtops::wait(&ctx_base, spec, &mut staging),
            op::LOG_READ => logops::read(&ctx_base, spec, &mut staging),
            op::MEMORY_SEAL => memops::seal(&ctx_base, spec, &mut staging),
            op::DOMAIN_UNMAP => domainops::unmap(&ctx_base, spec, &mut staging),
            op::DEVICE_UNMAP_REGION => devops::unmap_region(&ctx_base, spec, &mut staging),
            op::DEVICE_RESET => devops::reset(&ctx_base, spec, &mut staging),
            _ => Err(status::NOT_SUPPORTED),
        }
    } else {
        // One acquisition for the whole operation. Copying a descriptor in and
        // a response out both mean walking the calling domain's page tables,
        // so both need the lock the operation itself needs; taking it three
        // times in a row made most of this machine's lock traffic a handover
        // of a lock the same processor was about to ask for again, and every
        // handover is a cache line another processor has to be given.
        let mut machine = MACHINE.lock();
        let outcome = match read_descriptor(&machine, domain, operation, spec, frame, &mut staging)
        {
            Err(code) => Err(code),
            Ok(()) => {
                let resolved = if observes_lineage(operation) {
                    resolve_observer(&machine, domain, frame.rdi, spec.object_type, spec.rights)
                } else {
                    resolve(
                        &machine,
                        domain,
                        frame.rdi,
                        spec.object_type,
                        spec.rights,
                        now,
                    )
                };
                match resolved {
                    Ok(cap) => {
                        let ctx = Ctx { cap, ..ctx_base };
                        simple(&mut machine, &ctx, &mut staging, operation)
                    }
                    Err(code) => Err(code),
                }
            }
        };
        if spec.writes_response && staging.filled {
            answered = true;
            if let Err(code) = write_response(&machine, domain, operation, spec, frame, &staging) {
                drop(machine);
                return refuse(domain, operation, code);
            }
        }
        drop(machine);
        // The wakes the operation claimed go out now, with the lock dropped:
        // a reply's caller, a raised signal's waiters.
        crate::sched::flush_wakes();
        outcome
    };

    if operation == op::DOMAIN_TERMINATE {
        crate::domain::reap_dead();
    }

    // A response goes back whenever a handler wrote one, whatever the status.
    // Most refusals write nothing and the caller's buffer is left alone; the
    // ones that refuse with an explanation -- a retirement reporting what is
    // still outstanding -- would otherwise have their answer thrown away here,
    // which is the one place it cannot be recovered.
    if !answered && spec.writes_response && staging.filled {
        let machine = MACHINE.lock();
        if let Err(code) = write_response(&machine, domain, operation, spec, frame, &staging) {
            drop(machine);
            return refuse(domain, operation, code);
        }
    }

    match outcome {
        Ok(aux) => (status::OK, aux),
        Err(code) => refuse(domain, operation, code),
    }
}

fn refuse(domain: usize, operation: u32, code: i64) -> (i64, u64) {
    let mut machine = MACHINE.lock();
    note_refusal(&mut machine, domain, operation, code);
    (code, 0)
}

/// Fills in the response header of a descriptor the kernel is writing back.
pub fn response_header(staging: &mut Staging, spec: &OpSpec, cookie: u64) {
    staging.write(
        0,
        DescriptorHeader {
            major: thalyx_abi::VERSION_MAJOR,
            minor: thalyx_abi::VERSION_MINOR,
            opcode: spec.code,
            flags: 0,
            total_len: spec.descriptor_len,
            cookie,
            reserved: 0,
        },
    );
}

/// Body offset of every descriptor.
pub const BODY: usize = core::mem::size_of::<DescriptorHeader>();

/// Prepares a response body, zeroing whatever the request left behind.
///
/// A response never leaves the caller's own request bytes in the tail of the
/// buffer: the descriptor a caller reads back is entirely the kernel's.
pub fn begin_response(staging: &mut Staging, operation: u32) {
    staging.filled = true;
    let cookie: u64 = staging.read::<DescriptorHeader>(0).cookie;
    let Some(spec) = spec(operation) else { return };
    for byte in staging.bytes[BODY..spec.descriptor_len as usize].iter_mut() {
        *byte = 0;
    }
    response_header(staging, spec, cookie);
}

/// Writes a control receipt and mirrors it onto the diagnostic plane.
///
/// The two planes are not the same thing and the mirror does not merge them:
/// the receipt is the reserved, capability-readable record the observability
/// contract defines, and the mirrored line is a diagnostic copy that may be
/// coalesced or lost. Evidence that needs the first must read the log through a
/// capability; the mirror exists so a failure is visible without one.
#[allow(clippy::too_many_arguments)]
pub fn receipt(
    machine: &mut Machine,
    kind: u32,
    origin_domain: usize,
    origin_scope: ScopeId,
    object: u64,
    grant: u64,
    parent_invocation: u64,
    result: i64,
    a: u64,
    b: u64,
    reserved: bool,
) -> u64 {
    let Some(log_index) = machine.system_log else {
        return 0;
    };
    let domain_id = machine
        .domains
        .get(origin_domain)
        .map_or(0, |domain| domain.id);
    let scope_id = crate::scope::table()
        .get(origin_scope as usize)
        .map_or(0, |scope| scope.id());
    let record = crate::state::Receipt {
        schema: crate::ctrl::RECEIPT_SCHEMA,
        kind,
        sequence: 0,
        epoch: machine.boot_epoch,
        ns: now_ns(),
        origin_domain_id: domain_id,
        origin_scope_id: scope_id,
        object_id: object,
        grant_id: grant,
        parent_invocation_id: parent_invocation,
        result,
        a,
        b,
    };
    let log = &mut machine.logs[log_index as usize];
    let sequence = if reserved {
        log.commit_reserved(record)
    } else {
        log.write(record)
    };
    let lost = log.lost;
    let coalesced = log.coalesced;
    let used = log.count;
    let generation = log.generation;
    trace!(
        "ctrl.receipt",
        "seq={sequence} kind={kind} origin_domain={domain_id} origin_scope={scope_id} \
         object={object} grant={grant} parent={parent_invocation} result={result} \
         a=0x{a:x} b=0x{b:x} used={used} lost={lost} coalesced={coalesced}"
    );
    logops::wake_reader(machine, log_index as usize, generation);
    sequence
}

/// Reserves a control-receipt cell before a covered operation is admitted.
///
/// The audited profile buys coverage in advance. If no cell can be reserved the
/// operation is refused before it has an effect, which is the only way a claim
/// that every covered admission was recorded can be true.
pub fn reserve_receipt(machine: &mut Machine) -> bool {
    match machine.system_log {
        Some(index) => machine.logs[index as usize].reserve(),
        None => true,
    }
}

/// Returns a receipt reservation an operation did not use.
pub fn release_receipt(machine: &mut Machine) {
    if let Some(index) = machine.system_log {
        machine.logs[index as usize].release_reservation();
    }
}

/// True when `candidate` is `root` or descends from it in the grant tree.
#[must_use]
pub fn grant_within(machine: &Machine, root: GrantId, candidate: GrantId) -> bool {
    let mut current = candidate;
    let mut steps = 0;
    while current != NO_GRANT && steps <= thalyx_abi::limit::MAX_DERIVE_DEPTH + 1 {
        if current == root {
            return true;
        }
        let Some(node) = machine.grants.get(current as usize) else {
            return false;
        };
        if !node.used {
            return false;
        }
        current = node.parent;
        steps += 1;
    }
    false
}
