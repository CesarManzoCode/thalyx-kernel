//! Scope operations: create, limit, fence, drain, retire.
//!
//! Fencing is short by construction. It publishes a monotonic state, wakes the
//! waits that can be cancelled, withdraws messages nobody has received yet and
//! records a receipt in a cell reserved for exactly this. It does not walk page
//! tables, stop devices or wait for a server: those happen afterwards and are
//! reported through the drain status, which is why the interface separates
//! `fence`, `drain_status` and `retire` instead of offering one "revoke" that
//! would have to lie about one of the three.
//!
//! Retirement is refused while anything remains inside the perimeter. A wait
//! that times out leaves the state exactly where it was: a timeout is evidence
//! that draining is incomplete, never evidence that it finished.

use thalyx_abi::generated::{
    DrainReport, ScopeCreateRequest, ScopeInfo, ScopeLimits, receipt_kind, status,
};

use crate::api::{BODY, Ctx, begin_response, receipt};
use crate::ipc;
use crate::mm::{Frame, Owner};
use crate::obj::{ObjKind, ObjRef, ScopeId};
use crate::sched::WakeHint;
use crate::scope::{self, Limits, Resource, State};
use crate::state::Machine;
use crate::thread::{self, ThreadState};
use crate::ucopy::Staging;
use crate::{event, trace};
use core::sync::atomic::Ordering;
use thalyx_boot_protocol::PAGE_SIZE;

fn to_limits(request: &ScopeLimits) -> Limits {
    Limits {
        memory_pages: request.memory_pages,
        metadata_objects: request.metadata_objects,
        cpu_budget_ns: request.cpu_budget_ns,
        queue_bytes: request.queue_bytes,
        closure_reserve_ns: request.closure_reserve_ns,
        parallelism: request.parallelism,
    }
}

fn from_limits(limits: &Limits) -> ScopeLimits {
    ScopeLimits {
        memory_pages: limits.memory_pages,
        metadata_objects: limits.metadata_objects,
        cpu_budget_ns: limits.cpu_budget_ns,
        queue_bytes: limits.queue_bytes,
        closure_reserve_ns: limits.closure_reserve_ns,
        parallelism: limits.parallelism,
        reserved0: 0,
    }
}

/// True when every ceiling in `child` fits inside `parent`.
fn fits(child: &Limits, parent: &Limits) -> bool {
    child.memory_pages <= parent.memory_pages
        && child.metadata_objects <= parent.metadata_objects
        && child.cpu_budget_ns <= parent.cpu_budget_ns
        && child.queue_bytes <= parent.queue_bytes
        && child.closure_reserve_ns <= parent.closure_reserve_ns
        && child.parallelism <= parent.parallelism
}

