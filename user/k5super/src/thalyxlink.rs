//! Building the link domain: the K1 backend's machine side, stood up.
//!
//! The link is a Rust domain on the kernel's own target, not a native C image,
//! so it is built the way `services` builds the driver and the store: a scope,
//! a domain, the windows and objects it needs mapped, the capabilities its
//! role gets and no others, a fault channel, and activation. What is new is the
//! device it drives -- a virtio-console function the kernel assigned -- and the
//! authority it carries for the life of a Thalyx transaction: a parent scope it
//! creates a work's scope under, and the one facet of the state service it
//! writes the medium through.

use thalyx_abi::{boot_handle, boot_slot, dma_profile, right, status};
use thalyx_user_k5pkg::link::{
    DATA_PAGES, LINK_MAGIC, LinkConfig, MAX_PORTS, link_addr, link_slot,
};
use thalyx_user_rt::k2::{self, name16, report};

use crate::note;
use crate::services::config_object;

/// The modern virtio-console device id the kernel assigns as a byte channel.
const VIRTIO_CONSOLE: u32 = 0x1043;

/// What the supervisor keeps of the link it built.
pub struct Link {
    pub domain: u64,
    pub ready: u64,
    pub scope: u64,
}

/// Finds the assigned device with this device id among the boot device slots.
pub fn find_device(device_id: u32) -> Option<(u64, thalyx_abi::DeviceInfo)> {
    for offset in 0..(boot_slot::FIRST_MODULE - boot_slot::FIRST_DEVICE) {
        let handle = boot_handle(boot_slot::FIRST_DEVICE + offset);
        if let Ok(info) = k2::device_query(handle)
            && info.device_id == device_id
        {
            return Some((handle, info));
        }
    }
    None
}

