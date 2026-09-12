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

use thalyx_abi::generated::OpSpec;
use thalyx_abi::generated::{
    MemoryBytes, MemoryCopyRequest, MemoryCreateRequest, MemoryInfo, object_type, right, status,
};
use thalyx_boot_protocol::PAGE_SIZE;

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::arch::x86_64::cpu;
use crate::limits::MAX_OBJECT_PAGES;
use crate::memobj::{MemoryObject, State};
use crate::mm::{Frame, Owner, Rights};
use crate::obj::{ObjKind, ObjRef};
use crate::scope::{self, Resource};
use crate::state::{MACHINE, Machine};
use crate::trace;
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
    if scope::table()[sponsor as usize].state() != scope::State::Open {
        return Err(status::SCOPE_CLOSED);
    }

    let index = machine
        .memories
        .iter()
        .position(|object| object.state == State::Empty)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    if !scope::reserve(sponsor, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }
    if !scope::reserve(sponsor, Resource::MemoryPages, request.pages) {
        scope::release(sponsor, Resource::Metadata, 1);
        return Err(status::LIMIT_EXHAUSTED);
    }
    let base = match machine
        .allocator()
        .alloc_contiguous(request.pages, Owner::Scope(sponsor))
    {
        Ok(base) => base,
        Err(_) => {
            scope::release(sponsor, Resource::Metadata, 1);
            scope::release(sponsor, Resource::MemoryPages, request.pages);
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
        dma_grants: 0,
        label: request.label,
        refs: 0,
        unmapped_at: 0,
        unmapped_cpus: 0,
    };

    let object = ObjRef::new(ObjKind::Memory, index as u16, generation);
    let owner = machine.domains[ctx.domain].owner_scope;
    let grant = crate::api::grant_alloc(
        machine,
        owner,
        crate::obj::NO_GRANT,
        object,
        // The object's `max_rights` is a ceiling on what may ever be done
        // *through* a capability to these pages; it is not a statement about
        // the capability itself. A creator that could not inspect, narrow or
        // hand on the object it just made would be unable to give it to anyone,
        // which is the only reason to create one. Every other creator in the
        // interface holds its type's full mask, and this is the same rule with
        // the caller's ceiling applied to the memory bits.
        request.max_rights | ObjKind::COMMON_RIGHTS,
        0,
        None,
        0,
    )
    .ok_or(status::LIMIT_EXHAUSTED)?;
    let handle = crate::api::cap_install(machine, ctx.domain, object, grant, None)
        .ok_or(status::LIMIT_EXHAUSTED)?;

    trace!(
        "mem.created",
        "object={id} label={} pages={} max_rights=0x{:x} sponsor_scope={} base=0x{:x}",
        machine.memories[index].label_str(),
        request.pages,
        request.max_rights,
        scope::table()[sponsor as usize].id(),
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
        sponsor_scope_id: scope::table()[object.sponsor as usize].id(),
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

/// Processors executing in the address space rooted at `cr3` right now.
///
/// Measured rather than inferred, and measured at the moment a withdrawal is
/// published. "The mapping was removed" and "the mapping was removed while two
/// other processors were executing in that space" are different claims, and
/// only the second one is about the race this mechanism exists for.
#[must_use]
pub fn cpus_in_space(machine: &Machine, cr3: u64) -> u32 {
    let _ = machine;
    crate::sched::cpus_in_space(cr3)
}

/// Withdraws one mapping, invalidates it here and publishes it everywhere.
///
/// Two things are needed and they are not the same thing. The processor running
/// this call has to stop using the translation immediately, because it may
/// return straight to user code without a context switch, and that is the
/// per-page invalidation below. Every *other* processor has to stop using it
/// too, and it is not running this code: what it gets is a published
/// invalidation generation, which it retires before it next runs a user thread
/// and which an initiator can wait for. Publishing here rather than at each
/// call site is what makes it impossible to remove a mapping without announcing
/// it.
///
/// Waiting for the announcement is a separate decision, because only some
/// callers need the removal to be complete before they answer. Those wait; the
/// rest rely on the frames staying in quarantine until every processor has
/// caught up.
pub fn withdraw_map(machine: &mut Machine, map_index: usize) -> u32 {
    let (removed, memory) = withdraw_map_deferring(machine, map_index);
    if let Some(memory) = memory {
        collect_memory(machine, memory);
    }
    removed
}

/// Withdraws one mapping and names the object to collect, without collecting
/// it.
///
/// For the caller that is about to wait for the invalidation it just
/// published: an object collected before that wait can only be *deferred*,
/// because nothing has acknowledged anything yet, and its frames go to the
/// quarantine to be found again later. Collecting after the acknowledgement
/// puts them straight back in the pool, which is what the ordinary case
/// deserves and what the allocator's next contiguous request needs.
pub fn withdraw_map_deferring(machine: &mut Machine, map_index: usize) -> (u32, Option<usize>) {
    let record = machine.maps[map_index];
    if !record.used {
        return (0, None);
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
    // The record names its object by table slot, and a slot is reused. The
    // generation is what says whether it still names the object the mapping was
    // installed against; decrementing the counters of whatever occupies the
    // slot now would corrupt the accounting of an unrelated object.
    let memory = record.memory as usize;
    let same_object = machine.memories[memory].generation == record.memory_generation;
    if same_object {
        machine.memories[memory].map_count = machine.memories[memory].map_count.saturating_sub(1);
        if record.rights & right::MEMORY_WRITE != 0 {
            machine.memories[memory].writable_maps =
                machine.memories[memory].writable_maps.saturating_sub(1);
        }
    }
    let scope = record.scope;
    scope::decrement(&scope::table()[scope as usize].maps_pending);
    scope::release(scope, Resource::Metadata, 1);
    let grant = record.grant;
    machine.maps[map_index] = crate::memobj::MapRecord::empty();
    // The reference the mapping held on its authorising grant. Releasing it
    // here, after the record is gone, is what lets a node whose last handle was
    // closed while the mapping stood be collected now.
    if grant != crate::obj::NO_GRANT {
        machine.grants[grant as usize].refs = machine.grants[grant as usize].refs.saturating_sub(1);
        crate::api::collect_grant(machine, grant);
    }
    if removed != 0 {
        let published = crate::tlb::publish();
        // Recorded on the object, so a later release knows whether the
        // processors that could hold one of these translations have flushed
        // past the withdrawal. The set is the domain's, plus this processor,
        // which has just invalidated the entries itself.
        if same_object {
            machine.memories[memory].unmapped_at = published;
            // Every processor that could still hold one of these translations.
            // Not this one: it removed the entries page by page as it went,
            // which is the same retirement a flush would perform and a great
            // deal cheaper than performing it. A processor that has the space
            // loaded is exactly a processor whose bit is set, so "the entries
            // were invalidated here" and "this processor is in the set" are
            // the same claim, and the one place they could disagree -- an
            // unmapper that is not executing in the space it is unmapping --
            // is the one where nothing was invalidated here and nothing is
            // held here either.
            let mine = 1u64 << crate::percpu::index();
            let live = crate::tlb::live_mask(domain);
            machine.memories[memory].unmapped_cpus |=
                (live & !mine) | if current { 0 } else { live & mine };
        }
    }
    (removed, if same_object { Some(memory) } else { None })
}

/// Releases a memory object nothing can reach any more.
///
/// "Nothing" is exact: no capability names it, no mapping holds it, and no
/// device grant lets a device write it. Its frames go straight back to the
/// pool when every processor has flushed past the invalidation that withdrew
/// its last mapping -- the ordinary case, because an unmap does not answer
/// until every processor has acknowledged -- and through the quarantine when
/// one may not have. The sponsor's charge ends here either way, as it would at
/// retirement.
///
/// An object whose sponsor has already retired is left where retirement left
/// it: retirement reported it as retained, and the slot of a retired scope is
/// not one this can safely credit.
pub fn collect_memory(machine: &mut Machine, index: usize) {
    collect_memory_acked(machine, index, false);
}

/// Releases a memory object nothing can reach any more, `acknowledged` saying
/// whether the withdrawal of its last mapping has already been acknowledged by
/// every processor that could have held one of its translations.
///
/// That is the condition the frames actually need. The generation comparison
/// below answers the same question the other way round -- has *every* online
/// processor flushed past it -- which is a sufficient condition and not a
/// necessary one: a processor that never ran in the space the object was
/// mapped into holds nothing of it whatever generation it last flushed at.
pub fn collect_memory_acked(machine: &mut Machine, index: usize, acknowledged: bool) {
    let Some(object) = machine.memories.get(index) else {
        return;
    };
    if object.state == State::Empty
        || object.refs != 0
        || object.map_count != 0
        || object.dma_grants != 0
    {
        return;
    }
    let sponsor = object.sponsor;
    if !matches!(
        scope::table()[sponsor as usize].state(),
        scope::State::Open | scope::State::Fenced | scope::State::Quiescent
    ) {
        return;
    }
    let (base, pages, generation, id, state, unmapped_at, unmapped_cpus) = (
        object.base,
        u64::from(object.pages),
        object.generation,
        object.id,
        object.state,
        object.unmapped_at,
        object.unmapped_cpus,
    );
    let immediate = acknowledged
        || unmapped_at == 0
        || crate::tlb::flushed_by(unmapped_cpus, unmapped_at)
        || crate::tlb::safe_generation() >= unmapped_at;
    let stamp = if immediate {
        0
    } else {
        // One generation for the object's whole run of frames.
        crate::tlb::retire_stamp()
    };
    for page in 0..pages {
        let frame = Frame::containing(base.addr() + page * PAGE_SIZE);
        if immediate {
            // SAFETY: no capability names the object and no mapping holds it,
            // so no page table points at these frames; every processor has
            // flushed past the invalidation that removed the last such entry,
            // so no cached translation reaches them; and no device grant names
            // them.
            unsafe { machine.allocator().release(frame, Owner::Scope(sponsor)) };
        } else {
            machine
                .allocator()
                .retire_at(frame, Owner::Scope(sponsor), stamp);
        }
    }
    scope::release(sponsor, Resource::MemoryPages, pages);
    scope::release(sponsor, Resource::Metadata, 1);
    machine.memories[index] = MemoryObject::empty();
    machine.memories[index].generation = generation;
    trace!(
        "mem.released",
        "object={id} pages={pages} state_at_release={} sponsor_scope={} reason=unreferenced \
         release={}",
        state.name(),
        scope::table()[sponsor as usize].id(),
        if immediate { "immediate" } else { "deferred" }
    );
}

/// Describes a memory object as the interface reports it.
fn describe(machine: &Machine, index: usize) -> MemoryInfo {
    let object = &machine.memories[index];
    MemoryInfo {
        state: object.state.abi(),
        max_rights: object.max_rights,
        map_count: object.map_count,
        writable_maps: object.writable_maps,
        pages: u64::from(object.pages),
        object_id: object.id,
        sponsor_scope_id: scope::table()[object.sponsor as usize].id(),
        label: object.label,
    }
}

/// Refuses new writers, withdraws the existing ones, waits for every processor
/// to have retired their translations, and only then publishes the seal.
///
/// The order is the memory contract's and the wait is the part that makes it a
/// promise rather than a bit. Removing the page-table entries stops the
/// processor running this call; it does nothing about a processor that already
/// has the translation cached, and that processor can write through it. So the
/// seal is published after every online processor has acknowledged an
/// invalidation newer than the withdrawal — not after a message was sent, and
/// not after a snapshot of the processors that looked interested at the time.
///
/// This is why the operation owns its own locking: the acknowledgement cannot
/// be waited for with the machine lock held, because the processors being
/// waited for need it.
///
/// A wait that runs out of patience does not seal. The object stays in
/// `Sealing`, where no new writer can appear and the withdrawn ones are gone,
/// and the caller is told the drain is incomplete. Retrying is allowed and
/// finishes the transition; declaring the bytes immutable is not.
pub fn seal(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let (index, id, generation, withdrawn, pages, live, mask) = {
        let mut machine = MACHINE.lock();
        let mut writers = [false; crate::state::MAX_DOMAINS];
        let cap = resolve(
            &machine,
            ctx.domain,
            ctx.handle,
            spec.object_type,
            spec.rights,
            crate::api::now_ns(),
        )?;
        let index = cap.object.index as usize;
        let id = machine.memories[index].id;
        let generation = machine.memories[index].generation;
        match machine.memories[index].state {
            State::Sealed => {
                let info = describe(&machine, index);
                drop(machine);
                begin_response(staging, ctx.operation);
                staging.write(BODY, info);
                return Ok(0);
            }
            // A previous attempt reached `Sealing` and could not observe
            // quiescence. The transition is monotonic, so this continues it
            // rather than starting over.
            State::Mutable | State::Sealing => {}
            State::Empty => return Err(status::STATE_CONFLICT),
        }

        // A device that can reach these pages is a writer the page tables
        // cannot withdraw. Sealing while one exists would promise immutability
        // the kernel has no mechanism to keep, so it is refused and the caller
        // is told to finish the device's own withdrawal first.
        if machine.memories[index].dma_grants != 0 {
            let grants = machine.memories[index].dma_grants;
            trace!(
                "mem.seal_failed",
                "object={id} dma_grants={grants} reason=device_can_still_reach_pages"
            );
            return Err(status::STATE_CONFLICT);
        }

        // Admission of writers closes first. Anything that observes the object
        // from here on sees a state that refuses a writable mapping, so no new
        // alias can appear behind the withdrawal below.
        machine.memories[index].state = State::Sealing;

        let mut withdrawn = 0u32;
        let mut pages = 0u32;
        for map_index in 0..machine.maps.len() {
            let record = machine.maps[map_index];
            if !record.used
                || record.memory as usize != index
                || record.memory_generation != generation
                || record.rights & right::MEMORY_WRITE == 0
            {
                continue;
            }
            trace!(
                "mem.writer_withdrawn",
                "object={id} domain={} vaddr=0x{:x} offset_pages={} pages={} reason=sealing",
                record.domain,
                record.vaddr,
                record.offset_pages,
                record.pages
            );
            writers[record.domain as usize] = true;
            pages += withdraw_map(&mut machine, map_index);
            withdrawn += 1;
        }
        let mut live = 0u32;
        let mut mask = 0u64;
        for domain in 0..crate::state::MAX_DOMAINS {
            if !writers[domain] {
                continue;
            }
            if let Some(space) = machine.domains[domain].space.as_ref() {
                live += cpus_in_space(&machine, space.cr3());
            }
            mask |= crate::tlb::space_mask(domain);
        }
        (index, id, generation, withdrawn, pages, live, mask)
    };

    // No lock is held here, which is the point.
    let ack = crate::tlb::shootdown();

    let mut machine = MACHINE.lock();
    if machine.memories[index].generation != generation
        || machine.memories[index].state == State::Empty
    {
        return Err(status::PEER_DEAD);
    }
    if machine.memories[index].writable_maps != 0 {
        trace!(
            "mem.seal_failed",
            "object={id} writable_maps={} reason=alias_remains",
            machine.memories[index].writable_maps
        );
        return Err(status::STATE_CONFLICT);
    }
    if ack.timed_out {
        trace!(
            "mem.seal_failed",
            "object={id} reason=invalidation_unacknowledged generation={} \
             acknowledged={} expected={} spins={} state=sealing",
            ack.generation,
            ack.acknowledged,
            ack.expected,
            ack.spins
        );
        return Err(status::DRAIN_INCOMPLETE);
    }
    machine.memories[index].state = State::Sealed;

    let info = describe(&machine, index);
    let map_count = machine.memories[index].map_count;
    let label = machine.memories[index].label_str();
    let object_pages = machine.memories[index].pages;
    trace!(
        "mem.sealed",
        "object={id} label={label} pages={object_pages} writers_withdrawn={withdrawn} \
         pages_unmapped={pages} remaining_maps={map_count} \
         invalidation_generation={} acknowledged_cpus={} expected_cpus={} \
         writer_cpus_at_withdrawal={live} writer_cpu_mask=0x{mask:x} \
         writer_cpus_ever={} perimeter=cpu_translations_retired dma=none",
        ack.generation,
        ack.acknowledged,
        ack.expected,
        mask.count_ones()
    );
    drop(machine);
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(withdrawn))
}
