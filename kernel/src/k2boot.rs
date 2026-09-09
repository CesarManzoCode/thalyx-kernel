//! Establishing the resource tree and the first supervisor.
//!
//! The kernel builds exactly three things: a root scope that owns the machine's
//! resources, a control log, and one domain whose capability table it fills by
//! hand. Everything else in a K2 run — every other scope, domain, endpoint,
//! memory object, thread and facet — is created by that supervisor through the
//! interface, which is what "creation without ambient privilege" has to mean.
//! There is no syscall that opens a resource by name, and no domain but this
//! one receives authority it did not have to be given.
//!
//! The boot capability manifest is deliberately small and deliberately explicit:
//! the supervisor's own domain, the scope its children will be carved out of,
//! the control log, and one sealed memory object per remaining boot module. The
//! module images are copied into memory the kernel owns and then sealed, which
//! is the conservative route the memory contract describes — copy, withdraw the
//! writer, publish the seal — rather than handing out a mapping of loader memory
//! and calling it immutable.
//!
//! The first supervisor has no supervisor of its own. That is a real limit, not
//! an oversight: its fault ends the run, and the record says so.

use thalyx_abi::{boot_slot, right};
use thalyx_boot_protocol::{BootModule, HHDM_BASE, PAGE_SIZE};

use crate::event;
use crate::limits::{CONTROL_LOG_CAPACITY, CONTROL_LOG_RESERVED, MAX_OBJECT_PAGES, MAX_THREADS};
use crate::memobj::{MemoryObject, State as MemState};
use crate::mm::Owner;
use crate::obj::{NO_GRANT, ObjKind, ObjRef, ScopeId};
use crate::scope::{self, Limits, Resource, State};
use crate::state::Machine;

/// Recovery budget the root scope keeps for closing obligations, per window.
const ROOT_CLOSURE_RESERVE_NS: u64 = 2_000_000;
/// Kernel objects the root scope may hold. Every table in the kernel is smaller
/// than this, so the limit that actually binds is the table, which is what makes
/// exhaustion a refusal rather than a surprise.
const ROOT_METADATA: u64 = 512;
/// Queue bytes the root scope may hold pending.
const ROOT_QUEUE_BYTES: u64 = 64 * 1024;

/// Creates the root scope that owns everything the machine has.
pub fn establish_root(machine: &mut Machine) -> ScopeId {
    let free = machine.allocator().free_frames() as u64;
    let id = machine.next_id().expect("first identity");
    let node = &mut machine.scopes[0];
    *node = scope::Scope::empty();
    node.state = State::Open;
    node.generation = 1;
    node.id = id;
    node.parent = None;
    node.depth = 0;
    node.label[..4].copy_from_slice(b"root");
    node.limits = Limits {
        memory_pages: free,
        metadata_objects: ROOT_METADATA,
        cpu_budget_ns: crate::limits::CPU_WINDOW_NS,
        queue_bytes: ROOT_QUEUE_BYTES,
        closure_reserve_ns: ROOT_CLOSURE_RESERVE_NS,
        parallelism: MAX_THREADS as u32,
    };
    machine.root_scope = Some(0);
    event!(
        "scope.root",
        "scope=0 id={id} memory_pages={free} metadata={ROOT_METADATA} \
         cpu_budget_ns={} window_ns={} closure_reserve_ns={ROOT_CLOSURE_RESERVE_NS} \
         parallelism={}",
        crate::limits::CPU_WINDOW_NS,
        crate::limits::CPU_WINDOW_NS,
        MAX_THREADS
    );
    0
}

/// Creates the system control log the kernel writes its receipts to.
pub fn establish_control_log(machine: &mut Machine, sponsor: ScopeId) -> Option<u16> {
    let index = machine.logs.iter().position(|log| !log.used)?;
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::Metadata, 1) {
        return None;
    }
    let id = machine.next_id()?;
    let generation = machine.logs[index].generation.saturating_add(1);
    machine.logs[index] = crate::ctrl::ControlLog::empty();
    machine.logs[index].used = true;
    machine.logs[index].generation = generation;
    machine.logs[index].id = id;
    machine.logs[index].owner_scope = sponsor;
    machine.system_log = Some(index as u16);
    event!(
        "ctrl.log_established",
        "log={id} capacity={CONTROL_LOG_CAPACITY} reserved_cells={CONTROL_LOG_RESERVED} \
         profile=audited-control scope={}",
        machine.scopes[sponsor as usize].id
    );
    Some(index as u16)
}