/// Creates a child scope whose every ceiling fits inside this one.
///
/// A ceiling that fitted would still not multiply anything: the child's usage
/// is added to every ancestor as it is spent. The check here refuses a limit
/// that could never be honoured, so the refusal happens at configuration time
/// rather than at the first allocation.
pub fn create_child(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: ScopeCreateRequest = staging.read(BODY);
    if request.limits.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let parent = ctx.cap.object.index;
    let table = scope::table();
    let parent_state = table[parent as usize].state();
    if parent_state != State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let depth = table[parent as usize].depth.load(Ordering::Relaxed) + 1;
    if u64::from(depth) >= thalyx_abi::limit::MAX_SCOPE_DEPTH {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let limits = to_limits(&request.limits);
    if !fits(&limits, &table[parent as usize].limits.load()) {
        return Err(status::LIMIT_EXHAUSTED);
    }

    let index = match table.iter().position(|node| node.state() == State::Empty) {
        Some(index) => index,
        // No empty slot: a retired one nothing names any more gives up its
        // accounting for the newcomer.
        None => (0..table.len())
            .find(|&candidate| scope::collect_retired(machine, candidate))
            .ok_or(status::LIMIT_EXHAUSTED)?,
    };
    if !scope::reserve(parent, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let Some(id) = machine.next_id() else {
        scope::release(parent, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    };

    let node = &table[index];
    let generation = node.generation.load(Ordering::Relaxed).saturating_add(1);
    node.reset();
    node.generation.store(generation, Ordering::Relaxed);
    node.id.store(id, Ordering::Relaxed);
    node.set_parent(Some(parent));
    node.depth.store(depth, Ordering::Relaxed);
    node.set_label(request.label);
    node.limits.store(limits);
    node.window_index
        .store(ctx.now / crate::limits::CPU_WINDOW_NS, Ordering::Relaxed);
    crate::sched::forget_credits(index as ScopeId);
    // Published last: a state store is what a reader without the lock keys on.
    node.set_state(State::Open);
    table[parent as usize]
        .children
        .fetch_add(1, Ordering::Relaxed);

    let object = ObjRef::new(ObjKind::Scope, index as u16, generation);
    let sponsor = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        sponsor,
        crate::obj::NO_GRANT,
        object,
        ObjKind::Scope.rights_mask(),
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;

    let label = table[index].label_str();
    trace!(
        "scope.created",
        "scope={index} id={id} label={label} parent={parent} depth={depth} \
         memory_pages={} metadata={} cpu_budget_ns={} parallelism={} queue_bytes={} \
         closure_reserve_ns={}",
        limits.memory_pages,
        limits.metadata_objects,
        limits.cpu_budget_ns,
        limits.parallelism,
        limits.queue_bytes,
        limits.closure_reserve_ns
    );
    Ok(handle)
}

/// Reports limits, consumption and debt.
pub fn query(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let _ = machine;
    scope::roll_window(ctx.now);
    let index = ctx.cap.object.index as usize;
    let node = &scope::table()[index];
    // What the processors are still holding locally is part of the answer:
    // the pool plus every pending charge is the exact total at this instant.
    let pending = crate::sched::pending_charges(index as ScopeId);
    let info = ScopeInfo {
        limits: from_limits(&node.limits.load()),
        state: node.state().abi(),
        depth: u32::from(node.depth.load(Ordering::Relaxed)),
        parallelism_used: node.parallelism_used.load(Ordering::Relaxed),
        threads: node.threads.load(Ordering::Relaxed),
        scope_id: node.id(),
        parent_scope_id: node
            .parent()
            .map_or(0, |parent| scope::table()[parent as usize].id()),
        memory_pages_used: node.memory_pages.load(Ordering::Relaxed),
        metadata_used: node.metadata.load(Ordering::Relaxed),
        queue_bytes_used: node.queue_bytes.load(Ordering::Relaxed),
        cpu_window_used_ns: node.cpu_window_ns.load(Ordering::Relaxed) + pending,
        cpu_total_ns: node.cpu_total_ns.load(Ordering::Relaxed) + pending,
        cpu_debt_ns: node.cpu_debt_ns.load(Ordering::Relaxed),
        closure_used_ns: node.closure_used_ns.load(Ordering::Relaxed),
        window_index: node.window_index.load(Ordering::Relaxed),
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(info.cpu_total_ns)
}

/// Changes the ceilings of an open scope, within the parent's.
pub fn set_limits(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: ScopeLimits = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let _ = machine;
    let index = ctx.cap.object.index as usize;
    let node = &scope::table()[index];
    if node.state() != State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let limits = to_limits(&request);
    if let Some(parent) = node.parent()
        && !fits(&limits, &scope::table()[parent as usize].limits.load())
    {
        return Err(status::LIMIT_EXHAUSTED);
    }
    // A limit is never lowered below what the subtree already holds: that would
    // turn an accounted charge into a debt nobody agreed to.
    if limits.memory_pages < node.memory_pages.load(Ordering::Relaxed)
        || limits.metadata_objects < node.metadata.load(Ordering::Relaxed)
        || limits.queue_bytes < node.queue_bytes.load(Ordering::Relaxed)
    {
        return Err(status::STATE_CONFLICT);
    }
    node.limits.store(limits);
    event!(
        "scope.limits",
        "scope={index} id={} cpu_budget_ns={} memory_pages={} parallelism={}",
        node.id(),
        limits.cpu_budget_ns,
        limits.memory_pages,
        limits.parallelism
    );
    Ok(0)
}

/// Wakes every thread whose wait belongs to a fenced scope.
fn cancel_waits(root: ScopeId) -> u32 {
    let mut woken = 0;
    for (index, cell) in thread::iter() {
        if cell.state() != ThreadState::Blocked {
            continue;
        }
        let scope = cell.effective_scope();
        let owner = cell.control().owner_scope;
        if !scope::is_within(root, scope) && !scope::is_within(root, owner) {
            continue;
        }
        if thread::wake_if(index, |_| true, status::CANCELLED, 0, WakeHint::Any) {
            woken += 1;
        }
    }
    woken
}

/// Places the barrier and reports what it changed.
pub fn fence(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let root = ctx.cap.object.index;
    let scopes_fenced = scope::fence(root);
    let woken = cancel_waits(root);
    let withdrawn = crate::api::ipcops::withdraw_undelivered(machine, root, ctx.now);
    crate::api::ipcops::mark_cancelled(machine, root);
    let obligations = scope::pending(root);
    let id = scope::table()[root as usize].id();
    let label = scope::table()[root as usize].label_str();
    trace!(
        "scope.fenced",
        "scope={root} id={id} label={label} scopes_fenced={scopes_fenced} waits_cancelled={woken} \
         undelivered_withdrawn={withdrawn} threads={} invocations_pending={} effects_pending={} \
         maps_pending={}",
        obligations.threads,
        obligations.invocations,
        obligations.effects,
        obligations.maps
    );
    receipt(
        machine,
        receipt_kind::FENCE,
        ctx.domain,
        root,
        id,
        0,
        0,
        status::OK,
        u64::from(scopes_fenced),
        u64::from(withdrawn),
        false,
    );
    scope::advance_quiescence(ctx.now);
    Ok(u64::from(scopes_fenced))
}

fn report(machine: &mut Machine, root: ScopeId, now: u64) -> DrainReport {
    let _ = machine;
    scope::advance_quiescence(now);
    let obligations = scope::pending(root);
    let node = &scope::table()[root as usize];
    let mut report = DrainReport::zeroed();
    report.state = node.state().abi();
    report.threads_running = obligations.threads;
    report.invocations_pending = obligations.invocations;
    report.effects_pending = obligations.effects;
    report.maps_pending = obligations.maps;
    report.undelivered_cancelled = obligations.undelivered;
    report.retained_pages = obligations.pages;
    report.retained_metadata = obligations.metadata;
    report.last_progress_ns = node.last_progress_ns.load(Ordering::Relaxed);
    report.token = node.drain_token.load(Ordering::Relaxed);
    report
}

/// Reports the obligations still inside the perimeter.
pub fn drain_status(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let root = ctx.cap.object.index;
    let value = report(machine, root, ctx.now);
    begin_response(staging, ctx.operation);
    staging.write(BODY, value);
    Ok(u64::from(value.invocations_pending + value.effects_pending))
}

/// Releases a quiescent scope, or reports why it is not one yet.
pub fn retire(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let root = ctx.cap.object.index;
    let value = report(machine, root, ctx.now);
    begin_response(staging, ctx.operation);
    staging.write(BODY, value);

    let state = scope::table()[root as usize].state();
    if state == State::Open {
        return Err(status::STATE_CONFLICT);
    }
    if state == State::Fenced {
        // Not a failure of the caller: an accurate report that the perimeter is
        // still holding something. The descriptor above says what.
        return Err(status::DRAIN_INCOMPLETE);
    }
    if state == State::Retired {
        return Ok(0);
    }

    // Release what the perimeter sponsored, then report what is left. The
    // numbers have to be read after the release, or "retained" would mean
    // "held before anyone tried to free it", which is exactly the reassuring
    // and useless figure the resource contract warns against.
    let released = release_sponsored(machine, root);

    let node = &scope::table()[root as usize];
    let id = node.id();
    let previous = node.state().name();
    let pages = node.memory_pages.load(Ordering::Relaxed);
    let metadata = node.metadata.load(Ordering::Relaxed);
    for index in 0..scope::table().len() {
        let other = &scope::table()[index];
        if other.state() == State::Empty || !scope::is_within(root, index as ScopeId) {
            continue;
        }
        other.set_state(State::Retired);
    }
    trace!(
        "scope.retired",
        "scope={root} id={id} from_state={previous} freed_pages={} freed_objects={} \
         retained_pages={pages} retained_metadata={metadata}",
        released.0,
        released.1
    );
    receipt(
        machine,
        receipt_kind::RETIRE,
        ctx.domain,
        root,
        id,
        0,
        0,
        status::OK,
        pages,
        metadata,
        false,
    );
    Ok(pages)
}

/// Withdraws every capability that names `object`, wherever it is held.
///
/// A retired object's table slot is reused, and its generation is what stops an
/// old handle from naming the next occupant. Withdrawing the entries as well is
/// what keeps the accounting honest rather than merely safe: the grant and the
/// metadata charge behind each entry are released to whoever was paying for
/// them, instead of surviving as a charge on a scope that no longer exists.
fn withdraw_everywhere(machine: &mut Machine, object: ObjRef) {
    for domain in 0..machine.domains.len() {
        if machine.domains[domain].state == crate::state::DomainState::Empty {
            continue;
        }
        for slot in 0..crate::limits::MAX_CAPS {
            let entry = machine.domains[domain].caps.slots[slot];
            if entry.live && entry.object == object {
                crate::api::cap_release_slot(machine, domain, slot);
            }
        }
    }
}

/// Frees the objects a retired perimeter sponsored.
///
/// Returns the pages and the objects actually released. Retirement is where the
/// resource contract's third result happens -- "memory and metadata were freed"
/// -- and it is deliberately the slow operation of the three: the barrier is
/// what has to be brief, not this.
///
/// A memory object that something still maps is not freed. There is no mapping
/// left to take down here that this pass could confirm, and freeing a frame a
/// live page table still points at would be exactly the dangerous reuse the
/// memory contract forbids. It stays, and the retained counters say so.
fn release_sponsored(machine: &mut Machine, root: ScopeId) -> (u64, u64) {
    let mut pages = 0;
    let mut objects = 0;

    for index in 0..machine.memories.len() {
        let (state, sponsor, map_count, generation, base, page_count, id) = {
            let object = &machine.memories[index];
            (
                object.state,
                object.sponsor,
                object.map_count,
                object.generation,
                object.base,
                object.pages,
                object.id,
            )
        };
        if state == crate::memobj::State::Empty || !scope::is_within(root, sponsor) {
            continue;
        }
        if map_count != 0 {
            continue;
        }
        withdraw_everywhere(
            machine,
            ObjRef::new(ObjKind::Memory, index as u16, generation),
        );
        let count = u64::from(page_count);
        for page in 0..count {
            // SAFETY: the object has no mapping (`map_count == 0`), so no page
            // table points at these frames; this is a uniprocessor kernel with
            // no DMA in K2, so no other agent holds a reference either.
            unsafe {
                machine.allocator().release(
                    Frame::containing(base.addr() + page * PAGE_SIZE),
                    Owner::Scope(sponsor),
                );
            }
        }
        scope::release(sponsor, Resource::MemoryPages, count);
        scope::release(sponsor, Resource::Metadata, 1);
        machine.memories[index] = crate::memobj::MemoryObject::empty();
        machine.memories[index].generation = generation;
        trace!(
            "mem.released",
            "object={id} pages={count} state_at_release={} sponsor_scope={} \
             reason=scope_retired",
            state.name(),
            scope::table()[sponsor as usize].id()
        );
        pages += count;
        objects += 1;
    }

    for index in 0..machine.endpoints.len() {
        let (used, owner_scope, generation) = {
            let endpoint = &machine.endpoints[index];
            (endpoint.used, endpoint.owner_scope, endpoint.generation)
        };
        if !used || !scope::is_within(root, owner_scope) {
            continue;
        }
        withdraw_everywhere(
            machine,
            ObjRef::new(ObjKind::Endpoint, index as u16, generation),
        );
        scope::release(owner_scope, Resource::Metadata, 1);
        machine.endpoints[index] = crate::ipc::Endpoint::empty();
        machine.endpoints[index].generation = generation;
        objects += 1;
    }

    for index in 0..machine.signals.len() {
        let (used, owner_scope, generation) = {
            let signal = &machine.signals[index];
            (signal.used, signal.owner_scope, signal.generation)
        };
        if !used || !scope::is_within(root, owner_scope) {
            continue;
        }
        withdraw_everywhere(
            machine,
            ObjRef::new(ObjKind::Signal, index as u16, generation),
        );
        scope::release(owner_scope, Resource::Metadata, 1);
        machine.signals[index] = crate::events::Signal::empty();
        machine.signals[index].generation = generation;
        objects += 1;
    }

    for index in 0..machine.timers.len() {
        let (used, owner_scope, generation) = {
            let timer = &machine.timers[index];
            (timer.used, timer.owner_scope, timer.generation)
        };
        if !used || !scope::is_within(root, owner_scope) {
            continue;
        }
        withdraw_everywhere(
            machine,
            ObjRef::new(ObjKind::Timer, index as u16, generation),
        );
        scope::release(owner_scope, Resource::Metadata, 1);
        machine.timers[index] = crate::events::Timer::empty();
        machine.timers[index].generation = generation;
        objects += 1;
    }

    (pages, objects)
}

/// Number of invocations still charged to a scope subtree, for the summary.
#[must_use]
pub fn outstanding(machine: &Machine, root: ScopeId) -> u32 {
    let mut count = 0;
    for invocation in machine.invocations.iter() {
        if invocation.state != ipc::State::Empty
            && invocation.state != ipc::State::Resolved
            && scope::is_within(root, invocation.origin_scope)
        {
            count += 1;
        }
    }
    count
}
