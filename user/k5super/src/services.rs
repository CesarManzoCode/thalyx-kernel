//! The two services K5 stands on, built the way K4 built them.
//!
//! The block driver and the managed-state service are K4's programs, unchanged
//! and rebuilt from the same sources into this package. That is the point: K5's
//! obligation is to put Thalyx's semantics on the durable service that exists,
//! and a port that grew a second persistence mechanism next to the first would
//! have avoided the interesting half of the problem.
//!
//! What differs from K4's supervisor is who the service serves. There, four
//! clients of a protocol; here, work domains of a port, each with its own
//! principal facet, and a launcher and an engine beside them that K4 had no
//! reason to have.

use thalyx_abi::boot_handle;
use thalyx_abi::{ScopeLimits, boot_slot, dma_profile, right, status};
use thalyx_user_k4fmt::generated::geometry;
use thalyx_user_k4fmt::pkg::{DiskReply, DiskRequest, StoreConfig, disk_op, disk_status};
use thalyx_user_k4fmt::pkg::{
    IOBUF_PAGES, IOBUF_VADDR, RING_PAGES, RING_VADDR, STORE_CONFIG_VADDR, bit, disk_slot,
    store_slot,
};
use thalyx_user_k4fmt::{self as k4, Pod};
use thalyx_user_rt::k2::{self, name16, report};

/// Where the supervisor maps the shared block buffer in its own domain.
pub const SUPER_IOBUF_VADDR: u64 = 0x3000_0000;
/// Which block of that buffer the supervisor reads the run directive into.
pub const DIRECTIVE_SLOT: u32 = 3;
/// Bytes of one block.
pub const BLOCK: usize = 4096;
/// How long a call to the block driver may take.
pub const DISK_DEADLINE_NS: u64 = 4_000_000_000;

/// Where the driver finds each device register window.
pub const REGION_VADDR: [u64; 4] = thalyx_user_k4fmt::pkg::REGION_VADDR;

/// Ceilings for a scope of this package.
///
/// Metadata is the one that bites: it counts grants, mappings, invocations and
/// messages, and a work domain of this port makes far more of all four than a
/// K4 client did. The first version of this file used ninety-six and the
/// vertical died two thirds of the way through with `LIMIT_EXHAUSTED` coming
/// back from `ENDPOINT_CALL` -- which the state service then reported as
/// `NOT_FOUND`, because a service whose own read of the medium was refused
/// cannot show that an object is reachable.
pub fn limits_for(cpu_budget_ns: u64, memory_pages: u64, parallelism: u32) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 400,
        cpu_budget_ns,
        queue_bytes: 16384,
        closure_reserve_ns: 800_000,
        parallelism,
        reserved0: 0,
    }
}

/// Writes a structure into a fresh one-page object.
pub fn config_object<T: Pod>(scope: u64, label: &str, value: &T) -> Option<u64> {
    let object = k2::scope_create_memory(
        scope,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16(label),
    )
    .ok()?;
    k2::memory_write(object, 0, value.as_bytes()).ok()?;
    Some(object)
}

/// The supervisor's own view of one block of the shared buffer.
pub fn iobuf_slot(slot: u32) -> &'static [u8] {
    // SAFETY: the supervisor mapped the buffer writable in its own domain at
    // this address before reading it, and the offset is inside the mapping.
    unsafe {
        core::slice::from_raw_parts(
            (SUPER_IOBUF_VADDR + u64::from(slot) * BLOCK as u64) as *const u8,
            BLOCK,
        )
    }
}

