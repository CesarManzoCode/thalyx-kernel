//! Operations every typed capability supports.
//!
//! Attenuation is the whole point of this module. `CAP_DERIVE` can only remove
//! rights and bring a deadline forward; it can add a bounding scope, which is a
//! further condition on life and not a change of who pays. `CAP_COPY` adds a
//! handle and nothing else: the copy shares the grant, so a barrier reaches it.
//!
//! `CAP_FENCE` is the decisive case the K2 groundwork names. Fencing a grant has
//! to stop a copy that lives in another domain and a derivation made from it in
//! a third, and closing the original handle is a different operation with a
//! different meaning. Both are implemented here, and the difference is visible.

use thalyx_abi::generated::{
    CapInfo, DeriveRequest, DrainReport, cap_lineage, object_type, receipt_kind, right,
    scope_state, status,
};

use crate::api::{BODY, Ctx, begin_response, grant_within, receipt, resolve};
use crate::obj::NO_GRANT;
use crate::state::Machine;
use crate::trace;
use crate::ucopy::Staging;

/// Reports what a capability names and what its lineage still permits.
pub fn inspect(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let node = machine.grants[ctx.cap.grant as usize];
    let lineage = match crate::api::lineage_status(machine, ctx.cap.grant, ctx.now) {
        status::OK => cap_lineage::LIVE,
        status::EXPIRED => cap_lineage::EXPIRED,
        _ => cap_lineage::FENCED,
    };
    let info = CapInfo {
        object_type: ctx.cap.object.kind.abi_type(),
        rights: node.rights,
        derive_depth: u32::from(node.depth),
        lineage_state: lineage,
        grant_id: node.id,
        parent_grant_id: if node.parent == NO_GRANT {
            0
        } else {
            machine.grants[node.parent as usize].id
        },
        deadline_ns: node.deadline_ns,
        life_scope_id: node
            .life_scope
            .map_or(0, |scope| crate::scope::table()[scope as usize].id()),
        object_id: crate::api::object_id(machine, ctx.cap.object),
        facet: node.facet,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.rights))
}

/// Creates a child grant that removes rights, brings the deadline forward, or
/// adds a bounding scope.
pub fn derive(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DeriveRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let parent = machine.grants[ctx.cap.grant as usize];

    // Amplification is refused rather than clamped: a caller that asked for
    // more than it holds asked for something that does not exist, and silently
    // giving it less would hide the mistake.
    if request.rights_mask & !parent.rights != 0 {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    if request.rights_mask & !ctx.cap.object.kind.rights_mask() != 0 {
        return Err(status::INVALID_ARGUMENT);
    }

    let deadline = if request.deadline_ns == 0 {
        parent.deadline_ns
    } else {
        if parent.deadline_ns != 0 && request.deadline_ns > parent.deadline_ns {
            return Err(status::INVALID_ARGUMENT);
        }
        request.deadline_ns
    };

    let life_scope = if request.life_scope == 0 {
        parent.life_scope
    } else {
        let scope = resolve(
            machine,
            ctx.domain,
            request.life_scope,
            object_type::SCOPE,
            right::INSPECT,
            ctx.now,
        )?;
        Some(scope.object.index)
    };

    let sponsor = machine.domains[ctx.domain].owner_scope;
    let child = crate::api::grant_alloc(
        machine,
        sponsor,
        ctx.cap.grant,
        ctx.cap.object,
        request.rights_mask,
        deadline,
        life_scope,
        parent.facet,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;

    match crate::api::cap_install(machine, ctx.domain, ctx.cap.object, child, None) {
        Some(handle) => {
            let child_id = machine.grants[child as usize].id;
            let name = machine.domains[ctx.domain].name_str();
            trace!(
                "cap.derive",
                "domain={} name={name} object={} parent_grant={} child_grant={} \
                 parent_rights=0x{:x} child_rights=0x{:x} deadline_ns={deadline} depth={}",
                ctx.domain,
                ctx.cap.object.kind.name(),
                parent.id,
                child_id,
                parent.rights,
                request.rights_mask,
                machine.grants[child as usize].depth
            );
            Ok(handle)
        }
        None => {
            // The child grant never became reachable, so undoing it is the whole
            // of the rollback.
            machine.grants[ctx.cap.grant as usize].children = machine.grants
                [ctx.cap.grant as usize]
                .children
                .saturating_sub(1);
            let sponsor = machine.grants[child as usize].sponsor;
            machine.grants[child as usize] = crate::obj::Grant::empty();
            crate::scope::release(sponsor, crate::scope::Resource::Metadata, 1);
            Err(status::LIMIT_EXHAUSTED)
        }
    }
}

/// Installs a second handle on the same grant.
pub fn copy(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    crate::api::cap_install(machine, ctx.domain, ctx.cap.object, ctx.cap.grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)
}

/// Releases the caller's own table entry.
pub fn close(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    if crate::api::cap_release(machine, ctx.domain, ctx.handle) {
        Ok(0)
    } else {
        Err(status::INVALID_HANDLE)
    }
}

/// Places a barrier on this grant and everything derived or copied from it.
pub fn fence(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let changed = crate::obj::fence_lineage(&mut machine.grants, ctx.cap.grant);
    let grant_id = machine.grants[ctx.cap.grant as usize].id;
    let object = crate::api::object_id(machine, ctx.cap.object);
    let scope = machine.domains[ctx.domain].owner_scope;
    trace!(
        "cap.fence",
        "domain={} grant={grant_id} object_type={} object={object} nodes_fenced={changed}",
        ctx.domain,
        ctx.cap.object.kind.name()
    );
    receipt(
        machine,
        receipt_kind::FENCE,
        ctx.domain,
        scope,
        object,
        grant_id,
        0,
        status::OK,
        u64::from(changed),
        0,
        false,
    );
    Ok(u64::from(changed))
}

/// Reports what this grant lineage still holds.
pub fn drain_status(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let root = ctx.cap.grant;
    let mut report = DrainReport::zeroed();
    report.state = if machine.grants[root as usize].fenced {
        scope_state::FENCED
    } else {
        scope_state::OPEN
    };
    for map in machine.maps.iter() {
        if map.used && grant_within(machine, root, map.grant) {
            report.maps_pending = report.maps_pending.saturating_add(1);
        }
    }
    for invocation in machine.invocations.iter() {
        if invocation.state == crate::ipc::State::Empty {
            continue;
        }
        if invocation.state != crate::ipc::State::Resolved
            && grant_within(machine, root, invocation.grant)
        {
            report.invocations_pending = report.invocations_pending.saturating_add(1);
            if invocation.effect == crate::ipc::Effect::Admitted {
                report.effects_pending = report.effects_pending.saturating_add(1);
            }
        }
    }
    report.last_progress_ns = ctx.now;
    report.token = machine.grants[root as usize].id;
    begin_response(staging, ctx.operation);
    staging.write(BODY, report);
    Ok(u64::from(report.invocations_pending))
}
