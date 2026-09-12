//! Device operations: register windows, one interrupt, DMA grants, and the
//! session that ends them all at once.
//!
//! Every operation but the query names the session the caller believes the
//! device is in, and a mismatch is a refusal. That is the mechanism behind
//! "a reset invalidates the driver's session": there is no state to search for
//! stale references, because every reference carries the session it was made
//! under and stops being accepted the moment the number moves.
//!
//! The split of authority is in the rights, not in a convention. A driver holds
//! mapping, interrupt and DMA rights over its device; bus mastering and reset
//! are a separate right that recovery authority keeps. So a driver cannot
//! re-arm a device that is being stopped, and a supervisor does not have to
//! trust it not to.

use thalyx_abi::generated::{
    DeviceControlRequest, DeviceInfo, DeviceIrqRequest, DeviceMapRequest, DeviceRegion,
    DmaGrantInfo, DmaMapRequest, DmaUnmapRequest, OpSpec, dma_profile, object_type, status,
};
use thalyx_boot_protocol::{PAGE_SIZE, USER_MAX_ADDR, USER_MIN_ADDR};

use crate::api::{BODY, Ctx, begin_response, resolve};
use crate::arch::x86_64::trap::{DEVICE_VECTOR_BASE, DEVICE_VECTOR_COUNT};
use crate::device::{self, State};
use crate::limits::{MAX_DEVICE_REGIONS, MAX_DMA_GRANTS, MAX_IRQ_BINDINGS};
use crate::mm::{Frame, Owner, Rights};
use crate::scope::{self, Resource};
use crate::state::{DomainState, MACHINE, Machine};
use crate::ucopy::Staging;
use crate::{event, trace};

/// Physical base of the interrupt address space a message-signalled interrupt
/// writes to.
const INTERRUPT_ADDRESS_BASE: u64 = 0xFEE0_0000;

/// Builds the descriptor for one device.
fn describe(machine: &Machine, index: usize) -> DeviceInfo {
    let device = &machine.devices[index];
    let (profile, _) = device.profile(machine.iommu_described, machine.iommu_translating);
    let mut regions = [DeviceRegion {
        kind: 0,
        bar: 0,
        offset: 0,
        length: 0,
        flags: 0,
        notify_off_multiplier: 0,
    }; MAX_DEVICE_REGIONS];
    for slot in 0..device.region_count {
        let region = device.regions[slot];
        regions[slot] = DeviceRegion {
            kind: region.kind,
            bar: u32::from(region.bar),
            offset: region.offset,
            length: region.length,
            flags: u32::from(region.maps != 0),
            notify_off_multiplier: region.notify_off_multiplier,
        };
    }
    DeviceInfo {
        state: device.state.abi(),
        session: device.session,
        bdf: device.bdf,
        vendor_id: u32::from(device.vendor),
        device_id: u32::from(device.device_id),
        class_code: device.class,
        region_count: device.region_count as u32,
        irq_bound: machine
            .irqs
            .iter()
            .filter(|binding| binding.used && binding.device as usize == index)
            .count() as u32,
        dma_grants: machine
            .dma_grants
            .iter()
            .filter(|grant| grant.used && grant.device as usize == index)
            .count() as u32,
        dma_profile: profile,
        iommu_described: u32::from(machine.iommu_described),
        iommu_translating: u32::from(machine.iommu_translating),
        access_platform: u32::from(device.access_platform),
        bus_master: u32::from(device.bus_master),
        dma_address_bits: device.dma_address_bits,
        isolation_group: device.isolation_group,
        group_members: device.group_members,
        reserved0: 0,
        object_id: device.id,
        sponsor_scope_id: scope::table()[device.sponsor as usize].id(),
        label: device.label,
        regions,
    }
}

/// Checks that the caller is talking about the session the device is in.
fn session_ok(machine: &Machine, index: usize, session: u32) -> Result<(), i64> {
    if session == 0 || machine.devices[index].session != session {
        return Err(status::STATE_CONFLICT);
    }
    Ok(())
}

