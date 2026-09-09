//! Memory objects: creation, mediated access, mapping withdrawal and sealing.
//!
//! Rights on a mapping are the intersection of three things and not one: what
//! the capability's grant permits, what the object's maximum rights permit, and
//! what the platform can express. The last one is not a formality — with
//! ordinary x86_64 tables a writable or executable user page is also readable,
//! so a request to map without read is refused rather than quietly upgraded,
//! and write together with execute is refused outright.
//!
//! Sealing is the conservative route the memory contract describes. `Sealing`
//! stops new writers, the existing writable mappings are withdrawn and their
//! translations invalidated, and only then is `Sealed` published. K2 is
//! uniprocessor with no DMA, so that is the whole perimeter; the cross-core
//! acknowledgement and the device quiescence a full seal needs belong to K3 and
//! are not claimed.

use thalyx_abi::generated::{
    MemoryBytes, MemoryCopyRequest, MemoryCreateRequest, MemoryInfo, object_type, right, status,
};
use thalyx_boot_protocol::PAGE_SIZE;

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::arch::x86_64::cpu;
use crate::event;
use crate::limits::MAX_OBJECT_PAGES;
use crate::memobj::{MemoryObject, State};
use crate::mm::{Owner, Rights};
use crate::obj::{ObjKind, ObjRef};
use crate::scope::{self, Resource};
use crate::state::Machine;
use crate::ucopy::Staging;

/// Translates interface memory rights into a platform mapping request.
///
/// The refusals here are the platform's, stated rather than worked around.
pub fn mapping_rights(rights: u32) -> Result<Rights, i64> {
    let read = rights & right::MEMORY_READ != 0;
    let write = rights & right::MEMORY_WRITE != 0;
    let execute = rights & right::MEMORY_EXECUTE != 0;
    if !read && (write || execute) {
        return Err(status::INSUFFICIENT_RIGHTS);
    }
    if write && execute {
        return Err(status::INVALID_ARGUMENT);
    }
    if !read {
        return Err(status::INVALID_ARGUMENT);
    }
    Ok(Rights::user(read, write, execute))
}