/// Builds the link over the console function, the store facet and a work-scope
/// parent, and activates it. Answers the domain, its ready signal and a view of
/// its scope; `None` with a note naming the step on any refusal.
#[allow(clippy::too_many_arguments)]
pub fn build(
    system: u64,
    supervision: u64,
    image: u64,
    store_endpoint: u64,
    log: u64,
    seed: u64,
    scenario: u64,
    port_line: [u32; MAX_PORTS as usize + 2],
    cpu_window_ns: u64,
) -> Option<Link> {
    let Some((device, info)) = find_device(VIRTIO_CONSOLE) else {
        k2::note(note::BUILD_STEP_FAILED, 400);
        return None;
    };
    if info.dma_profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(info.dma_profile));
    }
    let session = info.session;

    let Ok(scope) = k2::scope_create_child(
        system,
        crate::services::limits_for(cpu_window_ns / 2, 2048, 3),
        name16("link"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 401);
        return None;
    };
    // The parent of every work scope: its own child of the system scope, so a
    // work's budget and pages are drawn from it and fencing one work reaches
    // only that work.
    let Ok(work_parent) = k2::scope_create_child(
        system,
        crate::services::limits_for(cpu_window_ns, 1024, 4),
        name16("works"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 402);
        return None;
    };

    let Ok(domain) = k2::scope_create_domain(scope, image, name16("k5link")) else {
        k2::note(note::BUILD_STEP_FAILED, 403);
        return None;
    };
    let Ok(irq) = k2::scope_create_signal(scope) else {
        k2::note(note::BUILD_STEP_FAILED, 404);
        return None;
    };
    let Ok(ready) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 405);
        return None;
    };
    let Ok(ring) = k2::scope_create_memory(
        scope,
        link_addr::RING_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("linkring"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 406);
        return None;
    };
    let Ok(data) = k2::scope_create_memory(
        scope,
        DATA_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("linkdata"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 407);
        return None;
    };
    let Ok(stage) = k2::scope_create_memory(
        scope,
        link_addr::STAGE_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("linkstage"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 408);
        return None;
    };

    // The one facet of the state service the link writes through: one service,
    // one writer.
    let Some((store_facet, _)) =
        crate::bind_facet(store_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 409);
        return None;
    };

    let config = LinkConfig {
        magic: LINK_MAGIC,
        seed,
        scenario,
        version: 1,
        ports: info.region_count.min(MAX_PORTS),
        reserved0: 0,
        port_line,
    };
    let Some(page) = config_object(scope, "linkcfg", &config) else {
        k2::note(note::BUILD_STEP_FAILED, 410);
        return None;
    };
    if k2::domain_map(domain, page, link_addr::CONFIG, 0, 1, right::MEMORY_READ).is_err() {
        k2::note(note::BUILD_STEP_FAILED, 411);
        return None;
    }
    let _ = k2::cap_close(page);

    // Register windows, in structure order, at the addresses the link expects.
    let windows = (info.region_count as usize).min(link_addr::REGION.len());
    for (index, vaddr) in link_addr::REGION.iter().enumerate().take(windows) {
        if let Err(code) = k2::device_map_region(device, domain, index as u32, *vaddr, session) {
            k2::note(report::UNEXPECTED, code as u64);
            k2::note(note::BUILD_STEP_FAILED, 412);
            return None;
        }
    }
    if k2::device_bind_irq(device, irq, thalyx_user_k4fmt::pkg::bit::DEVICE, 0, session).is_err() {
        k2::note(note::BUILD_STEP_FAILED, 413);
        return None;
    }

    // The rings and the data buffers, mapped for the link to touch and held by
    // the link so it can grant them to the device itself.
    if k2::domain_map(
        domain,
        ring,
        link_addr::RING,
        0,
        link_addr::RING_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .is_err()
        || k2::domain_map(
            domain,
            data,
            link_addr::DATA,
            0,
            DATA_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        )
        .is_err()
    {
        k2::note(note::BUILD_STEP_FAILED, 414);
        return None;
    }
    if k2::domain_map(
        domain,
        stage,
        link_addr::STAGE,
        0,
        link_addr::STAGE_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .is_err()
    {
        k2::note(note::BUILD_STEP_FAILED, 415);
        return None;
    }

    let device_rights = right::INSPECT | right::DEVICE_MAP | right::DEVICE_IRQ | right::DEVICE_DMA;
    let installs = [
        (link_slot::DEVICE, device, device_rights),
        (link_slot::IRQ, irq, right::INSPECT | right::SIGNAL_WAIT),
        (
            link_slot::RING,
            ring,
            right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        ),
        (
            link_slot::DATA,
            data,
            right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        ),
        (
            link_slot::STAGE,
            stage,
            right::INSPECT
                | right::DERIVE
                | right::TRANSFER
                | right::MEMORY_READ
                | right::MEMORY_WRITE,
        ),
        (
            link_slot::STORE,
            store_facet,
            right::INSPECT | right::DERIVE | right::TRANSFER | right::ENDPOINT_CALL,
        ),
        (
            link_slot::WORK_PARENT,
            work_parent,
            right::INSPECT | right::SCOPE_CREATE | right::SCOPE_FENCE | right::SCOPE_LIMIT,
        ),
        (
            link_slot::SELF_SCOPE,
            scope,
            right::INSPECT | right::SCOPE_CREATE,
        ),
        (
            link_slot::LOG,
            log,
            right::INSPECT | right::LOG_APPEND | right::LOG_READ,
        ),
        (
            link_slot::READY,
            ready,
            right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
        ),
    ];
    for (slot, handle, rights) in installs {
        if k2::domain_install_cap(domain, handle, slot, rights, 0).is_err() {
            k2::note(note::BUILD_STEP_FAILED, 416);
            k2::note(report::UNEXPECTED, u64::from(slot));
            return None;
        }
    }
    if k2::domain_set_fault_channel(domain, supervision).is_err() {
        k2::note(note::BUILD_STEP_FAILED, 417);
        return None;
    }
    if k2::domain_activate(domain).is_err() {
        k2::note(note::BUILD_STEP_FAILED, 418);
        return None;
    }

    // Bus mastering last, and by the authority that keeps it: until now the
    // device cannot issue a transaction whatever the link writes.
    if let Err(code) = k2::device_set_master(device, true, session) {
        k2::note(report::UNEXPECTED, code as u64);
        k2::note(note::BUILD_STEP_FAILED, 419);
        return None;
    }

    let _ = k2::cap_close(irq);
    let _ = k2::cap_close(ring);
    let _ = k2::cap_close(data);
    let _ = k2::cap_close(stage);
    let _ = k2::cap_close(store_facet);
    let _ = k2::cap_close(work_parent);
    let scope_view = k2::derive(scope, right::INSPECT, 0, 0).unwrap_or(0);
    let _ = k2::cap_close(scope);
    let _ = status::OK;

    Some(Link {
        domain,
        ready,
        scope: scope_view,
    })
}
