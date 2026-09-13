//! The `thalyx` stage: the K1 backend, driven by the real Thalyx.
//!
//! The other stages of the port drive the vertical from inside the machine --
//! a native program, a native tool, a resident engine. This stage drives
//! nothing: it stands up the durable state service on the K4 driver, then the
//! link that carries Thalyx's managed protocol in over a virtio-console
//! function, and waits. The consumer is the real Thalyx, on the host, reaching
//! this machine over the socket QEMU exposes -- which is exactly what EXP-13's
//! third arm is: the same Thalyx, over the same platform boundary, on this
//! kernel instead of on `thalyx-managed`.
//!
//! What runs where is the whole point of the arm, so it is explicit: the state
//! service, the block driver, the medium, the work scopes and the transport
//! are this kernel's; Thalyx's agent, QuickJS, tools, validation and answer are
//! the host's, exactly as they are for `linux-managed`. This stage builds the
//! machine side and gets out of the way.

use thalyx_abi::{boot_handle, boot_slot, domain_state, right};
use thalyx_user_k4fmt::pkg::{IOBUF_PAGES, StoreConfig, bit as k4bit, facet};
use thalyx_user_k5pkg::link::MAX_PORTS;
use thalyx_user_rt::k2::{self, name16};

use crate::audit::{self, Audit};
use crate::note;
use crate::services::{self, SUPER_IOBUF_VADDR, StoreParts};
use crate::thalyxlink;

/// How long the whole stage may run before the supervisor stops waiting. The
/// host drives it; a run whose consumer never connects ends rather than hangs.
const RUN_DEADLINE_NS: u64 = 600_000_000_000;
/// How often the link and the log are looked at.
const POLL_NS: u64 = 20_000_000;

