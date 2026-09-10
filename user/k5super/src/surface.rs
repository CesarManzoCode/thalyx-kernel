//! The `surface` stage: the first native Thalyx vertical.
//!
//! `vault/roadmap/phases.md` calls this K5a and says what it is worth:
//! a surface driven by fixtures and by something outside the guest, labelled a
//! partial integration. That label is the honest one and this file keeps it.
//! What runs here is Thalyx's *semantics* -- an identified version, context
//! from that version, a private workspace, a real change, a conditioned
//! publication and durable evidence -- on the managed state K4 built, with the
//! script coming from outside the guest through the medium.
//!
//! What does **not** run here is the language runtime or a real validation
//! tool. The validation record this stage stages is a claim the work makes
//! about itself, exactly as K4's client made one, and a claim is not a check.
//! The `work` stage is where that stops being true, and the gate's criteria are
//! named so that the difference cannot be read past.

use thalyx_abi::{boot_handle, boot_slot, domain_state, right, status};
use thalyx_user_k4fmt::pkg::{IOBUF_PAGES, StoreConfig, bit as k4bit, facet};
use thalyx_user_k5pkg::native::Plan;
use thalyx_user_k5pkg::proto::{WorkConfig, bit, work_addr, work_role};
use thalyx_user_rt::k2::{self, name16, report};

use crate::audit::{self, Audit};
use crate::launcher::{self, Launcher};
use crate::note;
use crate::services::{self, SUPER_IOBUF_VADDR, StoreParts};
use crate::work::{self, WorkParts};

/// How long the whole stage may take before the supervisor stops waiting.
const RUN_DEADLINE_NS: u64 = 240_000_000_000;
/// How long one wait between checks is.
const POLL_NS: u64 = 20_000_000;

/// Builds the port's services and one piece of work, and waits for it.
/// What a stage of the port asks of this builder.
pub struct Shape {
    /// Whether the work drives the language runtime.
    pub uses_runtime: bool,
    /// Whether the work may ask the resident engine.
    pub uses_engine: bool,
}

