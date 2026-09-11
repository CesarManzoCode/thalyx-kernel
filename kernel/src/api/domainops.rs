//! Domain operations: build, map, install, activate, stop.
//!
//! Everything a domain will hold is installed while it is `Building`, and the
//! transition out of that state happens exactly once, through a gate that
//! re-checks every requirement rather than trusting the path that got here. A
//! domain the supervisor forgot to give a fault channel, or whose scope closed
//! while it was being built, does not run.
//!
//! Installing a capability into another domain is deliberately a construction
//! operation. Once a domain is running, authority reaches it through IPC, where
//! the transfer is admitted, reserved and recorded. Allowing a builder to keep
//! reaching into a running domain's table would be a second, unaccounted path
//! for the same thing.

use thalyx_abi::generated::OpSpec;
use thalyx_abi::generated::{
    DomainCreateRequest, DomainInfo, FaultChannelRequest, InstallCapRequest, MapRequest,
    ThreadCreateRequest, UnmapRequest, object_type, right, status,
};
use thalyx_boot_protocol::PAGE_SIZE;

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::memobj::MapRecord;
use crate::memobj::State as MemState;
use crate::mm::Owner;
use crate::obj::{NO_GRANT, ObjKind, ObjRef};
use crate::scope::{self, Resource};
use crate::state::{DomainState, ExitReason, MACHINE, Machine};
use crate::ucopy::Staging;
use crate::{event, trace};

/// Page-table pages reserved for one mapping operation.
///
/// Three levels can be created by a mapping into an untouched region, plus one
/// for the page it lands in. Reserving the worst case keeps the refusal before
/// the first frame is taken; the difference stays reserved until the domain is
/// reclaimed, which is conservative rather than optimistic accounting.
const MAP_TABLE_RESERVE: u64 = 4;

