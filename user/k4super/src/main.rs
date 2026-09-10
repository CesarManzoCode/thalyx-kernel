//! K4 user domain `k4super`: the supervisor that builds the durable-state run.
//!
//! It receives the same authority a K3 supervisor did — its own domain, its own
//! scope, the control log, the sealed images, and a capability over one device
//! — and makes everything else through the interface.
//!
//! What it arranges, and why the arrangement is the claim:
//!
//! * **A service that reaches the medium only through a driver.** The state
//!   service has no device capability at all. It has a facet of the driver's
//!   endpoint and a buffer they share, and that is the whole of its access to
//!   persistence. A service that could touch the device could not be cut at a
//!   named write.
//! * **A driver that cannot re-arm what stops it.** Bus mastering and reset
//!   stay with the supervisor, exactly as in K3.
//! * **Principals that are facets, not fields.** Each client reaches the
//!   service through its own facet of one endpoint. Nothing a client sends says
//!   who it is, because the kernel already said.
//! * **A run that can be cut and continued.** The directive on the medium names
//!   a point; when the service reaches it, it asks for the run to end or for
//!   itself to be replaced. Ending is this domain exiting. Replacing is this
//!   domain building a second service over the same medium, which recovers from
//!   what is actually there.
//!
//! The supervisor reads the directive itself, once, through the driver, because
//! it has to know which leg of a scenario it is building before it builds it.
//! It reads a block that is outside the store and that no format rule names.

#![no_std]
#![no_main]

use thalyx_abi::boot_handle;
use thalyx_abi::generated::{
    ScopeLimits, boot_slot, dma_profile, domain_state, memory_state, receipt_kind, right, status,
};
use thalyx_user_k4fmt as k4;
use thalyx_user_k4fmt::pkg::{
    CONFIG_VADDR, ClientConfig, DiskReply, DiskRequest, IOBUF_PAGES, IOBUF_VADDR, PRINCIPALS,
    REGION_VADDR, RING_PAGES, RING_VADDR, STAGE_PAGES, STAGE_VADDR, STORE_CONFIG_VADDR,
    StoreConfig, bit, client_slot, disk_op, disk_slot, disk_status, facet, note, role, store_slot,
};
use thalyx_user_k4fmt::{BLOCK, Pod, geometry};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, name16, report};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0xD91E_4444_5555_7001;

/// Module images the package may hold.
const MAX_IMAGES: u32 = 5;

/// Where the supervisor maps the shared buffer in its own domain, to read the
/// directive out of it. The same address the other two use, because there is
/// only one buffer and no reason to name it differently.
const SUPER_IOBUF_VADDR: u64 = IOBUF_VADDR;

/// The buffer slot the directive is read into.
const DIRECTIVE_SLOT: u32 = 3;

/// How long the supervisor waits for the driver to answer.
const DISK_DEADLINE_NS: u64 = 6_000_000_000;

/// How long it waits for the service to say it is admitting requests.
const READY_DEADLINE_NS: u64 = 20_000_000_000;

/// One poll of the run's signals.
const POLL_NS: u64 = 100_000_000;

/// The whole run, however it ends.
const RUN_DEADLINE_NS: u64 = 60_000_000_000;

/// Publications the publisher attempts when the scenario does not say.
const DEFAULT_ROUNDS: u64 = 3;

/// Clients this supervisor builds.
const CLIENTS: usize = 4;

fn fail(step: u64, code: i64) -> ! {
    k2::note(report::BUILD_FAILED, step);
    k2::note(report::UNEXPECTED, code as u64);
    rt::exit(step)
}

fn image_named(label: &str) -> Option<u64> {
    let wanted = name16(label);
    for offset in 0..MAX_IMAGES {
        let handle = boot_handle(boot_slot::FIRST_MODULE + offset);
        let Ok(info) = k2::memory_query(handle) else {
            continue;
        };
        if info.state == memory_state::SEALED && info.label == wanted {
            return Some(handle);
        }
    }
    None
}