pub fn run(system: u64, supervision: u64, plan: &Plan, shape: &Shape) -> bool {
    let own_domain = boot_handle(boot_slot::SELF_DOMAIN);
    let log = boot_handle(boot_slot::CONTROL_LOG);

    let Some(disk_image) = crate::image_named("k5disk") else {
        k2::note(note::BUILD_STEP_FAILED, 100);
        return false;
    };
    let Some(store_image) = crate::image_named("k5store") else {
        k2::note(note::BUILD_STEP_FAILED, 101);
        return false;
    };
    let Some(work_image) = crate::image_named("k5work") else {
        k2::note(note::BUILD_STEP_FAILED, 102);
        return false;
    };
    let tool_image = crate::image_named("ncheck").unwrap_or(0);
    let runtime_image = crate::image_named("nhacer").unwrap_or(0);
    if shape.uses_runtime && (tool_image == 0 || runtime_image == 0) {
        k2::note(note::BUILD_STEP_FAILED, 103);
        return false;
    }

    let Ok(limits) = k2::limits() else {
        k2::note(note::BUILD_STEP_FAILED, 103);
        return false;
    };

    let Ok(disk_scope) = k2::scope_create_child(
        system,
        services::limits_for(limits.cpu_window_ns / 4, 96, 2),
        name16("disk"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 104);
        return false;
    };
    let Ok(store_scope) = k2::scope_create_child(
        system,
        services::limits_for(limits.cpu_window_ns / 2, 200, 2),
        name16("store"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 105);
        return false;
    };
    let Ok(work_scope) = k2::scope_create_child(
        system,
        services::limits_for(limits.cpu_window_ns / 2, 320, 3),
        name16("work"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 106);
        return false;
    };

    let Ok(iobuf) = k2::scope_create_memory(
        system,
        IOBUF_PAGES,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("iobuf"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 107);
        return false;
    };
    let Ok(crash) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 108);
        return false;
    };
    let Ok(done) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 109);
        return false;
    };
    let Ok(cancel) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 110);
        return false;
    };
    let Ok(store_endpoint) = k2::scope_create_endpoint(system, 8, name16("store")) else {
        k2::note(note::BUILD_STEP_FAILED, 111);
        return false;
    };
    let Ok(broker_endpoint) = k2::scope_create_endpoint(system, 8, name16("broker")) else {
        k2::note(note::BUILD_STEP_FAILED, 112);
        return false;
    };

    let Some(disk_endpoint) = services::build_driver(disk_scope, supervision, iobuf, disk_image)
    else {
        k2::note(note::BUILD_STEP_FAILED, 113);
        return false;
    };
    k2::note(note::SERVICE_BUILT, 1);
    // A handle is a grant, and a domain's table has thirty-two slots. A
    // supervisor that kept every handle it ever made runs out of table, and the
    // first thing to fail is whatever needed one next -- which is not where the
    // mistake was. Closing at the point of last use keeps the failure and the
    // cause in the same place.
    let _ = k2::cap_close(disk_image);

    // The supervisor's own view of the shared buffer, so it can read the block
    // the host wrote outside the store and know which leg it is building.
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
        k2::note(note::BUILD_STEP_FAILED, 114);
        return false;
    }
    let Some(super_disk) = crate::bind_facet(disk_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 115);
        return false;
    };
    let directive = services::read_directive(super_disk.0);
    let (leg, scenario) = match directive {
        Some(directive) => (u64::from(directive.leg), directive.scenario),
        None => (1, 0),
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
        k2::note(note::BUILD_STEP_FAILED, 116);
        return false;
    };
    let Some(broker_facet) =
        crate::bind_facet(broker_endpoint, right::INSPECT | right::ENDPOINT_CALL)
    else {
        k2::note(note::BUILD_STEP_FAILED, 117);
        return false;
    };

    // One facet per principal, bound in principal order and checked against the
    // number that came back. A work's identity is this number, so the run does
    // not start if the kernel gave a different one.
    let mut principals = [0u64; 4];
    for wanted in [facet::PUBLISHER, facet::RIVAL, facet::READER] {
        match crate::bind_facet(store_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
            Some((handle, got)) if got == wanted => {
                principals[wanted as usize] = handle;
                k2::note(report::BOUND, got);
            }
            _ => {
                k2::note(note::BUILD_STEP_FAILED, 118);
                return false;
            }
        }
    }

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
            scenario,
            apply_directive: 1,
            reserved0: 0,
            reserved1: 0,
        },
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 119);
        return false;
    };
    k2::note(note::SERVICE_BUILT, 2);
    // Everything these named is installed where it belongs. A handle is a
    // grant and a domain's table has thirty-two slots; closing at the point of
    // last use keeps a later failure and its cause in the same place.
    let _ = k2::cap_close(store_image);
    let _ = k2::cap_close(store_scope);
    let _ = k2::cap_close(disk_facet.0);
    let _ = k2::cap_close(broker_facet.0);
    let _ = k2::cap_close(broker_endpoint);
    let _ = k2::cap_close(disk_endpoint);
    let _ = k2::cap_close(iobuf);
    let _ = k2::cap_close(boot_handle(boot_slot::FIRST_DEVICE));
    let _ = k2::cap_close(own_domain);

    // The service recovers whatever the medium left it before it admits
    // anything. Waiting for it to say so is what keeps a work's first call from
    // being answered `UNAVAILABLE` for a reason that is not interesting.
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
        k2::note(note::BUILD_STEP_FAILED, 120);
        return false;
    }
    k2::note(note::STORE_READY, 1);

    // The launcher, and the two objects a run of the language runtime needs: the
    // region the work and the runtime share, and the page a tool writes its
    // report into. Both are the supervisor's, so what a tool or a runtime can
    // reach is what was mapped for it.
    let mut launcher = None;
    let mut host_endpoint = 0u64;
    let mut channel = 0u64;
    let mut launcher_facet = 0u64;
    if shape.uses_runtime {
        let Ok(launch_endpoint) = k2::scope_create_endpoint(system, 8, name16("launch")) else {
            k2::note(note::BUILD_STEP_FAILED, 130);
            return false;
        };
        let Some((facet_handle, _)) =
            crate::bind_facet(launch_endpoint, right::INSPECT | right::ENDPOINT_CALL)
        else {
            k2::note(note::BUILD_STEP_FAILED, 131);
            return false;
        };
        launcher_facet = facet_handle;
        let Ok(shared) = k2::scope_create_memory(
            system,
            work_addr::CHANNEL_PAGES,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
            name16("channel"),
        ) else {
            k2::note(note::BUILD_STEP_FAILED, 132);
            return false;
        };
        channel = shared;
        let Ok(report) = k2::scope_create_memory(
            system,
            1,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
            name16("report"),
        ) else {
            k2::note(note::BUILD_STEP_FAILED, 133);
            return false;
        };
        let Ok(host) = k2::scope_create_endpoint(system, 8, name16("host")) else {
            k2::note(note::BUILD_STEP_FAILED, 134);
            return false;
        };
        host_endpoint = host;
        let Some((host_facet, _)) = crate::bind_facet(host, right::INSPECT | right::ENDPOINT_CALL)
        else {
            k2::note(note::BUILD_STEP_FAILED, 135);
            return false;
        };
        let mut config_digest = [0u8; 32];
        for (index, byte) in config_digest.iter_mut().enumerate() {
            *byte = 0x35 ^ (index as u8);
        }
        launcher = Some(Launcher {
            endpoint: launch_endpoint,
            system,
            tool_image,
            runtime_image,
            supervision,
            channel,
            host_facet,
            report,
            seed: plan.seed,
            runtime_domain: 0,
            runtime_scope: 0,
            runtime_done: 0,
            tools_run: 0,
            tool_config_digest: config_digest,
        });
    }

    let config = WorkConfig {
        role: work_role::PUBLISHER,
        principal: facet::PUBLISHER as u32,
        scenario: scenario as u32,
        leg: leg as u32,
        seed: plan.seed,
        uses_runtime: u32::from(shape.uses_runtime),
        uses_engine: u32::from(shape.uses_engine),
        inferences: 0,
        reserved0: 0,
    };
    let work_parts = WorkParts {
        name: "k5pub",
        image: work_image,
        scope: work_scope,
        store_facet: principals[facet::PUBLISHER as usize],
        done,
        log: 0,
        supervision,
        launcher_facet,
        engine_facet: 0,
        host_endpoint,
        channel,
        cancel,
    };
    let Some(built) = work::build(&work_parts, config) else {
        k2::note(note::BUILD_STEP_FAILED, 121);
        return false;
    };
    // A narrowed handle on the work's scope, kept only to read what it was
    // charged after it has finished. Reading a budget is not spending one.
    let work_scope_handle = k2::derive(work_scope, right::INSPECT, 0, 0).unwrap_or(0);
    k2::note(note::WORK_BUILT, u64::from(work_role::PUBLISHER));

    for handle in principals {
        if handle != 0 {
            let _ = k2::cap_close(handle);
        }
    }
    let _ = k2::cap_close(work_image);
    let _ = k2::cap_close(built.stage);
    let _ = k2::cap_close(store_endpoint);
    let _ = k2::cap_close(disk_scope);
    if launcher_facet != 0 {
        let _ = k2::cap_close(launcher_facet);
    }
    if host_endpoint != 0 {
        let _ = k2::cap_close(host_endpoint);
    }

    // Wait for the work, and for the service asking to be cut.
    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut finished = false;
    while k2::now_ns() < deadline {
        // Before anything that waits, because the log fills while it does.
        audit::observe(log, &mut auditor);
        audit::drain(log, &mut auditor);
        // The launcher is served from this loop: a supervisor that blocked on
        // one thing could not answer the other.
        if let Some(launcher) = launcher.as_mut() {
            while launcher::serve(launcher, limits.cpu_window_ns, k2::now_ns() + POLL_NS) {
                audit::drain(log, &mut auditor);
            }
        }
        if let Ok(info) = k2::signal_wait(crash, k4bit::CRASH, k2::now_ns() + POLL_NS)
            && info.bits & k4bit::CRASH != 0
        {
            audit::drain(log, &mut auditor);
            audit::report(&auditor);
            k2::note(note::CUT, 1);
            return true;
        }
        audit::drain(log, &mut auditor);
        let _ = k2::signal_wait(done, bit::WORK_DONE, k2::now_ns() + POLL_NS);
        if let Ok(info) = k2::domain_query(built.domain)
            && (info.state == domain_state::DEAD || info.state == domain_state::FAULTED)
        {
            k2::note(note::DOMAIN_STOPPED, info.exit_code);
            if info.faults != 0 {
                k2::note(note::DOMAIN_FAULT, info.fault_vector);
            }
            finished = true;
            break;
        }
    }
    audit::observe(log, &mut auditor);
    audit::drain(log, &mut auditor);
    audit::report(&auditor);
    if work_scope_handle != 0
        && let Ok(info) = k2::scope_query(work_scope_handle)
    {
        k2::note(note::SCOPE_AFTER, info.cpu_total_ns);
        k2::note(note::SCOPE_PAGES, info.memory_pages_used);
    }
    let _ = store_domain;
    let _ = status::OK;
    finished
}
