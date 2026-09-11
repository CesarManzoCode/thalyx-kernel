//! The `surface` stage and every stage after it: the native Thalyx vertical.
//!
//! `vault/roadmap/phases.md` calls the first of these K5a and says what it is
//! worth: a surface driven by fixtures and by something outside the guest,
//! labelled a partial integration. That label is the honest one and this file
//! keeps it. What runs here is Thalyx's *semantics* -- an identified version,
//! context from that version, a private workspace, a real change, a
//! conditioned publication and durable evidence -- on the managed state K4
//! built, with the script coming from outside the guest through the medium.
//!
//! The later stages change who drives the surface and what decides: a program
//! in a real language runtime, a real tool in a domain of its own, a resident
//! engine. The shape of the run is the same, which is why one builder builds
//! all of them and a [`Shape`] says what a stage has.
//!
//! The scenario comes from the medium, like K4's: which works are built and
//! what is done to them. `BASELINE` is one work carrying the vertical through.
//! `RIVALS` is two works over the same version, both asking the same engine,
//! both publishing against the same generation, one of them refused and
//! starting again over the version that won. `CANCEL` is a work whose scope
//! is closed while the engine computes for it, and then an unrelated work
//! using that same engine. A cut in publication is not a scenario: it is a
//! directive the state service applies, exactly as in K4, and any scenario can
//! be cut.

use thalyx_abi::{boot_handle, boot_slot, domain_state, right, status};
use thalyx_user_k4fmt::pkg::{IOBUF_PAGES, StoreConfig, bit as k4bit, facet};
use thalyx_user_k5pkg::native::Plan;
use thalyx_user_k5pkg::proto::{WorkConfig, bit, scenario, work_addr, work_role};
use thalyx_user_rt::k2::{self, name16, report};

use crate::audit::{self, Audit};
use crate::launcher::{self, Launcher, MAX_WORKS, WorkSlot};
use crate::note;
use crate::services::{self, SUPER_IOBUF_VADDR, StoreParts};
use crate::work::{self, WorkParts};

/// How long the whole stage may take before the supervisor stops waiting.
const RUN_DEADLINE_NS: u64 = 240_000_000_000;
/// How long one wait between checks is.
const POLL_NS: u64 = 20_000_000;
/// How long after a work says it is asking the engine its scope is closed:
/// long enough for the engine to have taken the request, bound to it, decoded
/// the prompt and produced some tokens, and far shorter than the inference it
/// was asked for.
const CANCEL_AFTER_NS: u64 = 250_000_000;
/// How long a closed scope is given to drain before the run is called stuck.
const DRAIN_DEADLINE_NS: u64 = 30_000_000_000;

/// What a stage of the port asks of this builder.
pub struct Shape {
    /// Whether the work drives the language runtime.
    pub uses_runtime: bool,
    /// Whether the work may ask the resident engine.
    pub uses_engine: bool,
}

/// One work to build: its role, its principal, and its diagnostic name.
#[derive(Clone, Copy)]
struct Plan1 {
    role: u32,
    principal: u64,
    name: &'static str,
    scope_label: &'static str,
}

/// What the supervisor keeps of one work it built.
struct Standing {
    plan: Plan1,
    domain: u64,
    /// A narrowed handle on its scope, to read what it was charged.
    scope_view: u64,
    /// Its scope, kept with authority only in the scenario that closes it.
    scope: u64,
    done: u64,
    finished: bool,
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

    // Which works, decided by the scenario the medium named. The store facets
    // were bound in principal order above, so a role's principal is the facet
    // number the state service will authenticate its requests as.
    let plans: &[Plan1] = match scenario as u32 {
        scenario::RIVALS => &[
            Plan1 {
                role: work_role::PUBLISHER,
                principal: facet::PUBLISHER,
                name: "k5pub",
                scope_label: "work",
            },
            Plan1 {
                role: work_role::RIVAL,
                principal: facet::RIVAL,
                name: "k5riv",
                scope_label: "rival",
            },
        ],
        scenario::CANCEL => &[
            Plan1 {
                role: work_role::ASKER,
                principal: facet::READER,
                name: "k5ask",
                scope_label: "asker",
            },
            Plan1 {
                role: work_role::PUBLISHER,
                principal: facet::PUBLISHER,
                name: "k5pub",
                scope_label: "work",
            },
        ],
        _ => &[Plan1 {
            role: work_role::PUBLISHER,
            principal: facet::PUBLISHER,
            name: "k5pub",
            scope_label: "work",
        }],
    };
    if plans.len() > MAX_WORKS {
        k2::note(note::BUILD_STEP_FAILED, 129);
        return false;
    }