/// Releases a handle this supervisor will not name again.
///
/// A handle is a grant, and grants are a fixed global resource. A supervisor
/// that keeps every handle it ever made runs the machine out of them, and the
/// first thing to fail is whatever needed one next -- which is not where the
/// mistake was. Closing at the point of last use keeps the failure and the
/// cause in the same place.
fn drop_handle(handle: u64) {
    let _ = k2::cap_close(handle);
}

/// Reports which build step refused, and with what, before giving up.
///
/// A supervisor that answers "it did not work" is not evidence of anything.
/// Every step that can refuse says which one it was and what the kernel
/// answered, so a failed run names its own cause.
fn step(which: u64, outcome: Result<u64, i64>) -> Option<u64> {
    match outcome {
        Ok(value) => Some(value),
        Err(code) => {
            k2::note(report::BUILD_FAILED, which);
            k2::note(report::UNEXPECTED, code as u64);
            None
        }
    }
}

/// Binds a facet on an endpoint and says which one the kernel assigned.
///
/// The facet number is the kernel's to give, not the caller's to choose: the
/// request carries zero and the grant comes back carrying the number. That is
/// what makes a facet an identity rather than a claim, and it is why the
/// supervisor checks the number it got instead of assuming that the order it
/// bound them in produced the one it wanted.
fn bind_facet(endpoint: u64, rights: u32) -> Option<(u64, u64)> {
    // The supervisor's own handle on the facet carries `TRANSFER`, because
    // installing it into a domain is a transfer. What the domain then gets is
    // narrower: a client cannot pass its own identity on to anyone.
    let handle = k2::endpoint_bind_facet(endpoint, 0, rights | right::TRANSFER, 0).ok()?;
    let info = k2::cap_inspect(handle).ok()?;
    Some((handle, info.facet))
}

fn limits_for(cpu_budget_ns: u64, memory_pages: u64, parallelism: u32) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 200,
        cpu_budget_ns,
        queue_bytes: 16384,
        closure_reserve_ns: 800_000,
        parallelism,
        reserved0: 0,
    }
}

/// Writes a structure into a fresh one-page object.
fn config_object<T: Pod>(scope: u64, label: &str, value: &T) -> Option<u64> {
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

/// The supervisor's own view of the shared buffer.
fn iobuf_slot(slot: u32) -> &'static [u8] {
    // SAFETY: the supervisor mapped the buffer writable in its own domain at
    // this address before reading it, and the offset is inside the mapping.
    unsafe {
        core::slice::from_raw_parts(
            (SUPER_IOBUF_VADDR + u64::from(slot) * BLOCK as u64) as *const u8,
            BLOCK,
        )
    }
}

/// Everything the run needs to name later.
struct Run {
    log: u64,
    supervision: u64,
    store_scope: u64,
    client_scope: u64,
    store_image: u64,
    store_endpoint: u64,
    disk_facet: u64,
    broker_facet: u64,
    iobuf: u64,
    signal: u64,
    store_domain: u64,
    instances: u64,
}

/// Builds the block driver and starts it. Returns the endpoint it serves on.
fn build_driver(scope: u64, supervision: u64, iobuf: u64, image: u64) -> Option<u64> {
    let device = boot_handle(boot_slot::FIRST_DEVICE);
    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(_) => {
            // No device on this machine. Said once, plainly: there is no
            // durable state to demonstrate without one.
            k2::note(report::DEVICE_OBSERVED, 0);
            return None;
        }
    };
    k2::note(report::DEVICE_OBSERVED, u64::from(info.session));
    if info.dma_profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(info.dma_profile));
    }

    let domain = k2::scope_create_domain(scope, image, name16("k4disk")).ok()?;
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
    // A window this device does not have, and a session it is not in.
    k2::expect_refusal(
        k2::device_map_region(device, domain, 9, REGION_VADDR[0], info.session),
        status::INVALID_ARGUMENT,
    );
    k2::expect_refusal(
        k2::device_map_region(device, domain, 0, REGION_VADDR[0], info.session + 5),
        status::STATE_CONFLICT,
    );

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
    // The driver holds what it was given. The supervisor keeps the device and
    // the shared buffer because it uses both later; the ring, the driver's
    // domain and its own copy of the interrupt it will not name again.
    drop_handle(ring);
    drop_handle(domain);
    drop_handle(irq);

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