pub fn run(system: u64, supervision: u64, seed: u64, scenario: u64) -> bool {
    let own_domain = boot_handle(boot_slot::SELF_DOMAIN);
    let log = boot_handle(boot_slot::CONTROL_LOG);

    let (Some(disk_image), Some(store_image), Some(link_image)) = (
        crate::image_named("k5disk"),
        crate::image_named("k5store"),
        crate::image_named("k5link"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 200);
        return false;
    };

    let Ok(limits) = k2::limits() else {
        k2::note(note::BUILD_STEP_FAILED, 201);
        return false;
    };

    let Ok(disk_scope) = k2::scope_create_child(
        system,
        services::limits_for(limits.cpu_window_ns / 4, 96, 2),
        name16("disk"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 202);
        return false;
    };
    let Ok(store_scope) = k2::scope_create_child(
        system,
        services::limits_for(limits.cpu_window_ns / 2, 512, 2),
        name16("store"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 203);
        return false;
    };
    let Ok(iobuf) = k2::scope_create_memory(
        system,
        IOBUF_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("iobuf"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 204);
        return false;
    };
    let Ok(crash) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 205);
        return false;
    };
    let Ok(store_endpoint) = k2::scope_create_endpoint(system, 8, name16("store")) else {
        k2::note(note::BUILD_STEP_FAILED, 206);
        return false;
    };
    let Ok(broker_endpoint) = k2::scope_create_endpoint(system, 8, name16("broker")) else {
        k2::note(note::BUILD_STEP_FAILED, 207);
        return false;
    };

    let Some(disk_endpoint) = services::build_driver(disk_scope, supervision, iobuf, disk_image)
    else {
        k2::note(note::BUILD_STEP_FAILED, 208);
        return false;
    };
    k2::note(note::SERVICE_BUILT, 1);
    let _ = k2::cap_close(disk_image);

    // The supervisor's own view of the shared buffer, so it can read the run
    // directive the host wrote outside the store.
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
        k2::note(note::BUILD_STEP_FAILED, 209);
        return false;
    }
    let Some(super_disk) = crate::bind_facet(disk_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 210);
        return false;
    };
    let directive = services::read_directive(super_disk.0);
    let (leg, medium_scenario) = match directive {
        Some(directive) => (u64::from(directive.leg), directive.scenario),
        None => (1, scenario),
    };
    k2::note(
        note::DIRECTIVE,
        directive.map_or(0, |d| {
            u64::from(d.fault_point)
                | (u64::from(d.fault_mode) << 8)
                | (u64::from(d.leg) << 16)
                | (d.scenario << 24)
        }),
    );
    let _ = k2::cap_close(super_disk.0);

    let Some(disk_facet) = crate::bind_facet(disk_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 211);
        return false;
    };
    let Some(broker_facet) =
        crate::bind_facet(broker_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 212);
        return false;
    };

    // The link binds its own writer facet inside `thalyxlink::build`, and it is
    // the first facet bound on this endpoint, so it is the state service's
    // PUBLISHER -- the one principal the medium's single writer needs. The
    // Thalyx-level principals are enforced above K4, by the link's store and by
    // the work scopes.
    let _ = facet::PUBLISHER;

    let parts = StoreParts {
        scope: store_scope,
        image: store_image,
        endpoint: store_endpoint,
        disk_facet: disk_facet.0,
        iobuf,
        log,
        crash,
        broker_facet: broker_facet.0,
        supervision,
    };
    let Some(store_domain) = services::build_store(
        &parts,
        StoreConfig {
            instance: 0,
            leg,
            scenario: medium_scenario,
            apply_directive: 1,
            reserved0: 0,
            reserved1: 0,
        },
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 214);
        return false;
    };
    k2::note(note::SERVICE_BUILT, 2);
    let _ = k2::cap_close(store_image);
    let _ = k2::cap_close(store_scope);
    let _ = k2::cap_close(store_domain);
    let _ = k2::cap_close(disk_scope);
    let _ = k2::cap_close(disk_facet.0);
    let _ = k2::cap_close(broker_facet.0);
    let _ = k2::cap_close(broker_endpoint);
    let _ = k2::cap_close(disk_endpoint);

    // The service recovers whatever the medium left it before it admits
    // anything.
    let mut auditor = Audit::default();
    let ready_by = k2::now_ns() + 60_000_000_000;
    let mut ready = false;
    while k2::now_ns() < ready_by {
        audit::observe(log, &mut auditor);
        audit::drain(log, &mut auditor);
        if let Ok(info) =
            k2::signal_wait(crash, k4bit::READY | k4bit::CRASH, k2::now_ns() + POLL_NS)
        {
            if info.bits & k4bit::CRASH != 0 {
                audit::drain(log, &mut auditor);
                audit::report(&auditor);
                k2::note(note::CUT, 0);
                return true;
            }
            if info.bits & k4bit::READY != 0 {
                ready = true;
                break;
            }
        }
    }
    if !ready {
        k2::note(note::BUILD_STEP_FAILED, 215);
        return false;
    }
    k2::note(note::STORE_READY, 1);

    // The port-to-line mapping the scenario asks for. `lines` (the default)
    // gives every worker port its own line; `rivals` puts two ports on one
    // line so that two principals contend on one generation and a stale
    // publication is observed.
    let mut port_line = [0u32; MAX_PORTS as usize + 2];
    if medium_scenario == RIVALS_SCENARIO {
        port_line[1] = 1;
        port_line[2] = 1;
    }

    let Some(link) = thalyxlink::build(
        system,
        supervision,
        link_image,
        store_endpoint,
        log,
        seed,
        medium_scenario,
        port_line,
        limits.cpu_window_ns,
    ) else {
        return false;
    };
    let _ = k2::cap_close(link_image);
    let _ = k2::cap_close(store_endpoint);
    let _ = k2::cap_close(own_domain);
    k2::note(note::SERVICE_BUILT, 4);

    // Wait for the link: it ends when the host asks it to over the control
    // line, or when the deadline is reached. The auditor keeps the control
    // plane drained so the kernel never stops admitting the work the link asks
    // for.
    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut ended = None;
    while k2::now_ns() < deadline {
        audit::drain(log, &mut auditor);
        if let Ok(state) = k2::domain_query(link.domain)
            && (state.state == domain_state::DEAD || state.state == domain_state::FAULTED)
        {
            if state.faults != 0 {
                k2::note(note::DOMAIN_FAULT, state.fault_vector);
            }
            ended = Some(state.exit_code);
            break;
        }
        let _ = k2::signal_wait(link.ready, k4bit::CRASH, k2::now_ns() + POLL_NS);
    }
    audit::drain(log, &mut auditor);
    audit::report(&auditor);

    if link.scope != 0
        && let Ok(info) = k2::scope_query(link.scope)
    {
        k2::note(note::SCOPE_PAGES, info.memory_pages_used);
        k2::note(note::SCOPE_CPU, info.cpu_total_ns);
    }
    match ended {
        Some(code) => {
            k2::note(note::DOMAIN_STOPPED, code);
            code == 0
        }
        None => {
            k2::note(note::DOMAIN_STOPPED, u64::MAX);
            // A run the host never ended is not a failure of the machine: the
            // link stood up and served until the deadline.
            true
        }
    }
}

/// The scenario number the medium's directive carries for the rival mapping.
/// Kept here rather than shared with K5's own scenarios, which this stage does
/// not run.
const RIVALS_SCENARIO: u64 = 1;