/// Creates a child scope from kernel context.
fn child_scope(
    machine: &mut Machine,
    parent: ScopeId,
    label: &[u8],
    limits: Limits,
) -> Option<ScopeId> {
    let index = machine
        .scopes
        .iter()
        .position(|node| node.state == State::Empty)?;
    if !scope::reserve(&mut machine.scopes, parent, Resource::Metadata, 1) {
        return None;
    }
    let id = machine.next_id()?;
    let generation = machine.scopes[index].generation.saturating_add(1);
    let depth = machine.scopes[parent as usize].depth + 1;
    let node = &mut machine.scopes[index];
    *node = scope::Scope::empty();
    node.state = State::Open;
    node.generation = generation;
    node.id = id;
    node.parent = Some(parent);
    node.depth = depth;
    let len = label.len().min(16);
    node.label[..len].copy_from_slice(&label[..len]);
    node.limits = limits;
    machine.scopes[parent as usize].children += 1;
    event!(
        "scope.created",
        "scope={index} id={id} label={} parent={parent} depth={depth} memory_pages={} \
         metadata={} cpu_budget_ns={} parallelism={} queue_bytes={} closure_reserve_ns={}",
        machine.scopes[index].label_str(),
        limits.memory_pages,
        limits.metadata_objects,
        limits.cpu_budget_ns,
        limits.parallelism,
        limits.queue_bytes,
        limits.closure_reserve_ns
    );
    Some(index as ScopeId)
}

/// Copies a boot module into a memory object of its own and seals it.
///
/// The copy is the point. The loader's module memory is reclaimed, the object
/// the supervisor receives is memory the kernel allocated and charged, and its
/// only writer — the kernel, during this copy — is gone before the seal is
/// published.
fn image_object(
    machine: &mut Machine,
    sponsor: ScopeId,
    module: &BootModule,
) -> Option<(usize, u32)> {
    let pages = module.length.div_ceil(PAGE_SIZE);
    if pages == 0 || pages > MAX_OBJECT_PAGES {
        return None;
    }
    let index = machine
        .memories
        .iter()
        .position(|object| object.state == MemState::Empty)?;
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::Metadata, 1) {
        return None;
    }
    if !scope::reserve(&mut machine.scopes, sponsor, Resource::MemoryPages, pages) {
        scope::release(&mut machine.scopes, sponsor, Resource::Metadata, 1);
        return None;
    }
    let base = machine
        .allocator()
        .alloc_contiguous(pages, Owner::Scope(sponsor))
        .ok()?;
    let id = machine.next_id()?;
    let generation = machine.memories[index].generation.saturating_add(1);

    let mut label = [0u8; 16];
    let name_len = module.name.iter().position(|byte| *byte == 0).unwrap_or(32);
    let copy_len = name_len.min(16);
    label[..copy_len].copy_from_slice(&module.name[..copy_len]);

    machine.memories[index] = MemoryObject {
        state: MemState::Mutable,
        generation,
        id,
        base,
        pages: pages as u32,
        max_rights: right::MEMORY_READ | right::MEMORY_EXECUTE | right::MEMORY_MAP,
        sponsor,
        map_count: 0,
        writable_maps: 0,
        dma_grants: 0,
        label,
        refs: 0,
    };

    // SAFETY: the loader copied the module into reserved memory covered by the
    // direct map and recorded its exact length; the destination is the run of
    // frames just allocated, which nothing else references. The ranges cannot
    // overlap: one is loader memory, the other is free memory.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (HHDM_BASE + module.phys_base) as *const u8,
            base.hhdm_ptr(),
            module.length as usize,
        );
    }
    // The only writer was this copy, and it has finished. Nothing inside the
    // perimeter can write the object from here on, which is what the seal
    // promises.
    machine.memories[index].state = MemState::Sealed;
    event!(
        "mem.sealed",
        "object={id} label={} pages={pages} writers_withdrawn=0 pages_unmapped=0 \
         remaining_maps=0 perimeter=uniprocessor_no_dma origin=boot_module",
        machine.memories[index].label_str()
    );
    Some((index, generation))
}

/// Installs one boot capability in the supervisor's table.
fn install(
    machine: &mut Machine,
    domain: usize,
    slot: u32,
    object: ObjRef,
    rights: u32,
    sponsor: ScopeId,
) -> Option<u64> {
    let grant = crate::api::grant_alloc(machine, sponsor, NO_GRANT, object, rights, 0, None, 0)?;
    let handle = crate::api::cap_install(machine, domain, object, grant, Some(slot as usize))?;
    event!(
        "k2.boot_capability",
        "domain={domain} slot={slot} handle=0x{handle:x} object_type={} object={} \
         grant={} rights=0x{rights:x}",
        object.kind.name(),
        crate::api::object_id(machine, object),
        machine.grants[grant as usize].id
    );
    Some(handle)
}