/// Builds a domain from an image object, charged to the addressed scope.
pub fn create(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DomainCreateRequest = staging.read(BODY);
    let image = resolve(
        machine,
        ctx.domain,
        request.image_handle,
        object_type::MEMORY,
        right::MEMORY_READ,
        ctx.now,
    )?;
    let scope_index = ctx.cap.object.index;

    let mut name = [0u8; 16];
    for (slot, byte) in name.iter_mut().zip(request.name.iter()) {
        *slot = if byte.is_ascii_graphic() { *byte } else { 0 };
    }
    let len = name.iter().position(|byte| *byte == 0).unwrap_or(16);
    let text = core::str::from_utf8(&name[..len]).unwrap_or("domain");

    // SAFETY: the machine lock is held for the whole of this operation, the
    // object is a contiguous run of frames covered by the direct map, and
    // nothing can free or remap it while the borrow lives.
    let bytes = unsafe { machine.memories[image.object.index as usize].bytes() };

    let index =
        crate::domain::create_in(machine, text, bytes, scope_index, true).map_err(|error| {
            match error {
                crate::domain::CreateError::LimitExhausted
                | crate::domain::CreateError::DomainTableFull
                | crate::domain::CreateError::ThreadTableFull
                | crate::domain::CreateError::KernelStackExhausted
                | crate::domain::CreateError::OutOfMemory => status::LIMIT_EXHAUSTED,
                crate::domain::CreateError::ScopeClosed => status::SCOPE_CLOSED,
                _ => status::INVALID_ARGUMENT,
            }
        })?;

    let generation = machine.domains[index].generation;
    let object = ObjRef::new(ObjKind::Domain, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        NO_GRANT,
        object,
        ObjKind::Domain.rights_mask(),
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;

    trace!(
        "domain.created",
        "domain={index} name={text} id={} scope={} entry=0x{:x} segments={} image_pages={} \
         image_object={} state=building managed=1",
        machine.domains[index].id,
        machine.scopes[scope_index as usize].id,
        machine.domains[index].entry,
        machine.domains[index].segments,
        machine.domains[index].image_pages,
        machine.memories[image.object.index as usize].id
    );
    Ok(handle)
}

/// Installs a mapping of a memory object in the addressed domain.
pub fn map(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: MapRequest = staging.read(BODY);
    if request.reserved0 != 0 || request.page_count == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let target = ctx.cap.object.index as usize;
    if !matches!(
        machine.domains[target].state,
        DomainState::Building | DomainState::Runnable
    ) {
        return Err(status::STATE_CONFLICT);
    }

    let memory = resolve(
        machine,
        ctx.domain,
        request.memory_handle,
        object_type::MEMORY,
        right::MEMORY_MAP,
        ctx.now,
    )?;
    let object = memory.object.index as usize;

    // Three independent limits, and the mapping gets the intersection: what the
    // grant permits, what the object's maximum rights permit, and what the
    // platform can represent.
    if request.rights & !memory.rights != 0 {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    if request.rights & !machine.memories[object].max_rights != 0 {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    let rights = crate::api::memops::mapping_rights(request.rights)?;
    if rights.write && machine.memories[object].state != MemState::Mutable {
        return Err(status::STATE_CONFLICT);
    }

    let last = u64::from(request.offset_pages)
        .checked_add(u64::from(request.page_count))
        .ok_or(status::INVALID_ARGUMENT)?;
    if last > u64::from(machine.memories[object].pages) {
        return Err(status::INVALID_ARGUMENT);
    }
    if !request.vaddr.is_multiple_of(PAGE_SIZE) {
        return Err(status::INVALID_ARGUMENT);
    }
    let end = request
        .vaddr
        .checked_add(u64::from(request.page_count) * PAGE_SIZE)
        .ok_or(status::INVALID_ARGUMENT)?;
    if request.vaddr < thalyx_boot_protocol::USER_MIN_ADDR
        || end > thalyx_boot_protocol::USER_MAX_ADDR
    {
        return Err(status::INVALID_ARGUMENT);
    }

    let record_index = machine
        .maps
        .iter()
        .position(|record| !record.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let owner_scope = machine.domains[target].owner_scope;
    if !scope::reserve(&mut machine.scopes, owner_scope, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    if !scope::reserve(
        &mut machine.scopes,
        owner_scope,
        Resource::MemoryPages,
        MAP_TABLE_RESERVE,
    ) {
        scope::release(&mut machine.scopes, owner_scope, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    }

    let base = machine.memories[object].base;
    let mut installed = 0u32;
    let mut failure = None;
    for page in 0..u64::from(request.page_count) {
        let frame = crate::mm::Frame::containing(
            base.addr() + (u64::from(request.offset_pages) + page) * PAGE_SIZE,
        );
        let vaddr = request.vaddr + page * PAGE_SIZE;
        let owner = Owner::Domain(target as u16);
        let space = machine.domains[target]
            .space
            .as_mut()
            .expect("a live domain has an address space");
        let allocator = machine
            .memory
            .as_mut()
            .expect("frame allocator established");
        match space.map(vaddr, frame, rights, allocator, owner) {
            Ok(()) => installed += 1,
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }

    if let Some(error) = failure {
        for page in 0..u64::from(installed) {
            let vaddr = request.vaddr + page * PAGE_SIZE;
            if let Some(space) = machine.domains[target].space.as_mut() {
                space.unmap(vaddr);
            }
        }
        scope::release(&mut machine.scopes, owner_scope, Resource::Metadata, 1);
        scope::release(
            &mut machine.scopes,
            owner_scope,
            Resource::MemoryPages,
            MAP_TABLE_RESERVE,
        );
        return Err(match error {
            crate::arch::x86_64::paging::MapError::AlreadyMapped => status::STATE_CONFLICT,
            crate::arch::x86_64::paging::MapError::OutOfMemory => status::LIMIT_EXHAUSTED,
            _ => status::INVALID_ARGUMENT,
        });
    }

    machine.maps[record_index] = MapRecord {
        used: true,
        memory: memory.object.index,
        memory_generation: memory.object.generation,
        domain: target as u16,
        domain_generation: machine.domains[target].generation,
        vaddr: request.vaddr,
        offset_pages: request.offset_pages,
        pages: request.page_count,
        rights: request.rights,
        grant: memory.grant,
        scope: owner_scope,
    };
    // The record names the grant that authorised it, and it names it by table
    // slot. Holding a reference is what keeps that slot from being collected
    // and reused under the record while the mapping is still installed: a
    // withdrawal would then consult an unrelated grant.
    if memory.grant != crate::obj::NO_GRANT {
        machine.grants[memory.grant as usize].refs =
            machine.grants[memory.grant as usize].refs.saturating_add(1);
    }
    machine.domains[target].reserved_pages += MAP_TABLE_RESERVE;
    machine.memories[object].map_count += 1;
    if request.rights & right::MEMORY_WRITE != 0 {
        machine.memories[object].writable_maps += 1;
    }
    machine.scopes[owner_scope as usize].maps_pending += 1;

    trace!(
        "mem.mapped",
        "domain={target} name={} object={} vaddr=0x{:x} pages={} rights=0x{:x} \
         writable_maps={} map_count={}",
        machine.domains[target].name_str(),
        machine.memories[object].id,
        request.vaddr,
        request.page_count,
        request.rights,
        machine.memories[object].writable_maps,
        machine.memories[object].map_count
    );
    let _ = staging;
    Ok(u64::from(installed))
}

/// Withdraws a mapping and its translations.
/// Withdraws one mapping and does not answer until every processor has retired
/// the translation.
///
/// Unmapping is how authority over a page is taken back, and taking it back
/// means nothing while another processor still holds the translation. So this
/// owns its own locking, like the seal: the entries are removed and the
/// invalidation published under the lock, the lock is released, and the answer
/// waits for every online processor to acknowledge.
///
/// An unacknowledged invalidation is reported as an incomplete drain. The
/// mapping is gone from the tables either way — this is not an undo — but the
/// caller is not told the withdrawal is complete when it has not been observed
/// to be.
pub fn unmap(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let request: UnmapRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let (target, pages, live, mask) = {
        let mut machine = MACHINE.lock();
        let cap = resolve(
            &machine,
            ctx.domain,
            ctx.handle,
            spec.object_type,
            spec.rights,
            crate::api::now_ns(),
        )?;
        let target = cap.object.index as usize;
        let generation = machine.domains[target].generation;
        let record = machine.maps.iter().position(|record| {
            record.used
                && record.domain as usize == target
                && record.domain_generation == generation
                && record.vaddr == request.vaddr
                && record.pages == request.page_count
        });
        let Some(record_index) = record else {
            return Err(status::INVALID_ARGUMENT);
        };
        // Measured before the entries are removed: afterwards nothing is
        // executing there through this mapping by definition.
        let live = machine.domains[target].space.as_ref().map_or(0, |space| {
            crate::api::memops::cpus_in_space(&machine, space.cr3())
        });
        // Every processor this address space has ever been dispatched on, not
        // only the ones inside it at this instant. That is the set an
        // invalidation actually has to reach.
        let mask = machine.domains[target].cpu_mask;
        (
            target,
            crate::api::memops::withdraw_map(&mut machine, record_index),
            live,
            mask,
        )
    };

    let ack = crate::tlb::shootdown();
    trace!(
        "mem.unmapped",
        "domain={target} vaddr=0x{:x} pages={pages} active_in_space={live} \
         space_cpu_mask=0x{mask:x} space_cpus={} invalidation_generation={} \
         acknowledged_cpus={} expected_cpus={} acknowledged={}",
        request.vaddr,
        mask.count_ones(),
        ack.generation,
        ack.acknowledged,
        ack.expected,
        u8::from(!ack.timed_out)
    );
    if ack.timed_out {
        return Err(status::DRAIN_INCOMPLETE);
    }
    Ok(u64::from(pages))
}

/// Installs a capability into a building domain's table.
pub fn install_cap(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: InstallCapRequest = staging.read(BODY);
    let target = ctx.cap.object.index as usize;
    if machine.domains[target].state != DomainState::Building {
        return Err(status::STATE_CONFLICT);
    }
    if request.target_slot as usize >= crate::limits::MAX_CAPS {
        return Err(status::INVALID_ARGUMENT);
    }
    let source = resolve(
        machine,
        ctx.domain,
        request.source_handle,
        object_type::NONE,
        right::TRANSFER,
        ctx.now,
    )?;

    let grant = if request.rights_mask == 0 && request.deadline_ns == 0 {
        // A copy: the receiver shares the lineage, so a barrier reaches it.
        source.grant
    } else {
        if request.rights_mask & !source.rights != 0 {
            return Err(status::INSUFFICIENT_RIGHTS);
        }
        let parent = machine.grants[source.grant as usize];
        let deadline = if request.deadline_ns == 0 {
            parent.deadline_ns
        } else {
            if parent.deadline_ns != 0 && request.deadline_ns > parent.deadline_ns {
                return Err(status::INVALID_ARGUMENT);
            }
            request.deadline_ns
        };
        let sponsor = machine.domains[target].owner_scope;
        crate::api::grant_alloc(
            machine,
            sponsor,
            source.grant,
            source.object,
            request.rights_mask,
            deadline,
            parent.life_scope,
            parent.facet,
        )
        .ok_or(status::LIMIT_EXHAUSTED)?
    };

    let rights = machine.grants[grant as usize].rights;
    if source.object.kind == ObjKind::Endpoint && rights & right::ENDPOINT_RECEIVE != 0 {
        let endpoint = source.object.index as usize;
        let current = machine.endpoints[endpoint].receiver_domain;
        if current != u16::MAX && current as usize != target {
            return Err(status::STATE_CONFLICT);
        }
        machine.endpoints[endpoint].receiver_domain = target as u16;
        machine.endpoints[endpoint].receiver_generation = machine.domains[target].generation;
    }

    let handle = crate::api::cap_install(
        machine,
        target,
        source.object,
        grant,
        Some(request.target_slot as usize),
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    trace!(
        "cap.installed",
        "target={target} name={} slot={} handle=0x{handle:x} object_type={} object={} \
         grant={} rights=0x{rights:x}",
        machine.domains[target].name_str(),
        request.target_slot,
        source.object.kind.name(),
        crate::api::object_id(machine, source.object),
        machine.grants[grant as usize].id
    );
    Ok(handle)
}

/// Creates a thread of a building domain.
pub fn add_thread(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: ThreadCreateRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let target = ctx.cap.object.index as usize;
    // Before the address space is consulted, because a domain that has stopped
    // has no address space either: asking about the entry point first would
    // report "that entry point is not mapped" for a domain whose real answer is
    // "this domain is no longer being built".
    if machine.domains[target].state != DomainState::Building {
        return Err(status::STATE_CONFLICT);
    }
    let space_ok = machine.domains[target].space.as_ref().is_some_and(|space| {
        let entry = space
            .translate(request.entry)
            .is_some_and(|(_, flags)| flags & crate::arch::x86_64::paging::FLAG_USER != 0);
        let stack = space
            .translate(request.stack_top.saturating_sub(8))
            .is_some_and(|(_, flags)| {
                flags & crate::arch::x86_64::paging::FLAG_USER != 0
                    && flags & crate::arch::x86_64::paging::FLAG_WRITABLE != 0
            });
        entry && stack
    });
    if !space_ok {
        return Err(status::INVALID_ARGUMENT);
    }
    let thread = crate::domain::add_thread_in(
        machine,
        target,
        request.entry,
        request.stack_top,
        request.argument,
    )
    .map_err(|error| match error {
        // The state was checked above, so what is left here is a table, a
        // budget or a kernel stack that ran out.
        crate::domain::CreateError::ScopeClosed => status::STATE_CONFLICT,
        _ => status::LIMIT_EXHAUSTED,
    })?;
    trace!(
        "thread.created",
        "domain={target} name={} thread={thread} id={} entry=0x{:x} stack_top=0x{:x}",
        machine.domains[target].name_str(),
        machine.threads[thread].id,
        request.entry,
        request.stack_top
    );
    Ok(machine.threads[thread].id)
}

/// Installs the channel a fault of this domain is reported on.
///
/// The queue cell is reserved now, not when the fault happens: a supervisor
/// whose queue is full of ordinary traffic must still hear that one of its
/// domains died.
pub fn set_fault_channel(
    machine: &mut Machine,
    ctx: &Ctx,
    staging: &mut Staging,
) -> Result<u64, i64> {
    let request: FaultChannelRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let target = ctx.cap.object.index as usize;
    if machine.domains[target].state != DomainState::Building {
        return Err(status::STATE_CONFLICT);
    }
    if machine.domains[target].fault_endpoint.is_some() {
        return Err(status::STATE_CONFLICT);
    }
    let endpoint = resolve(
        machine,
        ctx.domain,
        request.endpoint_handle,
        object_type::ENDPOINT,
        right::ENDPOINT_SEND,
        ctx.now,
    )?;
    let index = endpoint.object.index as usize;
    if machine.endpoints[index].reserved_cells + 1 >= machine.endpoints[index].capacity {
        return Err(status::LIMIT_EXHAUSTED);
    }
    machine.endpoints[index].reserved_cells += 1;
    machine.grants[endpoint.grant as usize].refs += 1;
    machine.domains[target].fault_endpoint = Some(endpoint.object);
    machine.domains[target].fault_grant = endpoint.grant;
    machine.domains[target].fault_facet = endpoint.facet;
    machine.domains[target].fault_reserved = true;
    trace!(
        "domain.fault_channel",
        "domain={target} name={} endpoint={} facet={} reserved_cells={}",
        machine.domains[target].name_str(),
        machine.endpoints[index].id,
        endpoint.facet,
        machine.endpoints[index].reserved_cells
    );
    Ok(0)
}

/// Crosses the activation gate.
pub fn activate(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let target = ctx.cap.object.index as usize;
    match crate::domain::activate_in(machine, target) {
        Ok(()) => {
            trace!(
                "domain.activated",
                "domain={target} name={} id={} entry=0x{:x} threads={} scope={} \
                 fault_channel=1 state=runnable",
                machine.domains[target].name_str(),
                machine.domains[target].id,
                machine.domains[target].entry,
                machine.domains[target].thread_count(),
                machine.scopes[machine.domains[target].owner_scope as usize].id
            );
            Ok(machine.domains[target].id)
        }
        Err(refusal) => {
            event!(
                "domain.activation_refused",
                "domain={target} name={} reason={}",
                machine.domains[target].name_str(),
                refusal.name()
            );
            Err(status::STATE_CONFLICT)
        }
    }
}

/// Stops a domain.
pub fn terminate(machine: &mut Machine, ctx: &Ctx) -> Result<u64, i64> {
    let target = ctx.cap.object.index as usize;
    if crate::domain::terminate_in(machine, target, ExitReason::Terminated, 0) {
        Ok(machine.domains[target].id)
    } else {
        Err(status::STATE_CONFLICT)
    }
}

/// Reports lifecycle, faults and exit.
pub fn query(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let target = ctx.cap.object.index as usize;
    let fault = machine.domains[target].fault.unwrap_or_default();
    let charged = machine.allocator().charged(Owner::Domain(target as u16)) as u64;
    let domain = &machine.domains[target];
    let info = DomainInfo {
        state: domain.state.abi(),
        threads: domain.thread_count() as u32,
        faults: domain.faults,
        space_cpu_mask: domain.cpu_mask as u32,
        domain_id: domain.id,
        owner_scope_id: machine.scopes[domain.owner_scope as usize].id,
        exit_code: domain.exit_code,
        fault_vector: fault.vector,
        fault_rip: fault.rip,
        charged_pages: charged,
        entry_point: domain.entry,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.state))
}