/// Reads the fault directive through the driver, into the supervisor's own
/// mapping of the shared buffer.
///
/// Scaffolding. The block it reads is outside the store, carries its own magic,
/// and is named by no structure of the format.
fn read_directive(disk: u64) -> Option<k4::HarnessDirective> {
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

/// Builds one state service over the medium and activates it.
fn build_store(run: &mut Run, config: StoreConfig) -> Option<u64> {
    let domain = step(
        101,
        k2::scope_create_domain(run.store_scope, run.store_image, name16("k4store")),
    )?;
    let page = config_object(run.store_scope, "storecfg", &config)?;
    step(
        102,
        k2::domain_map(domain, page, STORE_CONFIG_VADDR, 0, 1, right::MEMORY_READ),
    )?;
    let _ = k2::cap_close(page);
    step(
        103,
        k2::domain_map(
            domain,
            run.iobuf,
            IOBUF_VADDR,
            0,
            IOBUF_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        ),
    )?;

    // A facet of the driver's endpoint, and nothing else that reaches a
    // device. This is the whole of the service's access to persistence.
    step(
        104,
        k2::domain_install_cap(
            domain,
            run.disk_facet,
            store_slot::DISK,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        ),
    )?;
    step(
        105,
        k2::domain_install_cap(
            domain,
            run.iobuf,
            store_slot::IOBUF,
            right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
            0,
        ),
    )?;
    step(
        106,
        k2::domain_install_cap(
            domain,
            run.store_endpoint,
            store_slot::SERVICE,
            right::INSPECT | right::ENDPOINT_RECEIVE,
            0,
        ),
    )?;
    step(
        107,
        k2::domain_install_cap(
            domain,
            run.log,
            store_slot::LOG,
            right::INSPECT | right::LOG_APPEND | right::LOG_READ,
            0,
        ),
    )?;
    step(
        108,
        k2::domain_install_cap(
            domain,
            run.broker_facet,
            store_slot::BROKER,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        ),
    )?;
    step(
        109,
        k2::domain_install_cap(
            domain,
            run.signal,
            store_slot::CRASH,
            right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
            0,
        ),
    )?;
    step(110, k2::domain_set_fault_channel(domain, run.supervision))?;
    step(111, k2::domain_activate(domain))?;
    run.instances += 1;
    Some(domain)
}

/// Builds one client domain and activates it.
fn build_client(
    run: &Run,
    image: u64,
    name: &str,
    config: ClientConfig,
    done: u64,
    broker_endpoint: u64,
    store_facet: u64,
) -> Option<u64> {
    let domain = step(
        201,
        k2::scope_create_domain(run.client_scope, image, name16(name)),
    )?;
    let page = config_object(run.client_scope, "clientcfg", &config)?;
    step(
        202,
        k2::domain_map(domain, page, CONFIG_VADDR, 0, 1, right::MEMORY_READ),
    )?;
    let _ = k2::cap_close(page);

    let stage = step(
        203,
        k2::scope_create_memory(
            run.client_scope,
            STAGE_PAGES,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
            name16("stage"),
        ),
    )?;
    step(
        204,
        k2::domain_map(
            domain,
            stage,
            STAGE_VADDR,
            0,
            STAGE_PAGES as u32,
            right::MEMORY_READ | right::MEMORY_WRITE,
        ),
    )?;
    step(
        205,
        k2::domain_install_cap(
            domain,
            stage,
            client_slot::STAGE,
            // Narrowing this buffer and handing the narrowing over are what a
            // client does with it on every call, so both rights are here. What
            // it lends the service each time is never this handle.
            right::INSPECT
                | right::MEMORY_READ
                | right::MEMORY_WRITE
                | right::DERIVE
                | right::TRANSFER,
            0,
        ),
    )?;
    let _ = k2::cap_close(stage);

    if config.role != role::BROKER {
        // Its own facet of the one service endpoint, bound before any client
        // existed and checked then. The facet is the principal: this is where a
        // client's identity comes from, and nothing it sends can change it.
        step(
            206,
            k2::domain_install_cap(
                domain,
                store_facet,
                client_slot::STORE,
                right::INSPECT | right::ENDPOINT_CALL,
                0,
            ),
        )?;
    } else {
        step(
            207,
            k2::domain_install_cap(
                domain,
                broker_endpoint,
                client_slot::SERVICE,
                right::INSPECT | right::ENDPOINT_RECEIVE,
                0,
            ),
        )?;
    }
    step(
        208,
        k2::domain_install_cap(
            domain,
            done,
            client_slot::DONE,
            right::INSPECT | right::SIGNAL_RAISE,
            0,
        ),
    )?;
    step(209, k2::domain_set_fault_channel(domain, run.supervision))?;
    step(210, k2::domain_activate(domain))?;
    Some(domain)
}

/// What the auditor has seen of the control plane so far.
///
/// The service records what it did; this reads what the kernel recorded about
/// it. They are different statements, and keeping the second is what lets the
/// gate ask whether a publication the medium holds was ever admitted, rather
/// than taking the service's word for both halves.
#[derive(Default)]
struct Audit {
    /// Receipts read and acknowledged.
    drained: u64,
    /// Effect admissions among them: one per publication the kernel let begin.
    effects: u64,
    /// The sequence the next receipt must carry if coverage is unbroken.
    expected: u64,
    /// Sequences that never arrived, counted rather than assumed absent.
    gaps: u64,
    /// What the log itself says it dropped.
    lost: u32,
    /// The deepest occupancy observed.
    high_water: u32,
}

/// Consumes every receipt the log is holding, and says what was in them.
///
/// The log is a ring the kernel refuses admissions to protect: a covered
/// operation reserves a cell before it may happen, so a full log does not lose
/// receipts, it stops the machine admitting the work they would cover. Someone
/// has to consume it, and it cannot be the service being audited. It is the
/// supervisor, because the supervisor is the authority the service was built
/// by and the only domain in this package that holds the log with the right to
/// acknowledge.
///
/// Reading does not consume. Acknowledging through the last sequence read is
/// what frees the cells, and it happens after the batch has been counted, so a
/// receipt is never dropped before it has been looked at.
fn drain_receipts(log: u64, audit: &mut Audit) {
    loop {
        let Ok(batch) = k2::log_read(log) else { return };
        if batch.lost > audit.lost {
            audit.lost = batch.lost;
            k2::note(note::AUDIT_LOST, u64::from(batch.lost));
        }
        if batch.count == 0 {
            return;
        }
        let mut through = 0u64;
        for record in batch.records.iter().take(batch.count as usize) {
            if audit.expected != 0 && record.sequence != audit.expected {
                audit.gaps += 1;
                k2::note(note::AUDIT_GAP, audit.expected);
            }
            audit.expected = record.sequence + 1;
            audit.drained += 1;
            if record.kind == receipt_kind::EFFECT {
                audit.effects += 1;
                k2::note(note::AUDIT_EFFECT, record.object_id);
            }
            through = record.sequence;
        }
        if let Err(code) = k2::log_acknowledge(log, through) {
            k2::note(report::UNEXPECTED, code as u64);
            return;
        }
    }
}

/// States what the auditor saw, in the four numbers a gate reads.
///
/// Emitted on both ways out, because a run that was cut has an audit too, and
/// a cut whose receipts were never reported would leave the kernel's account
/// of the writes missing exactly where it matters most.
fn report_audit(audit: &Audit) {
    k2::note(note::AUDIT_DRAINED, audit.drained);
    k2::note(note::AUDIT_EFFECT, audit.effects);
    k2::note(
        note::AUDIT_HIGH_WATER,
        u64::from(audit.high_water) | (audit.gaps << 32),
    );
    k2::note(note::AUDIT_LOST, u64::from(audit.lost));
}

/// Reads how full the log got, before draining it.
fn observe_log(log: u64, audit: &mut Audit) {
    if let Ok(info) = k2::log_query(log)
        && info.used > audit.high_water
    {
        audit.high_water = info.used;
    }
}

/// Waits for the service to say it is admitting requests, auditing meanwhile.
///
/// A wait consumes the bits it observed, so what it returns is the only report
/// of them: querying the signal afterwards would find them already gone. That
/// is why nothing here polls the signal. The log is different -- nothing wakes
/// anyone when a receipt is written -- so the wait is taken in slices and the
/// log is drained between them. Waiting the whole deadline in one call is what
/// let the log fill while the service was still formatting the medium.
fn await_ready(signal: u64, log: u64, audit: &mut Audit) -> bool {
    let deadline = k2::now_ns() + READY_DEADLINE_NS;
    loop {
        observe_log(log, audit);
        drain_receipts(log, audit);
        let slice = (k2::now_ns() + POLL_NS).min(deadline);
        match k2::signal_wait(signal, bit::READY | bit::CRASH | bit::RESTART, slice) {
            Ok(info) => {
                k2::note(report::SIGNAL_OBSERVED, info.bits);
                if info.bits & bit::READY != 0 {
                    drain_receipts(log, audit);
                    return true;
                }
                if info.bits & (bit::CRASH | bit::RESTART) != 0 {
                    return false;
                }
            }
            Err(code) if code != status::TIMED_OUT => {
                k2::note(report::UNEXPECTED, code as u64);
                return false;
            }
            Err(_) => {}
        }
        if k2::now_ns() >= deadline {
            return false;
        }
    }
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let own_scope = boot_handle(boot_slot::SELF_SCOPE);
    let own_domain = boot_handle(boot_slot::SELF_DOMAIN);
    let log = boot_handle(boot_slot::CONTROL_LOG);

    let limits = match k2::limits() {
        Ok(limits) => limits,
        Err(code) => fail(1, code),
    };
    k2::note(report::LIMITS, limits.page_size);
    if let Ok(info) = k2::scope_query(own_scope) {
        k2::note(
            report::BOUNDED,
            info.limits.metadata_objects | (info.metadata_used << 32),
        );
    }

    let disk_image = match image_named("k4disk") {
        Some(handle) => handle,
        None => fail(2, status::INVALID_HANDLE),
    };
    let store_image = match image_named("k4store") {
        Some(handle) => handle,
        None => fail(3, status::INVALID_HANDLE),
    };
    let client_image = match image_named("k4client") {
        Some(handle) => handle,
        None => fail(4, status::INVALID_HANDLE),
    };

    let disk_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 4, 96, 2),
        name16("disk"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(5, code),
    };
    let store_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 2, 160, 2),
        name16("store"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(6, code),
    };
    let client_scope = match k2::scope_create_child(
        own_scope,
        limits_for(limits.cpu_window_ns / 2, 160, 4),
        name16("client"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(7, code),
    };

    let supervision = match k2::scope_create_endpoint(own_scope, 8, name16("faults")) {
        Ok(handle) => handle,
        Err(code) => fail(8, code),
    };
    let signal = match k2::scope_create_signal(own_scope) {
        Ok(handle) => handle,
        Err(code) => fail(9, code),
    };
    let done = match k2::scope_create_signal(own_scope) {
        Ok(handle) => handle,
        Err(code) => fail(10, code),
    };
    let iobuf = match k2::scope_create_memory(
        own_scope,
        IOBUF_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("iobuf"),
    ) {
        Ok(handle) => handle,
        Err(code) => fail(11, code),
    };
    let store_endpoint = match k2::scope_create_endpoint(own_scope, 8, name16("store")) {
        Ok(handle) => handle,
        Err(code) => fail(12, code),
    };
    let broker_endpoint = match k2::scope_create_endpoint(own_scope, 8, name16("broker")) {
        Ok(handle) => handle,
        Err(code) => fail(13, code),
    };
    k2::note(report::BUILT, 1);

    let Some(disk_endpoint) = build_driver(disk_scope, supervision, iobuf, disk_image) else {
        fail(14, status::INVALID_HANDLE)
    };
    k2::note(note::SUPER_BUILT, 1);

    // The supervisor's own view of the shared buffer, so it can read a block
    // that is not part of the store and decide which leg it is building.
    if k2::domain_map(
        own_domain,
        iobuf,
        SUPER_IOBUF_VADDR,
        0,
        IOBUF_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .is_err()
    {
        fail(15, status::INVALID_ARGUMENT);
    }
    // The mapping holds the buffer; the handle on this domain is what is now
    // redundant, and every handle is a grant the rest of the run cannot have.
    drop_handle(own_domain);
    let super_disk = match bind_facet(disk_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
        Some((handle, _)) => handle,
        None => fail(16, status::INVALID_ARGUMENT),
    };
    let directive = read_directive(super_disk);
    let (leg, scenario) = match directive {
        Some(directive) => (u64::from(directive.leg), directive.scenario),
        None => (1, 0),
    };
    k2::note(
        note::FAULT_DIRECTIVE,
        directive.map_or(0, |d| {
            u64::from(d.fault_point)
                | (u64::from(d.fault_mode) << 8)
                | (u64::from(d.leg) << 16)
                | (d.scenario << 24)
        }),
    );
    let _ = k2::cap_close(super_disk);

    let disk_facet = match bind_facet(disk_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
        Some((handle, _)) => handle,
        None => fail(17, status::INVALID_ARGUMENT),
    };
    let broker_facet = match bind_facet(broker_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
        Some((handle, _)) => handle,
        None => fail(18, status::INVALID_ARGUMENT),
    };

    // One facet per principal, bound in principal order and checked against the
    // number that came back. A client's identity is this number, so the run
    // does not start if the kernel gave a different one.
    let mut principals = [0u64; PRINCIPALS];
    for wanted in [facet::PUBLISHER, facet::RIVAL, facet::READER] {
        match bind_facet(store_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
            Some((handle, facet)) if facet == wanted => {
                principals[wanted as usize] = handle;
                k2::note(report::BOUND, facet);
            }
            Some((_, facet)) => {
                k2::note(report::UNEXPECTED, facet);
                fail(19, status::STATE_CONFLICT)
            }
            None => fail(19, status::INVALID_ARGUMENT),
        }
    }

    let mut run = Run {
        log,
        supervision,
        store_scope,
        client_scope,
        store_image,
        store_endpoint,
        disk_facet,
        broker_facet,
        iobuf,
        signal,
        store_domain: 0,
        instances: 0,
    };

    let Some(store_domain) = build_store(
        &mut run,
        StoreConfig {
            instance: 0,
            leg,
            scenario,
            apply_directive: 1,
            reserved0: 0,
            reserved1: 0,
        },
    ) else {
        fail(20, status::INVALID_HANDLE)
    };
    run.store_domain = store_domain;
    k2::note(note::SUPER_BUILT, 2);

    let mut audit = Audit::default();
    if !await_ready(signal, log, &mut audit) {
        // The service did not get as far as admitting requests. That is a
        // result of the run, not a failure of the supervisor: the medium says
        // what happened, and the gate reads the medium.
        k2::note(note::SUPER_CRASH, 0);
        rt::exit(0)
    }
    k2::note(note::SUPER_BUILT, 3);

    // Scenario 2 is the one with two publishers racing for one transition.
    let rounds = if scenario == 4 { 12 } else { DEFAULT_ROUNDS };
    let mut clients = [0u64; CLIENTS];
    let mut built = 0usize;
    let roster: [(&str, u64, u64); CLIENTS] = [
        ("k4broker", role::BROKER, 4),
        ("k4reader", role::READER, facet::READER),
        ("k4rival", role::RIVAL, facet::RIVAL),
        ("k4pub", role::PUBLISHER, facet::PUBLISHER),
    ];
    for (name, which, principal) in roster {
        if which == role::RIVAL && scenario != 2 {
            continue;
        }
        let config = ClientConfig {
            role: which,
            principal,
            scenario,
            leg,
            rounds,
            reserved0: 0,
        };
        let store_facet = if which == role::BROKER {
            0
        } else {
            principals[principal as usize]
        };
        match build_client(
            &run,
            client_image,
            name,
            config,
            done,
            broker_endpoint,
            store_facet,
        ) {
            Some(domain) => {
                clients[built] = domain;
                built += 1;
            }
            None => fail(30 + built as u64, status::INVALID_HANDLE),
        }
    }
    // Everything the supervisor bound through has been installed where it
    // belongs. What it still names is what it still uses: the store's image for
    // a replacement, the clients' domains to see them finish, the two signals,
    // the buffer, and the log.
    // Including the facets of principals this scenario did not build: a run
    // that binds four and uses three still holds four handles.
    for handle in principals {
        if handle != 0 {
            drop_handle(handle);
        }
    }
    drop_handle(broker_endpoint);
    drop_handle(store_endpoint);
    drop_handle(client_image);
    drop_handle(disk_image);
    drop_handle(disk_endpoint);
    drop_handle(disk_scope);
    drop_handle(client_scope);
    if let Ok(info) = k2::scope_query(own_scope) {
        k2::note(report::BOUNDED, info.metadata_used);
    }
    k2::note(note::SUPER_BUILT, 4);

    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut restarts = 0u64;
    loop {
        // Before anything else, because everything else in this loop waits and
        // the log fills while it does.
        observe_log(log, &mut audit);
        drain_receipts(log, &mut audit);
        let _ = k2::signal_wait(done, bit::CLIENT_DONE, k2::now_ns() + POLL_NS);
        drain_receipts(log, &mut audit);
        // Consuming rather than querying, for the reason `await_ready` gives.
        if let Ok(info) = k2::signal_wait(signal, bit::CRASH | bit::RESTART, k2::now_ns() + POLL_NS)
        {
            if info.bits & bit::CRASH != 0 {
                // The service asked for the run to end at a named point. It
                // ends here, with nothing further written, which is the whole
                // value of the request. The receipts that cover what was
                // written before the cut are read first: they are the only
                // account of it that is not the service's own.
                drain_receipts(log, &mut audit);
                report_audit(&audit);
                k2::note(note::SUPER_CRASH, run.instances);
                rt::exit(0)
            }
            if info.bits & bit::RESTART != 0 && restarts == 0 {
                restarts += 1;
                let _ = k2::domain_terminate(run.store_domain);
                match build_store(
                    &mut run,
                    StoreConfig {
                        instance: restarts,
                        leg,
                        scenario,
                        apply_directive: 0,
                        reserved0: 0,
                        reserved1: 0,
                    },
                ) {
                    Some(domain) => {
                        run.store_domain = domain;
                        k2::note(note::SUPER_RESTARTED, restarts);
                    }
                    None => fail(40, status::INVALID_HANDLE),
                }
            }
        }

        let mut finished = 0usize;
        for domain in clients.iter().take(built) {
            if let Ok(info) = k2::domain_query(*domain)
                && (info.state == domain_state::DEAD || info.state == domain_state::FAULTED)
            {
                finished += 1;
            }
        }
        if finished == built {
            break;
        }
        if k2::now_ns() > deadline {
            k2::note(report::UNEXPECTED, u64::MAX);
            break;
        }
    }

    // The last receipts, including the ones the run's own ending wrote.
    observe_log(log, &mut audit);
    drain_receipts(log, &mut audit);
    report_audit(&audit);
    k2::note(note::SUPER_FINISHED, 1);
    rt::exit(0)
}

rt::entry!(run);