/// Builds the block driver and starts it. Answers with the endpoint it serves.
pub fn build_driver(scope: u64, supervision: u64, iobuf: u64, image: u64) -> Option<u64> {
    let device = boot_handle(boot_slot::FIRST_DEVICE);
    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(_) => {
            k2::note(report::DEVICE_OBSERVED, 0);
            return None;
        }
    };
    k2::note(report::DEVICE_OBSERVED, u64::from(info.session));
    if info.dma_profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(info.dma_profile));
    }

    let domain = k2::scope_create_domain(scope, image, name16("k5disk")).ok()?;
    let irq = k2::scope_create_signal(scope).ok()?;
    let ring = k2::scope_create_memory(
        scope,
        RING_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("ring"),
    )
    .ok()?;
    let endpoint = k2::scope_create_endpoint(scope, 8, name16("disk")).ok()?;

    let windows = (info.region_count as usize).min(REGION_VADDR.len());
    for (index, vaddr) in REGION_VADDR.iter().enumerate().take(windows) {
        match k2::device_map_region(device, domain, index as u32, *vaddr, info.session) {
            Ok(_) => k2::note(report::REGION_MAPPED, *vaddr),
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return None;
            }
        }
    }
    match k2::device_bind_irq(device, irq, bit::DEVICE, 0, info.session) {
        Ok(vector) => k2::note(report::IRQ_BOUND, vector),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return None;
        }
    }

    k2::domain_map(
        domain,
        ring,
        RING_VADDR,
        0,
        RING_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;
    k2::domain_map(
        domain,
        iobuf,
        IOBUF_VADDR,
        0,
        IOBUF_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;

    // Mapping, interrupts and DMA are the driver's business. Bus mastering and
    // reset are not, and the narrowing here is what makes that true.
    let driver_rights = right::INSPECT | right::DEVICE_MAP | right::DEVICE_IRQ | right::DEVICE_DMA;
    k2::domain_install_cap(domain, device, disk_slot::DEVICE, driver_rights, 0).ok()?;
    k2::domain_install_cap(
        domain,
        irq,
        disk_slot::IRQ,
        right::INSPECT | right::SIGNAL_WAIT,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        ring,
        disk_slot::RING,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        iobuf,
        disk_slot::IOBUF,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        endpoint,
        disk_slot::SERVICE,
        right::INSPECT | right::ENDPOINT_RECEIVE,
        0,
    )
    .ok()?;
    k2::domain_set_fault_channel(domain, supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    let _ = k2::cap_close(ring);
    let _ = k2::cap_close(domain);
    let _ = k2::cap_close(irq);

    // Bus mastering last, and by the authority that keeps it. Until this, the
    // device cannot issue a transaction whatever the driver writes.
    match k2::device_set_master(device, true, info.session) {
        Ok(_) => k2::note(report::BUILT, 100),
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            return None;
        }
    }
    Some(endpoint)
}

/// Reads the run directive through the driver, into the supervisor's own
/// mapping of the shared buffer.
///
/// Scaffolding, and marked as such where K4 marks it: the block is outside the
/// store, carries its own magic, and is named by no structure of the format.
/// It is how the host varies a run without rebuilding an image.
pub fn read_directive(disk: u64) -> Option<k4::HarnessDirective> {
    let request = DiskRequest {
        op: disk_op::READ,
        block: geometry::HARNESS_DIRECTIVE_BLOCK,
        count: 1,
        buffer_block: DIRECTIVE_SLOT,
        ..DiskRequest::zeroed()
    };
    let result = k2::endpoint_call(
        disk,
        1,
        request.as_bytes(),
        &[],
        k2::now_ns() + DISK_DEADLINE_NS,
        false,
    )
    .ok()?;
    let reply = DiskReply::read_from(&result.payload, 0)?;
    if reply.status != disk_status::OK {
        return None;
    }
    k4::parse_harness(iobuf_slot(DIRECTIVE_SLOT))
}

/// What a state service needs to be built.
pub struct StoreParts {
    /// The scope it is charged to.
    pub scope: u64,
    /// Its image.
    pub image: u64,
    /// The endpoint it receives on.
    pub endpoint: u64,
    /// A facet of the block driver.
    pub disk_facet: u64,
    /// The shared block buffer.
    pub iobuf: u64,
    /// The control log.
    pub log: u64,
    /// The signal it raises to ask the run to end or be replaced.
    pub crash: u64,
    /// A facet of a broker, for outbox intents.
    pub broker_facet: u64,
    /// The endpoint faults are reported on.
    pub supervision: u64,
}

/// Builds one state service over the medium and activates it.
pub fn build_store(parts: &StoreParts, config: StoreConfig) -> Option<u64> {
    let domain = k2::scope_create_domain(parts.scope, parts.image, name16("k5store")).ok()?;
    let page = config_object(parts.scope, "storecfg", &config)?;
    k2::domain_map(domain, page, STORE_CONFIG_VADDR, 0, 1, right::MEMORY_READ).ok()?;
    let _ = k2::cap_close(page);
    k2::domain_map(
        domain,
        parts.iobuf,
        IOBUF_VADDR,
        0,
        IOBUF_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;

    // A facet of the driver's endpoint, and nothing else that reaches a
    // device. This is the whole of the service's access to persistence.
    k2::domain_install_cap(
        domain,
        parts.disk_facet,
        store_slot::DISK,
        right::INSPECT | right::ENDPOINT_CALL,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.iobuf,
        store_slot::IOBUF,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.endpoint,
        store_slot::SERVICE,
        right::INSPECT | right::ENDPOINT_RECEIVE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.log,
        store_slot::LOG,
        right::INSPECT | right::LOG_APPEND | right::LOG_READ,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.broker_facet,
        store_slot::BROKER,
        right::INSPECT | right::ENDPOINT_CALL,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        parts.crash,
        store_slot::CRASH,
        right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
        0,
    )
    .ok()?;
    k2::domain_set_fault_channel(domain, parts.supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    let _ = status::OK;
    Some(domain)
}
