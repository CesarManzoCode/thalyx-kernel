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
    ScopeLimits, boot_slot, dma_profile, domain_state, memory_state, right, status,
};
use thalyx_user_k4fmt as k4;
use thalyx_user_k4fmt::pkg::{
    CONFIG_VADDR, ClientConfig, DiskReply, DiskRequest, IOBUF_PAGES, IOBUF_VADDR, REGION_VADDR,
    RING_PAGES, RING_VADDR, STAGE_PAGES, STAGE_VADDR, STORE_CONFIG_VADDR, StoreConfig, bit,
    client_slot, disk_op, disk_slot, disk_status, facet, note, role, store_slot,
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

fn limits_for(cpu_budget_ns: u64, memory_pages: u64, parallelism: u32) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 48,
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
#[allow(clippy::too_many_arguments)]
fn build_driver(scope: u64, supervision: u64, iobuf: u64, log: u64, image: u64) -> Option<u64> {
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
    let endpoint = k2::scope_create_endpoint(scope, 16, name16("disk")).ok()?;

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
    k2::domain_install_cap(
        domain,
        log,
        disk_slot::SCOPE,
        right::INSPECT | right::LOG_APPEND,
        0,
    )
    .ok()?;
    k2::domain_set_fault_channel(domain, supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    let _ = k2::cap_close(ring);

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
    let domain =
        k2::scope_create_domain(run.store_scope, run.store_image, name16("k4store")).ok()?;
    let page = config_object(run.store_scope, "storecfg", &config)?;
    k2::domain_map(domain, page, STORE_CONFIG_VADDR, 0, 1, right::MEMORY_READ).ok()?;
    let _ = k2::cap_close(page);
    k2::domain_map(
        domain,
        run.iobuf,
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
        run.disk_facet,
        store_slot::DISK,
        right::INSPECT | right::ENDPOINT_CALL,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        run.iobuf,
        store_slot::IOBUF,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        run.store_endpoint,
        store_slot::SERVICE,
        right::INSPECT | right::ENDPOINT_RECEIVE,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        run.log,
        store_slot::LOG,
        right::INSPECT | right::LOG_APPEND | right::LOG_READ,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        run.broker_facet,
        store_slot::BROKER,
        right::INSPECT | right::ENDPOINT_CALL,
        0,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        run.signal,
        store_slot::CRASH,
        right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
        0,
    )
    .ok()?;
    k2::domain_set_fault_channel(domain, run.supervision).ok()?;
    k2::domain_activate(domain).ok()?;
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
) -> Option<u64> {
    let domain = k2::scope_create_domain(run.client_scope, image, name16(name)).ok()?;
    let page = config_object(run.client_scope, "clientcfg", &config)?;
    k2::domain_map(domain, page, CONFIG_VADDR, 0, 1, right::MEMORY_READ).ok()?;
    let _ = k2::cap_close(page);

    let stage = k2::scope_create_memory(
        run.client_scope,
        STAGE_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("stage"),
    )
    .ok()?;
    k2::domain_map(
        domain,
        stage,
        STAGE_VADDR,
        0,
        STAGE_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
    )
    .ok()?;
    k2::domain_install_cap(
        domain,
        stage,
        client_slot::STAGE,
        right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        0,
    )
    .ok()?;
    let _ = k2::cap_close(stage);

    if config.role != role::BROKER {
        // Its own facet of the one service endpoint. The facet is the
        // principal: this is where a client's identity comes from.
        let facet = k2::endpoint_bind_facet(
            run.store_endpoint,
            config.principal,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        )
        .ok()?;
        k2::domain_install_cap(
            domain,
            facet,
            client_slot::STORE,
            right::INSPECT | right::ENDPOINT_CALL,
            0,
        )
        .ok()?;
        let _ = k2::cap_close(facet);
    } else {
        k2::domain_install_cap(
            domain,
            broker_endpoint,
            client_slot::SERVICE,
            right::INSPECT | right::ENDPOINT_RECEIVE,
            0,
        )
        .ok()?;
    }
    k2::domain_install_cap(
        domain,
        done,
        client_slot::DONE,
        right::INSPECT | right::SIGNAL_RAISE,
        0,
    )
    .ok()?;
    k2::domain_set_fault_channel(domain, run.supervision).ok()?;
    k2::domain_activate(domain).ok()?;
    Some(domain)
}

/// Waits for the service to say it is admitting requests.
fn await_ready(signal: u64) -> bool {
    let deadline = k2::now_ns() + READY_DEADLINE_NS;
    while k2::now_ns() < deadline {
        if let Ok(info) = k2::signal_query(signal) {
            if info.bits & bit::READY != 0 {
                return true;
            }
            if info.bits & (bit::CRASH | bit::RESTART) != 0 {
                return false;
            }
        }
        let _ = k2::signal_wait(
            signal,
            bit::READY | bit::CRASH | bit::RESTART,
            k2::now_ns() + POLL_NS,
        );
    }
    false
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
    let store_endpoint = match k2::scope_create_endpoint(own_scope, 16, name16("store")) {
        Ok(handle) => handle,
        Err(code) => fail(12, code),
    };
    let broker_endpoint = match k2::scope_create_endpoint(own_scope, 8, name16("broker")) {
        Ok(handle) => handle,
        Err(code) => fail(13, code),
    };
    k2::note(report::BUILT, 1);

    let Some(disk_endpoint) = build_driver(disk_scope, supervision, iobuf, log, disk_image) else {
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
    let super_disk =
        match k2::endpoint_bind_facet(disk_endpoint, 0, right::INSPECT | right::ENDPOINT_CALL, 0) {
            Ok(handle) => handle,
            Err(code) => fail(16, code),
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

    let disk_facet =
        match k2::endpoint_bind_facet(disk_endpoint, 1, right::INSPECT | right::ENDPOINT_CALL, 0) {
            Ok(handle) => handle,
            Err(code) => fail(17, code),
        };
    let broker_facet =
        match k2::endpoint_bind_facet(broker_endpoint, 1, right::INSPECT | right::ENDPOINT_CALL, 0)
        {
            Ok(handle) => handle,
            Err(code) => fail(18, code),
        };

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
        fail(19, status::INVALID_HANDLE)
    };
    run.store_domain = store_domain;
    k2::note(note::SUPER_BUILT, 2);

    if !await_ready(signal) {
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
        match build_client(&run, client_image, name, config, done, broker_endpoint) {
            Some(domain) => {
                clients[built] = domain;
                built += 1;
            }
            None => fail(20 + built as u64, status::INVALID_HANDLE),
        }
    }
    k2::note(note::SUPER_BUILT, 4);

    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut restarts = 0u64;
    loop {
        let _ = k2::signal_wait(done, bit::CLIENT_DONE, k2::now_ns() + POLL_NS);
        if let Ok(info) = k2::signal_query(signal) {
            if info.bits & bit::CRASH != 0 {
                // The service asked for the run to end at a named point. It
                // ends here, with nothing further written, which is the whole
                // value of the request.
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
                    None => fail(30, status::INVALID_HANDLE),
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

    k2::note(note::SUPER_FINISHED, 1);
    rt::exit(0)
}

rt::entry!(run);