/// Reports identity, state, regions and isolation profile.
pub fn query(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let index = ctx.cap.object.index as usize;
    let info = describe(machine, index);
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(u64::from(info.session))
}

/// Maps one authorised register window into a domain.
pub fn map_region(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DeviceMapRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    session_ok(machine, index, request.session)?;
    let slot = request.region_index as usize;
    if slot >= machine.devices[index].region_count {
        return Err(status::INVALID_ARGUMENT);
    }
    if request.vaddr % PAGE_SIZE != 0
        || request.vaddr < USER_MIN_ADDR
        || request.vaddr >= USER_MAX_ADDR
    {
        return Err(status::INVALID_ADDRESS);
    }

    let target = resolve(
        machine,
        ctx.domain,
        request.domain_handle,
        object_type::DOMAIN,
        thalyx_abi::right::DOMAIN_BUILD,
        ctx.now,
    )?;
    let domain = target.object.index as usize;
    if machine.domains[domain].state != DomainState::Building {
        return Err(status::STATE_CONFLICT);
    }

    let region = machine.devices[index].regions[slot];
    let pages = region.length / PAGE_SIZE;
    if request.vaddr.saturating_add(region.length) > USER_MAX_ADDR {
        return Err(status::INVALID_ADDRESS);
    }
    let record_slot = machine
        .device_maps
        .iter()
        .position(|record| !record.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let owner = machine.domains[domain].owner_scope;
    if !scope::reserve(owner, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }

    // Device registers are uncacheable and never executable. Write-back caching
    // of a register window would turn a read of a device into a read of a
    // cache line, which is a different operation with the same syntax.
    let rights = Rights {
        read: true,
        write: true,
        execute: false,
        user: true,
        uncacheable: true,
    };
    let mut mapped = 0u64;
    for page in 0..pages {
        let frame = Frame::containing(region.phys + page * PAGE_SIZE);
        let allocator = machine
            .memory
            .as_mut()
            .expect("frame allocator established");
        let Some(space) = machine.domains[domain].space.as_mut() else {
            break;
        };
        if space
            .map(
                request.vaddr + page * PAGE_SIZE,
                frame,
                rights,
                allocator,
                Owner::Domain(domain as u16),
            )
            .is_err()
        {
            break;
        }
        mapped += 1;
    }
    if mapped != pages {
        for page in 0..mapped {
            if let Some(space) = machine.domains[domain].space.as_mut() {
                space.unmap(request.vaddr + page * PAGE_SIZE);
            }
        }
        scope::release(owner, Resource::Metadata, 1);
        return Err(status::STATE_CONFLICT);
    }

    machine.device_maps[record_slot] = device::MapRecord {
        used: true,
        device: index as u16,
        device_generation: machine.devices[index].generation,
        session: machine.devices[index].session,
        region: slot as u8,
        domain: domain as u16,
        domain_generation: machine.domains[domain].generation,
        vaddr: request.vaddr,
        pages: pages as u32,
        scope: owner,
    };
    machine.devices[index].regions[slot].maps += 1;
    let id = machine.devices[index].id;
    trace!(
        "device.region_mapped",
        "device={id} region={slot} kind={} domain={domain} vaddr=0x{:x} pages={pages} \
         cacheable=0 executable=0 session={}",
        region.kind,
        request.vaddr,
        request.session
    );
    Ok(pages)
}

/// Withdraws a device mapping and waits for every processor to retire it.
pub fn unmap_region(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let request: DeviceMapRequest = staging.read(BODY);
    let (id, pages) = {
        let mut machine = MACHINE.lock();
        let cap = resolve(
            &machine,
            ctx.domain,
            ctx.handle,
            spec.object_type,
            spec.rights,
            crate::api::now_ns(),
        )?;
        let index = cap.object.index as usize;
        session_ok(&machine, index, request.session)?;
        let record = machine
            .device_maps
            .iter()
            .position(|record| {
                record.used && record.device as usize == index && record.vaddr == request.vaddr
            })
            .ok_or(status::INVALID_ARGUMENT)?;
        (
            machine.devices[index].id,
            withdraw_device_map(&mut machine, record),
        )
    };
    let ack = crate::tlb::shootdown();
    trace!(
        "device.region_unmapped",
        "device={id} vaddr=0x{:x} pages={pages} invalidation_generation={} \
         acknowledged_cpus={} expected_cpus={} acknowledged={}",
        request.vaddr,
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

/// Removes one device mapping from its domain's tables.
///
/// The frames are device registers, not RAM, so nothing goes to the frame
/// allocator or to quarantine; what has to be retired is the translation, and
/// the caller decides whether to wait for that.
pub fn withdraw_device_map(machine: &mut Machine, record_index: usize) -> u32 {
    let record = machine.device_maps[record_index];
    if !record.used {
        return 0;
    }
    let domain = record.domain as usize;
    let mut removed = 0u32;
    if machine.domains[domain].generation == record.domain_generation {
        for page in 0..u64::from(record.pages) {
            let Some(space) = machine.domains[domain].space.as_mut() else {
                break;
            };
            if space.unmap(record.vaddr + page * PAGE_SIZE).is_some() {
                removed += 1;
            }
        }
    }
    let device = record.device as usize;
    if machine.devices[device].generation == record.device_generation {
        let region = record.region as usize;
        if region < MAX_DEVICE_REGIONS {
            machine.devices[device].regions[region].maps = machine.devices[device].regions[region]
                .maps
                .saturating_sub(1);
        }
    }
    scope::release(record.scope, Resource::Metadata, 1);
    machine.device_maps[record_index] = device::MapRecord::empty();
    if removed != 0 {
        crate::tlb::publish();
    }
    removed
}

/// Routes one device interrupt to a signal.
pub fn bind_irq(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DeviceIrqRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    session_ok(machine, index, request.session)?;
    if request.bits == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let Some(msix) = machine.devices[index].msix else {
        return Err(status::NOT_SUPPORTED);
    };
    let entry = u16::try_from(request.vector_index).map_err(|_| status::INVALID_ARGUMENT)?;
    if entry >= msix.vectors {
        return Err(status::INVALID_ARGUMENT);
    }

    let signal = resolve(
        machine,
        ctx.domain,
        request.signal_handle,
        object_type::SIGNAL,
        thalyx_abi::right::SIGNAL_RAISE,
        ctx.now,
    )?;

    if machine
        .irqs
        .iter()
        .any(|binding| binding.used && binding.device as usize == index && binding.entry == entry)
    {
        return Err(status::STATE_CONFLICT);
    }
    let slot = machine
        .irqs
        .iter()
        .position(|binding| !binding.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let used: u64 = machine
        .irqs
        .iter()
        .filter(|binding| binding.used)
        .map(|binding| 1u64 << (binding.vector - DEVICE_VECTOR_BASE))
        .sum();
    let offset = (0..DEVICE_VECTOR_COUNT)
        .find(|offset| used & (1u64 << offset) == 0)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let vector = DEVICE_VECTOR_BASE + offset;

    // The destination is a processor that is online, and the entry is written
    // by the kernel through its own mapping of the table. A driver that could
    // write this would be choosing the address and the payload of a memory
    // write the machine performs for it.
    let destination = crate::sched::cpu_state(0)
        .apic_id
        .load(core::sync::atomic::Ordering::Acquire);
    let address = INTERRUPT_ADDRESS_BASE | (u64::from(destination) << 12);
    let data = u32::from(vector);
    let table = machine.devices[index].msix_table;
    // SAFETY: `msix_table` is the kernel's own mapping of this function's
    // interrupt table, established when the device was assigned, and `entry` is
    // below the vector count read from the capability.
    unsafe { crate::pci::Ecam::write_msix_entry(table, entry, address, data, false) };

    machine.irqs[slot] = device::IrqBinding {
        used: true,
        device: index as u16,
        device_generation: machine.devices[index].generation,
        session: machine.devices[index].session,
        entry,
        vector,
        signal: signal.object.index,
        signal_generation: signal.object.generation,
        bits: request.bits,
        deliveries: 0,
    };
    let id = machine.devices[index].id;
    event!(
        "device.irq_bound",
        "device={id} entry={entry} vector=0x{vector:x} destination_apic_id={destination} \
         address=0x{address:x} data=0x{data:x} signal={} bits=0x{:x} session={} \
         table_written_by=kernel",
        crate::api::object_id(machine, signal.object),
        request.bits,
        request.session
    );
    Ok(u64::from(vector))
}

/// Enables or disables bus mastering.
pub fn set_master(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DeviceControlRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    session_ok(machine, index, request.session)?;
    let Some(ecam) = machine.ecam else {
        return Err(status::NOT_SUPPORTED);
    };
    let enable = request.enable != 0;
    let bdf = machine.devices[index].bdf;
    let command = ecam.set_command(bdf, crate::pci::COMMAND_BUS_MASTER, enable);
    let effective = command & crate::pci::COMMAND_BUS_MASTER != 0;
    machine.devices[index].bus_master = effective;
    machine.devices[index].state = if effective {
        State::Running
    } else {
        State::Ready
    };
    let id = machine.devices[index].id;
    event!(
        "device.bus_master",
        "device={id} requested={} effective={} command=0x{command:x} state={} session={}",
        u8::from(enable),
        u8::from(effective),
        machine.devices[index].state.name(),
        request.session
    );
    Ok(u64::from(effective))
}

/// Grants a device access to pages of a memory object.
pub fn dma_map(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DmaMapRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    session_ok(machine, index, request.session)?;
    if request.reserved0 != 0 || request.page_count == 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    let allowed = thalyx_abi::right::MEMORY_READ | thalyx_abi::right::MEMORY_WRITE;
    if request.rights == 0 || request.rights & !allowed != 0 {
        return Err(status::INVALID_ARGUMENT);
    }

    // The profile is decided before anything is pinned. A machine that cannot
    // sustain what the caller required refuses here, with nothing charged and
    // nothing granted, rather than granting something weaker under the name of
    // the thing that was asked for.
    let (profile, reason) =
        machine.devices[index].profile(machine.iommu_described, machine.iommu_translating);
    if request.required_profile != 0 && request.required_profile != profile {
        let id = machine.devices[index].id;
        event!(
            "device.profile_refused",
            "device={id} required={} available={profile} reason={reason} \
             iommu_described={} iommu_translating={} access_platform={} group_members={}",
            request.required_profile,
            u8::from(machine.iommu_described),
            u8::from(machine.iommu_translating),
            u8::from(machine.devices[index].access_platform),
            machine.devices[index].group_members
        );
        return Err(status::UNSUPPORTED_PROFILE);
    }

    let memory = resolve(
        machine,
        ctx.domain,
        request.memory_handle,
        object_type::MEMORY,
        request.rights,
        ctx.now,
    )?;
    let object = memory.object.index as usize;
    let pages = u64::from(machine.memories[object].pages);
    let end = u64::from(request.offset_pages) + u64::from(request.page_count);
    if end > pages {
        return Err(status::INVALID_ARGUMENT);
    }
    // A sealed object promises that nothing inside the perimeter can modify it.
    // A device that can write is inside the perimeter.
    if request.rights & thalyx_abi::right::MEMORY_WRITE != 0
        && machine.memories[object].state != crate::memobj::State::Mutable
    {
        return Err(status::STATE_CONFLICT);
    }

    let base = machine.memories[object].base.addr() + u64::from(request.offset_pages) * PAGE_SIZE;
    let length = u64::from(request.page_count) * PAGE_SIZE;
    let bits = machine.devices[index].dma_address_bits;
    if bits < 64 && (base + length) > (1u64 << bits) {
        return Err(status::INVALID_ARGUMENT);
    }

    let slot = machine
        .dma_grants
        .iter()
        .position(|grant| !grant.used)
        .ok_or(status::LIMIT_EXHAUSTED)?;
    let owner = machine.domains[ctx.domain].owner_scope;
    if !scope::reserve(owner, Resource::Metadata, 1) {
        return Err(status::LIMIT_EXHAUSTED);
    }

    machine.dma_grants[slot] = device::DmaGrant {
        used: true,
        device: index as u16,
        session: machine.devices[index].session,
        memory: memory.object.index,
        memory_generation: memory.object.generation,
        offset_pages: request.offset_pages,
        pages: request.page_count,
        rights: request.rights,
        iova: base,
        profile,
        scope: owner,
    };
    machine.memories[object].dma_grants += 1;

    let info = DmaGrantInfo {
        iova: base,
        length,
        rights: request.rights,
        profile,
        session: request.session,
        grant_index: slot as u32,
        memory_object_id: machine.memories[object].id,
    };
    let id = machine.devices[index].id;
    trace!(
        "device.dma_granted",
        "device={id} grant={slot} object={} iova=0x{base:x} length=0x{length:x} \
         rights=0x{:x} profile={profile} profile_reason={reason} session={} \
         enforced_by={}",
        info.memory_object_id,
        request.rights,
        request.session,
        if profile == dma_profile::STRONG_IOMMU {
            "remapping_unit"
        } else {
            "nothing_driver_is_trusted"
        }
    );
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    Ok(base)
}

/// Revokes a DMA grant.
///
/// Refused while the device can still be issuing transactions. The conservative
/// rule this revision applies is the transport-independent half of the
/// withdrawal protocol: bus mastering off, or a reset that the device
/// acknowledged, before any page stops being reachable. Removing a grant while
/// the device is live would be a bookkeeping change with no effect on what the
/// hardware may still do.
pub fn dma_unmap(machine: &mut Machine, ctx: &Ctx, staging: &mut Staging) -> Result<u64, i64> {
    let request: DmaUnmapRequest = staging.read(BODY);
    let index = ctx.cap.object.index as usize;
    session_ok(machine, index, request.session)?;
    if request.reserved0 != 0 {
        return Err(status::INVALID_ARGUMENT);
    }
    if machine.devices[index].bus_master {
        let id = machine.devices[index].id;
        event!(
            "device.dma_revoke_refused",
            "device={id} iova=0x{:x} reason=bus_master_still_enabled state={}",
            request.iova,
            machine.devices[index].state.name()
        );
        return Err(status::STATE_CONFLICT);
    }
    let slot = machine
        .dma_grants
        .iter()
        .position(|grant| {
            grant.used && grant.device as usize == index && grant.iova == request.iova
        })
        .ok_or(status::INVALID_ARGUMENT)?;
    let released = revoke_grant(machine, slot);
    let id = machine.devices[index].id;
    trace!(
        "device.dma_revoked",
        "device={id} grant={slot} iova=0x{:x} pages={released} session={} \
         device_quiescent=1",
        request.iova,
        request.session
    );
    Ok(u64::from(released))
}

/// Removes one grant and gives its metadata charge back.
fn revoke_grant(machine: &mut Machine, slot: usize) -> u32 {
    let grant = machine.dma_grants[slot];
    if !grant.used {
        return 0;
    }
    let object = grant.memory as usize;
    if machine.memories[object].generation == grant.memory_generation {
        machine.memories[object].dma_grants = machine.memories[object].dma_grants.saturating_sub(1);
    }
    scope::release(grant.scope, Resource::Metadata, 1);
    machine.dma_grants[slot] = device::DmaGrant::empty();
    grant.pages
}

/// Stops the device, takes back everything it could reach, and starts a new
/// session.
///
/// The order is the withdrawal protocol's: close admission, take back control,
/// stop the device and *confirm* it stopped, retire the translations, and only
/// then release. A device that does not confirm leaves the object in
/// `Stopping`, keeps every grant, and reports an incomplete drain. A timeout is
/// not a successful withdrawal, and the difference is the whole point of having
/// a protocol rather than a delay.
pub fn reset(ctx: &Ctx, spec: &OpSpec, staging: &mut Staging) -> Result<u64, i64> {
    let (index, id, common, ecam, bdf, msix) = {
        let mut machine = MACHINE.lock();
        let cap = resolve(
            &machine,
            ctx.domain,
            ctx.handle,
            spec.object_type,
            spec.rights,
            crate::api::now_ns(),
        )?;
        let index = cap.object.index as usize;
        machine.devices[index].state = State::Stopping;
        (
            index,
            machine.devices[index].id,
            machine.devices[index].common_config,
            machine.ecam,
            machine.devices[index].bdf,
            machine.devices[index].msix,
        )
    };

    // Control first: a driver must not be able to re-arm the device between the
    // stop and the confirmation.
    if let Some(ecam) = ecam {
        ecam.set_command(bdf, crate::pci::COMMAND_BUS_MASTER, false);
    }
    // SAFETY: the kernel's own mapping of this device's common configuration,
    // established when the device was assigned.
    let confirmed = unsafe { device::reset_transport(common) };
    // SAFETY: as above; a device that has been reset reports zero.
    let observed = unsafe { device::read_status(common) };

    if !confirmed {
        event!(
            "device.quiesce_failed",
            "device={id} status=0x{observed:x} expected=0x0 bus_master=0 \
             grants_retained={} reason=transport_did_not_confirm_reset",
            MACHINE
                .lock()
                .dma_grants
                .iter()
                .filter(|grant| grant.used && grant.device as usize == index)
                .count()
        );
        return Err(status::DRAIN_INCOMPLETE);
    }

    let mut machine = MACHINE.lock();
    machine.devices[index].bus_master = false;

    let mut unbound = 0u32;
    for slot in 0..MAX_IRQ_BINDINGS {
        if !machine.irqs[slot].used || machine.irqs[slot].device as usize != index {
            continue;
        }
        let entry = machine.irqs[slot].entry;
        if let Some(msix) = msix {
            let table = machine.devices[index].msix_table;
            let _ = msix;
            // SAFETY: the kernel's own mapping of the table; masking an entry
            // before forgetting it is what stops the hardware producing an
            // interrupt this kernel would no longer be able to attribute.
            unsafe { crate::pci::Ecam::write_msix_entry(table, entry, 0, 0, true) };
        }
        machine.irqs[slot] = device::IrqBinding::empty();
        unbound += 1;
    }

    let mut unmapped = 0u32;
    for slot in 0..crate::device::MAX_DEVICE_MAPS {
        if !machine.device_maps[slot].used || machine.device_maps[slot].device as usize != index {
            continue;
        }
        unmapped += withdraw_device_map(&mut machine, slot);
    }

    let mut revoked = 0u32;
    for slot in 0..MAX_DMA_GRANTS {
        if !machine.dma_grants[slot].used || machine.dma_grants[slot].device as usize != index {
            continue;
        }
        revoked += revoke_grant(&mut machine, slot);
    }

    machine.devices[index].session = machine.devices[index].session.wrapping_add(1).max(1);
    machine.devices[index].state = State::Ready;
    let session = machine.devices[index].session;
    let info = describe(&machine, index);
    drop(machine);

    let ack = crate::tlb::shootdown();
    event!(
        "device.reset",
        "device={id} status_after_reset=0x{observed:x} bus_master=0 irq_unbound={unbound} \
         regions_unmapped={unmapped} dma_pages_revoked={revoked} new_session={session} \
         invalidation_generation={} acknowledged_cpus={} expected_cpus={} \
         confirmed_by=transport_status_zero",
        ack.generation,
        ack.acknowledged,
        ack.expected
    );
    begin_response(staging, ctx.operation);
    staging.write(BODY, info);
    if ack.timed_out {
        return Err(status::DRAIN_INCOMPLETE);
    }
    Ok(u64::from(session))
}