/// Creates a memory object sponsored by the addressed scope.
pub fn create(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: MemoryCreateRequest = staging.read(BODY);
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.pages == 0 || request.pages > MAX_OBJECT_PAGES {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.max_rights & !ObjKind::Memory.rights_mask() != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let sponsor = ctx.cap.object.index;
    if machine.scopes[sponsor as usize].state != scope::State::Open {
        return Err(status::SCOPE_CLOSED);
    }

    let index = machine
        .memories
        .iter()
        .position(|object| object.state == State::Empty)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    if !scope::reserve(
        &mut machine.scopes,
        sponsor,
        Resource::MemoryPages,
        request.pages,
    ) {
        scope::release(&mut machine.scopes, sponsor, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    }
    let base = match machine
        .allocator()
        .alloc_contiguous(request.pages, Owner::Scope(sponsor))
    {
        Ok(base) => base,
        Err(_) => {
            scope::release(&mut machine.scopes, sponsor, Resource::Metadata, 1);
            scope::release(
                &mut machine.scopes,
                sponsor,
                Resource::MemoryPages,
                request.pages,
            );
            return Err(status::LIMIT_EXHAUSTED);
        }
    };
    let Some(id) = machine.next_id() else {
        return Err(status::LIMIT_EXHAUSTED);
    };

    let generation = machine.memories[index].generation.saturating_add(1);
    machine.memories[index] = MemoryObject {
        state: State::Mutable,
        generation,
        id,
        base,
        pages: request.pages as u32,
        max_rights: request.max_rights,
        sponsor,
        map_count: 0,
        writable_maps: 0,
        label: request.label,
        refs: 0,
    };

    let object = ObjRef::new(ObjKind::Memory, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        crate::obj::NO_GRANT,
        object,
        request.max_rights,
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;

    event!(
        "mem.created",
        "object={id} label={} pages={} max_rights=0x{:x} sponsor_scope={} base=0x{:x}",
        machine.memories[index].label_str(),
        request.pages,
        request.max_rights,
        machine.scopes[sponsor as usize].id,
        base.addr()
    );
    Ok(handle)
}

/// Reports size, maximum rights, seal state and alias counts.
pub fn query(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let object = &machine.memories[index];
    let info = MemoryInfo {
        state: object.state.abi(),
        max_rights: object.max_rights,
        map_count: object.map_count,
        writable_maps: object.writable_maps,
        pages: u64::from(object.pages),
        object_id: object.id,
        sponsor_scope_id: machine.scopes[object.sponsor as usize].id,
        label: object.label,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(info.pages)
}

fn range_ok(offset: u64, length: u64, size: u64) -> bool {
    match offset.checked_add(length) {
        Some(end) => end <= size,
        None => false,
    }
}

/// Copies a bounded range from a readable object into this writable one.
pub fn copy(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: MemoryCopyRequest = staging.read(BODY);
    let source = resolve(
        machine,
        ctx.domain,
        request.source_handle,
        object_type::MEMORY,
        right::MEMORY_READ,
        ctx.now,
    )?;
    let destination = ctx.cap.object.index as usize;
    let origin = source.object.index as usize;
    if machine.memories[destination].state != State::Mutable {
        return Err(status::STATE_CONFLICT);
    }
    if !range_ok(
        request.source_offset,
        request.length,
        machine.memories[origin].len(),
    ) || !range_ok(
        request.dest_offset,
        request.length,
        machine.memories[destination].len(),
    ) {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.length == 0 {
        return Ok(0);
    }
    if origin == destination {
        return Err(status::INVALID_ARGUMENT);
    }

    let from = machine.memories[origin].base.hhdm_addr() + request.source_offset;
    let to = machine.memories[destination].base.hhdm_addr() + request.dest_offset;
    // SAFETY: both ranges were checked against their objects' lengths with
    // non-overflowing arithmetic, both objects are distinct contiguous runs of
    // frames covered by the direct map, and the machine lock is held.
    unsafe {
        core::ptr::copy_nonoverlapping(from as *const u8, to as *mut u8, request.length as usize);
    }
    Ok(request.length)
}

/// Mediated write of a bounded range.
///
/// The bytes come from the descriptor that was already copied and validated, so
/// this reveals nothing of what the object held before, which is what lets a
/// capability carry write without read.
pub fn write(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: MemoryBytes = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    if machine.memories[index].state != State::Mutable {
        return Err(status::STATE_CONFLICT);
    }
    if request.length > request.bytes.len() as u64
        || !range_ok(
            request.offset,
            request.length,
            machine.memories[index].len(),
        )
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let target = machine.memories[index].base.hhdm_addr() + request.offset;
    // SAFETY: the range was checked against the object's length with
    // non-overflowing arithmetic, the object is a contiguous run of frames
    // covered by the direct map, the source is the kernel's own staging copy,
    // and the machine lock is held.
    unsafe {
        core::ptr::copy_nonoverlapping(
            request.bytes.as_ptr(),
            target as *mut u8,
            request.length as usize,
        );
    }
    Ok(request.length)
}

/// Mediated read of a bounded range.
pub fn read(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: MemoryBytes = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    if request.length > request.bytes.len() as u64
        || !range_ok(
            request.offset,
            request.length,
            machine.memories[index].len(),
        )
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let mut result = MemoryBytes {
        offset: request.offset,
        length: request.length,
        bytes: [0; 256],
    };
    let source = machine.memories[index].base.hhdm_addr() + request.offset;
    // SAFETY: as `write`, in the other direction, into the kernel's staging
    // copy which is at least as long as `length`.
    unsafe {
        core::ptr::copy_nonoverlapping(
            source as *const u8,
            result.bytes.as_mut_ptr(),
            request.length as usize,
        );
    }
    begin_response(staging, ctx.operation);
    staging.write(BODY, result);
    Ok(request.length)
}

/// Withdraws one mapping and invalidates its translations.
///
/// Without PCID, loading CR3 flushes every non-global translation, so a mapping
/// removed from an address space that is not the current one cannot survive
/// into that space's next run. Only the current space needs an explicit
/// invalidation, which is what this does.
pub fn withdraw_map(machine: &mut Machine, map_index: usize) -> u32 {
    let record = machine.maps[map_index];
    if !record.used {
        return 0;
    }
    let domain = record.domain as usize;
    let active = cpu::read_cr3();
    let mut removed = 0;
    let current = machine.domains[domain]
        .space
        .as_ref()
        .is_some_and(|space| space.cr3() == active);
    for page in 0..u64::from(record.pages) {
        let vaddr = record.vaddr + page * PAGE_SIZE;
        let Some(space) = machine.domains[domain].space.as_mut() else {
            break;
        };
        if space.unmap(vaddr).is_some() {
            removed += 1;
            if current {
                // SAFETY: the translation was just removed from the address
                // space the CPU is running in, and invalidating it is exactly
                // what makes the removal take effect.
                unsafe { cpu::invlpg(vaddr) };
            }
        }
    }
    let memory = record.memory as usize;
    machine.memories[memory].map_count = machine.memories[memory].map_count.saturating_sub(1);
    if record.rights & right::MEMORY_WRITE != 0 {
        machine.memories[memory].writable_maps =
            machine.memories[memory].writable_maps.saturating_sub(1);
    }
    let scope = record.scope;
    machine.scopes[scope as usize].maps_pending = machine.scopes[scope as usize]
        .maps_pending
        .saturating_sub(1);
    scope::release(&mut machine.scopes, scope, Resource::Metadata, 1);
    machine.maps[map_index] = crate::memobj::MapRecord::empty();
    removed
}

/// Refuses new writers, withdraws the existing ones, then publishes the seal.
pub fn seal(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let id = machine.memories[index].id;
    match machine.memories[index].state {
        State::Sealed => {
            let object = &machine.memories[index];
            let info = MemoryInfo {
                state: object.state.abi(),
                max_rights: object.max_rights,
                map_count: object.map_count,
                writable_maps: object.writable_maps,
                pages: u64::from(object.pages),
                object_id: object.id,
                sponsor_scope_id: machine.scopes[object.sponsor as usize].id,
                label: object.label,
            };
            begin_response(staging, ctx.operation);
            staging.write(BODY, info);
            return Ok(0);
        }
        State::Mutable => {}
        _ => return Err(status::STATE_CONFLICT),
    }

    // Admission of writers closes first. Anything that observes the object from
    // here on sees a state that refuses a writable mapping, so no new alias can
    // appear behind the withdrawal below.
    machine.memories[index].state = State::Sealing;

    let mut withdrawn = 0u32;
    let mut pages = 0u32;
    for map_index in 0..machine.maps.len() {
        let record = machine.maps[map_index];
        if !record.used
            || record.memory as usize != index
            || record.rights & right::MEMORY_WRITE == 0
        {
            continue;
        }
        pages += withdraw_map(machine, map_index);
        withdrawn += 1;
    }

    if machine.memories[index].writable_maps != 0 {
        // The promise could not be made. The object stays unsealed rather than
        // being published as immutable bytes that something can still write.
        machine.memories[index].state = State::Mutable;
        event!(
            "mem.seal_failed",
            "object={id} writable_maps={} reason=alias_remains",
            machine.memories[index].writable_maps
        );
        return Err(status::STATE_CONFLICT);
    }
    machine.memories[index].state = State::Sealed;

    let object = &machine.memories[index];
    let info = MemoryInfo {
        state: object.state.abi(),
        max_rights: object.max_rights,
        map_count: object.map_count,
        writable_maps: object.writable_maps,
        pages: u64::from(object.pages),
        object_id: object.id,
        sponsor_scope_id: machine.scopes[object.sponsor as usize].id,
        label: object.label,
    };
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    event!(
        "mem.sealed",
        "object={id} label={} pages={} writers_withdrawn={withdrawn} pages_unmapped={pages} \
         remaining_maps={} perimeter=uniprocessor_no_dma",
        machine.memories[index].label_str(),
        machine.memories[index].pages,
        machine.memories[index].map_count
    );
    Ok(u64::from(withdrawn))
}