/// Builds the first supervisor and hands it its boot capabilities.
///
/// Returns the supervisor's domain index, or `None` if the manifest could not
/// be assembled, in which case nothing partial is left activated.
pub fn establish_supervisor(
    machine: &mut Machine,
    root: ScopeId,
    supervisor_module: &BootModule,
    modules: &[BootModule],
) -> Option<usize> {
    let free = machine.allocator().free_frames() as u64;
    let system = child_scope(
        machine,
        root,
        b"system",
        Limits {
            memory_pages: free.min(machine.scopes[root as usize].limits.memory_pages),
            metadata_objects: ROOT_METADATA - 8,
            cpu_budget_ns: crate::limits::CPU_WINDOW_NS,
            queue_bytes: ROOT_QUEUE_BYTES,
            closure_reserve_ns: ROOT_CLOSURE_RESERVE_NS,
            parallelism: MAX_THREADS as u32,
        },
    )?;
    let own = child_scope(
        machine,
        system,
        b"supervisor",
        Limits {
            memory_pages: 256,
            metadata_objects: 128,
            cpu_budget_ns: crate::limits::CPU_WINDOW_NS / 2,
            queue_bytes: 16 * 1024,
            closure_reserve_ns: ROOT_CLOSURE_RESERVE_NS / 2,
            parallelism: 4,
        },
    )?;

    // SAFETY: the loader copied the module into reserved memory covered by the
    // direct map and recorded its exact length.
    let image = unsafe {
        core::slice::from_raw_parts(
            (HHDM_BASE + supervisor_module.phys_base) as *const u8,
            supervisor_module.length as usize,
        )
    };
    let index = match crate::domain::create_in(machine, "supervisor", image, own, false) {
        Ok(index) => index,
        Err(error) => {
            event!("k2.supervisor_rejected", "reason={}", error.name());
            return None;
        }
    };
    let generation = machine.domains[index].generation;

    let self_domain = ObjRef::new(ObjKind::Domain, index as u16, generation);
    install(
        machine,
        index,
        boot_slot::SELF_DOMAIN,
        self_domain,
        ObjKind::Domain.rights_mask(),
        own,
    )?;

    let self_scope = ObjRef::new(
        ObjKind::Scope,
        system,
        machine.scopes[system as usize].generation,
    );
    install(
        machine,
        index,
        boot_slot::SELF_SCOPE,
        self_scope,
        ObjKind::Scope.rights_mask(),
        own,
    )?;

    let log_index = machine.system_log?;
    let log = ObjRef::new(
        ObjKind::ControlLog,
        log_index,
        machine.logs[log_index as usize].generation,
    );
    install(
        machine,
        index,
        boot_slot::CONTROL_LOG,
        log,
        ObjKind::ControlLog.rights_mask(),
        own,
    )?;

    // Devices before modules, at fixed slots. A supervisor that receives none
    // is told so by their absence, and asks no question of the kernel to find
    // out: there is no call that opens a device by name.
    let mut devices = 0;
    for index in 0..crate::limits::MAX_DEVICES {
        if !machine.devices[index].used {
            continue;
        }
        let slot = boot_slot::FIRST_DEVICE + devices;
        if slot >= boot_slot::FIRST_MODULE {
            break;
        }
        let object = ObjRef::new(
            ObjKind::Device,
            index as u16,
            machine.devices[index].generation,
        );
        if install(
            machine,
            index,
            slot,
            object,
            ObjKind::Device.rights_mask(),
            system,
        )
        .is_none()
        {
            event!(
                "k2.device_rejected",
                "device={} reason=no_capability_slot",
                machine.devices[index].id
            );
            continue;
        }
        devices += 1;
    }

    let mut slot = boot_slot::FIRST_MODULE;
    let mut installed = 0;
    for module in modules {
        let Some((object_index, object_generation)) = image_object(machine, system, module) else {
            event!(
                "k2.image_rejected",
                "name={} reason=no_memory_object",
                module_name(module)
            );
            continue;
        };
        let object = ObjRef::new(ObjKind::Memory, object_index as u16, object_generation);
        let rights = right::INSPECT
            | right::DERIVE
            | right::TRANSFER
            | right::MEMORY_READ
            | right::MEMORY_EXECUTE
            | right::MEMORY_MAP;
        if install(machine, index, slot, object, rights, system).is_none() {
            event!(
                "k2.image_rejected",
                "name={} reason=no_capability_slot",
                module_name(module)
            );
            continue;
        }
        slot += 1;
        installed += 1;
    }

    machine.supervisor = Some(index);
    event!(
        "k2.supervisor_built",
        "domain={index} id={} scope={} system_scope={} images={installed} \
         devices={devices} boot_slots={} fault_channel=absent role=root_supervisor",
        machine.domains[index].id,
        machine.scopes[own as usize].id,
        machine.scopes[system as usize].id,
        slot
    );
    Some(index)
}

fn module_name(module: &BootModule) -> &str {
    let len = module.name.iter().position(|byte| *byte == 0).unwrap_or(32);
    core::str::from_utf8(&module.name[..len]).unwrap_or("?")
}
