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
use crate::event;
use crate::ipc;
use crate::obj::{ObjKind, ObjRef, ScopeId};
use crate::scope::{self, Limits, Resource, State};
use crate::state::{Machine, ThreadState, Wait};
use crate::ucopy::Staging;

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
    let parent_state = machine.scopes[parent as usize].state;
    if parent_state != State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let depth = machine.scopes[parent as usize].depth + 1;
    if u64::from(depth) >= thalyx_abi::limit::MAX_SCOPE_DEPTH {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let limits = to_limits(&request.limits);
    if !fits(&limits, &machine.scopes[parent as usize].limits) {
        return Err(status::LIMIT_EXHAUSTED);
    }

    let index = machine
        .scopes
        .iter()
        .position(|node| node.state == State::Empty)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(&mut machine.scopes, parent, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    let Some(id) = machine.next_id() else {
        scope::release(&mut machine.scopes, parent, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    };

    let generation = machine.scopes[index].generation.saturating_add(1);
    let node = &mut machine.scopes[index];
    *node = scope::Scope::empty();
    node.state = State::Open;
    node.generation = generation;
    node.id = id;
    node.parent = Some(parent);
    node.depth = depth;
    node.label = request.label;
    node.limits = limits;
    node.window_index = ctx.now / crate::limits::CPU_WINDOW_NS;
    machine.scopes[parent as usize].children += 1;

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

    let label = machine.scopes[index].label_str();
    event!(
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
    scope::roll_window(&mut machine.scopes, ctx.now);
    let index = ctx.cap.object.index as usize;
    let node = &machine.scopes[index];
    let info = ScopeInfo {
        limits: from_limits(&node.limits),
        state: node.state.abi(),
        depth: u32::from(node.depth),
        parallelism_used: node.parallelism_used,
        threads: node.threads,
        scope_id: node.id,
        parent_scope_id: node
            .parent
            .map_or(0, |parent| machine.scopes[parent as usize].id),
        memory_pages_used: node.memory_pages,
        metadata_used: node.metadata,
        queue_bytes_used: node.queue_bytes,
        cpu_window_used_ns: node.cpu_window_ns,
        cpu_total_ns: node.cpu_total_ns,
        cpu_debt_ns: node.cpu_debt_ns,
        closure_used_ns: node.closure_used_ns,
        window_index: node.window_index,
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
    let index = ctx.cap.object.index as usize;
    if machine.scopes[index].state != State::Open {
        return Err(status::SCOPE_CLOSED);
    }
    let limits = to_limits(&request);
    if let Some(parent) = machine.scopes[index].parent
        && !fits(&limits, &machine.scopes[parent as usize].limits)
    {
        return Err(status::LIMIT_EXHAUSTED);
    }
    // A limit is never lowered below what the subtree already holds: that would
    // turn an accounted charge into a debt nobody agreed to.
    let node = &machine.scopes[index];
    if limits.memory_pages < node.memory_pages
        || limits.metadata_objects < node.metadata
        || limits.queue_bytes < node.queue_bytes
    {
        return Err(status::STATE_CONFLICT);
    }
    machine.scopes[index].limits = limits;
    event!(
        "scope.limits",
        "scope={index} id={} cpu_budget_ns={} memory_pages={} parallelism={}",
        machine.scopes[index].id,
        limits.cpu_budget_ns,
        limits.memory_pages,
        limits.parallelism
    );
    Ok(0)
}

/// Wakes every thread whose wait belongs to a fenced scope.
fn cancel_waits(machine: &mut Machine, root: ScopeId) -> u32 {
    let mut woken = 0;
    for index in 0..machine.threads.len() {
        if machine.threads[index].state != ThreadState::Blocked {
            continue;
        }
        let scope = machine.threads[index].effective_scope;
        let owner = machine.threads[index].owner_scope;
        if !scope::is_within(&machine.scopes, root, scope)
            && !scope::is_within(&machine.scopes, root, owner)
        {
            continue;
        }
        machine.threads[index].wake_status = status::CANCELLED;
        machine.threads[index].wait = Wait::None;
        machine.threads[index].wait_deadline_ns = 0;
        machine.threads[index].state = ThreadState::Ready;
        woken += 1;
    }
    woken
}

/// Places the barrier and reports what it changed.
pub fn fence(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let root = ctx.cap.object.index;
    let scopes_fenced = scope::fence(&mut machine.scopes, root);
    let woken = cancel_waits(machine, root);
    let withdrawn = crate::api::ipcops::withdraw_undelivered(machine, root, ctx.now);
    crate::api::ipcops::mark_cancelled(machine, root);
    let obligations = scope::pending(&machine.scopes, root);
    let id = machine.scopes[root as usize].id;
    let label = machine.scopes[root as usize].label_str();
    event!(
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
    scope::advance_quiescence(&mut machine.scopes, ctx.now);
    Ok(u64::from(scopes_fenced))
}

fn report(machine: &mut Machine, root: ScopeId, now: u64) -> DrainReport {
    scope::advance_quiescence(&mut machine.scopes, now);
    let obligations = scope::pending(&machine.scopes, root);
    let node = &machine.scopes[root as usize];
    let mut report = DrainReport::zeroed();
    report.state = node.state.abi();
    report.threads_running = obligations.threads;
    report.invocations_pending = obligations.invocations;
    report.effects_pending = obligations.effects;
    report.maps_pending = obligations.maps;
    report.undelivered_cancelled = obligations.undelivered;
    report.retained_pages = obligations.pages;
    report.retained_metadata = obligations.metadata;
    report.last_progress_ns = node.last_progress_ns;
    report.token = node.drain_token;
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

    let state = machine.scopes[root as usize].state;
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

    let id = machine.scopes[root as usize].id;
    let pages = machine.scopes[root as usize].memory_pages;
    let metadata = machine.scopes[root as usize].metadata;
    for index in 0..machine.scopes.len() {
        if machine.scopes[index].state == State::Empty
            || !scope::is_within(&machine.scopes, root, index as ScopeId)
        {
            continue;
        }
        machine.scopes[index].state = State::Retired;
    }
    event!(
        "scope.retired",
        "scope={root} id={id} retained_pages={pages} retained_metadata={metadata}"
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

/// Number of invocations still charged to a scope subtree, for the summary.
#[must_use]
pub fn outstanding(machine: &Machine, root: ScopeId) -> u32 {
    let mut count = 0;
    for invocation in machine.invocations.iter() {
        if invocation.state != ipc::State::Empty
            && invocation.state != ipc::State::Resolved
            && scope::is_within(&machine.scopes, root, invocation.origin_scope)
        {
            count += 1;
        }
    }
    count
}