    // One facet per principal, bound in principal order and checked against the
    // number that came back. A work's identity is this number, so the run does
    // not start if the kernel gave a different one. The facets no work of this
    // scenario is built as are closed at once: a handle is a grant, and the
    // supervisor's table has thirty-two slots.
    let mut principals = [0u64; 4];
    for wanted in [facet::PUBLISHER, facet::RIVAL, facet::READER] {
        match crate::bind_facet(store_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
            Some((handle, got)) if got == wanted => {
                if plans.iter().any(|plan| plan.principal == wanted) {
                    principals[wanted as usize] = handle;
                } else {
                    let _ = k2::cap_close(handle);
                }
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
    let _ = k2::cap_close(store_endpoint);
    let _ = k2::cap_close(store_domain);
    let _ = k2::cap_close(disk_scope);
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

    // The resident engine, when the stage has one: built once, before any work
    // exists, over the model the image carries, and kept for every work that
    // follows. A work reaches it by a facet bound here and by nothing else.
    let mut engine = None;
    if shape.uses_engine {
        let (Some(engine_image), Some(model)) =
            (crate::image_named("nengine"), crate::image_named("k5model"))
        else {
            k2::note(note::BUILD_STEP_FAILED, 140);
            return false;
        };
        let Some(built) = crate::engine::build(
            system,
            supervision,
            engine_image,
            model,
            plan.arg0,
            limits.cpu_window_ns,
            plan.seed,
        ) else {
            k2::note(note::BUILD_STEP_FAILED, 141);
            return false;
        };
        let _ = k2::cap_close(engine_image);
        let _ = k2::cap_close(model);
        engine = Some(built);
    }

    // The launcher, and what each work's run of the language runtime needs: the
    // region the work and its runtime share, and the endpoint the runtime calls
    // the work on. Both are the supervisor's, so what a runtime can reach is
    // what was mapped for it. One page for a tool's report, because tools run
    // one at a time.
    let mut launcher = None;
    let mut launcher_endpoint = 0u64;
    let mut host_endpoints = [0u64; MAX_WORKS];
    let mut channels = [0u64; MAX_WORKS];
    if shape.uses_runtime {
        let Ok(launch_endpoint) = k2::scope_create_endpoint(system, 8, name16("launch")) else {
            k2::note(note::BUILD_STEP_FAILED, 130);
            return false;
        };
        launcher_endpoint = launch_endpoint;
        // The domains the launcher builds report their faults to the launcher,
        // on an endpoint of its own. A fault channel reserves a cell of its
        // endpoint's queue for as long as the domain lives, and a queue has
        // eight: the services, the engine and two works fill most of one, and
        // two runtimes and a tool on top of that is where the first version of
        // this file was refused a fault channel and could not launch.
        let Ok(launch_supervision) = k2::scope_create_endpoint(system, 8, name16("supervise2"))
        else {
            k2::note(note::BUILD_STEP_FAILED, 136);
            return false;
        };
        let Ok(report) = k2::scope_create_memory(
            system,
            1,
            right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
            name16("report"),
        ) else {
            k2::note(note::BUILD_STEP_FAILED, 133);
            return false;
        };
        let mut slots = [WorkSlot::EMPTY; MAX_WORKS];
        for (index, plan) in plans.iter().enumerate() {
            if plan.role == work_role::ASKER {
                // It asks the engine and nothing else: no runtime, no tool.
                continue;
            }
            let Ok(shared) = k2::scope_create_memory(
                system,
                work_addr::CHANNEL_PAGES,
                right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
                name16("channel"),
            ) else {
                k2::note(note::BUILD_STEP_FAILED, 132);
                return false;
            };
            let Ok(host) = k2::scope_create_endpoint(system, 8, name16("host")) else {
                k2::note(note::BUILD_STEP_FAILED, 134);
                return false;
            };
            let Some((host_facet, _)) =
                crate::bind_facet(host, right::INSPECT | right::ENDPOINT_CALL)
            else {
                k2::note(note::BUILD_STEP_FAILED, 135);
                return false;
            };
            channels[index] = shared;
            host_endpoints[index] = host;
            slots[index] = WorkSlot {
                channel: shared,
                host_facet,
                runtime_domain: 0,
                runtime_scope: 0,
                runtime_done: 0,
            };
        }
        let mut config_digest = [0u8; 32];
        for (index, byte) in config_digest.iter_mut().enumerate() {
            *byte = 0x35 ^ (index as u8);
        }
        launcher = Some(Launcher {
            endpoint: launch_endpoint,
            system,
            tool_image,
            runtime_image,
            supervision: launch_supervision,
            report,
            seed: plan.seed,
            slots,
            tools_run: 0,
            tool_config_digest: config_digest,
        });
    }

    // The works, in plan order. Each gets a scope of its own, a done signal, a
    // facet of the launcher bound in its slot's position, a facet of the engine,
    // and its principal's facet of the state service -- and nothing that any
    // other work holds. The facet numbers of the launch endpoint are what the
    // launcher tells works apart by, so they are bound in the same order the
    // slots were filled and checked against the number that came back.
    let mut standing: [Option<Standing>; MAX_WORKS] = [None, None];
    for (index, plan_one) in plans.iter().enumerate() {
        let Ok(work_scope) = k2::scope_create_child(
            system,
            services::limits_for(limits.cpu_window_ns / 2, 320, 3),
            name16(plan_one.scope_label),
        ) else {
            k2::note(note::BUILD_STEP_FAILED, 106);
            return false;
        };
        let Ok(done) = k2::scope_create_signal(system) else {
            k2::note(note::BUILD_STEP_FAILED, 109);
            return false;
        };
        // A facet is bound for every work in plan order, so that a work's facet
        // number is its slot's position; a work that never launches anything
        // gets its facet closed rather than installed, and its slot stays
        // empty, which is what the launcher refuses with.
        let mut launcher_facet = 0u64;
        if shape.uses_runtime {
            match crate::bind_facet(launcher_endpoint, right::INSPECT | right::ENDPOINT_CALL) {
                Some((handle, got)) if got as usize == index + 1 => launcher_facet = handle,
                _ => {
                    k2::note(note::BUILD_STEP_FAILED, 131);
                    return false;
                }
            }
            if plan_one.role == work_role::ASKER {
                let _ = k2::cap_close(launcher_facet);
                launcher_facet = 0;
            }
        }
        let mut engine_facet = 0u64;
        if let Some(engine) = engine.as_ref() {
            let Some((handle, _)) =
                crate::bind_facet(engine.endpoint, right::INSPECT | right::ENDPOINT_CALL)
            else {
                k2::note(note::BUILD_STEP_FAILED, 142);
                return false;
            };
            engine_facet = handle;
        }
        let config = WorkConfig {
            role: plan_one.role,
            principal: plan_one.principal as u32,
            scenario: scenario as u32,
            leg: leg as u32,
            seed: plan.seed,
            uses_runtime: u32::from(shape.uses_runtime && plan_one.role != work_role::ASKER),
            uses_engine: u32::from(shape.uses_engine),
            inferences: u32::from(shape.uses_engine) * 2,
            reserved0: 0,
        };
        let work_parts = WorkParts {
            name: plan_one.name,
            image: work_image,
            scope: work_scope,
            store_facet: principals[plan_one.principal as usize],
            done,
            log: 0,
            supervision,
            launcher_facet,
            engine_facet,
            host_endpoint: host_endpoints[index],
            channel: channels[index],
        };
        let Some(built) = work::build(&work_parts, config) else {
            k2::note(note::BUILD_STEP_FAILED, 121);
            return false;
        };
        // A narrowed handle on the work's scope, kept only to read what it was
        // charged after it has finished. Reading a budget is not spending one.
        // The scope itself, with authority to close it, is kept only for the
        // work the scenario closes; every other work's is let go here.
        let scope_view = k2::derive(work_scope, right::INSPECT, 0, 0).unwrap_or(0);
        let scope = if plan_one.role == work_role::ASKER {
            work_scope
        } else {
            let _ = k2::cap_close(work_scope);
            0
        };
        k2::note(note::WORK_BUILT, u64::from(plan_one.role));
        let _ = k2::cap_close(built.stage);
        if launcher_facet != 0 {
            let _ = k2::cap_close(launcher_facet);
        }
        if engine_facet != 0 {
            let _ = k2::cap_close(engine_facet);
        }
        if host_endpoints[index] != 0 {
            let _ = k2::cap_close(host_endpoints[index]);
        }
        // The done signal is kept only where a bit on it decides something:
        // the asking work says on it when it is asking. Every other work is
        // watched through its domain, and the handle is a grant in a table of
        // thirty-two.
        let done = if plan_one.role == work_role::ASKER {
            done
        } else {
            let _ = k2::cap_close(done);
            0
        };
        standing[index] = Some(Standing {
            plan: *plan_one,
            domain: built.domain,
            scope_view,
            scope,
            done,
            finished: false,
        });
    }
    if let Some(engine) = engine.as_ref() {
        // Every facet a work will ever hold is bound; the engine keeps its own
        // grant on the endpoint, and this one is a slot.
        let _ = k2::cap_close(engine.endpoint);
    }

    for handle in principals {
        if handle != 0 {
            let _ = k2::cap_close(handle);
        }
    }
    let _ = k2::cap_close(work_image);

    // Wait for every work, and for the service asking to be cut. The launcher
    // is served from this loop: a supervisor that blocked on one thing could
    // not answer the other. In the `CANCEL` scenario this loop is also where
    // the closing happens, at the moment the asking work says it is asking.
    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut cancel_at: Option<(usize, u64)> = None;
    let mut cancelled = false;
    while k2::now_ns() < deadline {
        // Before anything that waits, because the log fills while it does.
        audit::observe(log, &mut auditor);
        audit::drain(log, &mut auditor);
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

        let mut all_finished = true;
        for (index, slot) in standing.iter_mut().enumerate() {
            let Some(work) = slot.as_mut() else {
                continue;
            };
            if work.finished {
                continue;
            }
            if work.done != 0
                && let Ok(seen) = k2::signal_wait(
                    work.done,
                    bit::WORK_DONE | bit::WORK_ASKING,
                    k2::now_ns() + POLL_NS,
                )
                && seen.bits & bit::WORK_ASKING != 0
                && work.plan.role == work_role::ASKER
                && cancel_at.is_none()
            {
                cancel_at = Some((index, k2::now_ns() + CANCEL_AFTER_NS));
            }
            if let Ok(info) = k2::domain_query(work.domain)
                && (info.state == domain_state::DEAD || info.state == domain_state::FAULTED)
            {
                k2::note(note::DOMAIN_STOPPED, info.exit_code);
                if info.faults != 0 {
                    k2::note(note::DOMAIN_FAULT, info.fault_vector);
                }
                work.finished = true;
            }
            all_finished &= work.finished;
        }

        // Closing the asking work while the engine computes for it. The scope
        // is fenced -- the kernel cancels its waits, withdraws what it never
        // delivered and marks its invocations -- and then drained: the engine
        // still holds an obligation on it until it notices and answers, and the
        // scope cannot be retired until it has. That is the whole point: closure
        // is a barrier and a drain and a retirement, in that order, and the
        // engine stays where it is through all three.
        if let Some((index, at)) = cancel_at
            && !cancelled
            && k2::now_ns() >= at
            && let Some(work) = standing[index].as_mut()
        {
            cancelled = true;
            let fenced = k2::scope_fence(work.scope);
            k2::note(note::WORK_CANCELLED, fenced.map_or(0, u64::from));
            let drain_by = k2::now_ns() + DRAIN_DEADLINE_NS;
            let mut retired = false;
            while k2::now_ns() < drain_by {
                audit::drain(log, &mut auditor);
                if let Some(launcher) = launcher.as_mut() {
                    while launcher::serve(launcher, limits.cpu_window_ns, k2::now_ns() + POLL_NS) {}
                }
                let (outcome, drain) = k2::scope_retire(work.scope);
                if outcome.is_ok() {
                    k2::note(note::WORK_RETIRED, drain.retained_pages);
                    retired = true;
                    break;
                }
                k2::note(
                    note::WORK_DRAINING,
                    u64::from(drain.invocations_pending)
                        | (u64::from(drain.threads_running) << 16)
                        | (u64::from(drain.effects_pending) << 32),
                );
                let _ = k2::signal_wait(crash, k4bit::CRASH, k2::now_ns() + POLL_NS);
            }
            if !retired {
                k2::note(note::BUILD_STEP_FAILED, 150);
                return false;
            }
            let _ = k2::cap_close(work.scope);
            let _ = k2::cap_close(work.done);
            work.scope = 0;
            work.done = 0;
            work.finished = true;
        }

        if all_finished {
            break;
        }
    }
    audit::observe(log, &mut auditor);
    audit::drain(log, &mut auditor);
    audit::report(&auditor);
    let mut finished = true;
    for slot in standing.iter() {
        let Some(work) = slot.as_ref() else {
            continue;
        };
        finished &= work.finished;
        if work.scope_view != 0
            && let Ok(info) = k2::scope_query(work.scope_view)
        {
            k2::note(note::SCOPE_AFTER, info.cpu_total_ns);
            k2::note(note::SCOPE_PAGES, info.memory_pages_used);
        }
    }
    // What residency cost, next to what the works cost: two scopes, two numbers,
    // both the kernel's.
    if let Some(engine) = engine.as_ref() {
        crate::engine::report(engine);
    }
    let _ = status::OK;
    finished
}
