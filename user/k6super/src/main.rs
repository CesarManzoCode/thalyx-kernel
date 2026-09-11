//! The K6 supervisor: the native half of the paired benchmarks.
//!
//! K6 compares this kernel with Linux on the same virtual machine, from one C
//! source, and this program is what stands the native half up. It builds a
//! client domain that runs the plan the host wrote, an answering domain with
//! one thread per endpoint, and, when the plan asks the engine anything, the
//! resident engine K5 built -- with K5's own launcher and engine builder, the
//! same files, so a benchmark of the engine is a benchmark of that engine and
//! not of a copy.
//!
//! It has two jobs while the plan runs. It is the **auditor**: every admission
//! the kernel makes reserves a control receipt, a full log refuses admissions,
//! and nothing wakes anyone when a receipt is written, so the log has to be
//! drained by someone polling it. That cost is the audited profile's, and this
//! program measures it rather than hiding it. And it runs the entries the
//! client cannot: an entry that needs a domain built, a scope fenced or a
//! scope with a budget of its own is handed over in the shared page, run here,
//! and handed back, in the plan's order, so both backends run the same entries
//! in the same sequence.

#![no_std]
#![no_main]

// K5's auditor, for its observation and report; the draining itself is done
// by `entries::drain_timed`, which times every batch, so K5's `drain` is unused.
#[allow(dead_code)]
#[path = "../../k5super/src/audit.rs"]
mod audit;
#[path = "../../k5super/src/engine.rs"]
mod engine;
mod entries;
mod generated;
#[path = "../../k5super/src/launch.rs"]
mod launch;

use thalyx_abi::{ScopeLimits, boot_handle, boot_slot, domain_state, memory_state, right};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::native::{self, Config, addr};
use thalyx_user_rt::k2::{self, name16};
use thalyx_user_rt::{self as rt, entry};

use generated::{Handoff, Plan, bench};

/// Boot slots that may hold a sealed image object.
pub const MAX_IMAGES: u32 = 16;

/// Notes this supervisor emits. The first twelve are the names K5's engine
/// builder and auditor use, given K6's own range here so a K6 log is never read
/// as a K5 one.
pub mod note {
    /// A build step refused. Value: the step number.
    pub const BUILD_STEP_FAILED: u64 = 0x6100;
    /// A service was built. Value: which one.
    pub const SERVICE_BUILT: u64 = 0x6101;
    /// A domain the supervisor was waiting for has stopped. Value: exit code.
    pub const DOMAIN_STOPPED: u64 = 0x6102;
    /// A domain reported a fault. Value: the vector.
    pub const DOMAIN_FAULT: u64 = 0x6103;
    /// The engine loaded its weights. Value: bytes of the model.
    pub const ENGINE_READY: u64 = 0x6104;
    /// The engine's scope after the run. Value: pages it held.
    pub const ENGINE_SCOPE_PAGES: u64 = 0x6105;
    /// The engine's scope after the run. Value: nanoseconds it was charged.
    pub const ENGINE_SCOPE_CPU: u64 = 0x6106;
    /// An effect receipt the auditor read. Value: the object it names.
    pub const AUDIT_EFFECT: u64 = 0x6107;
    /// Receipts read and acknowledged over the run.
    pub const AUDIT_DRAINED: u64 = 0x6108;
    /// Effect receipts among them.
    pub const AUDIT_EFFECTS: u64 = 0x6109;
    /// The fullest the control log was observed, and gaps in its sequence.
    pub const AUDIT_HIGH_WATER: u64 = 0x610A;
    /// Receipts the kernel had to drop.
    pub const AUDIT_LOST: u64 = 0x610B;
    /// The supervisor started. Value: the plan's seed.
    pub const STARTED: u64 = 0x6110;
    /// An entry was handed over. Value: benchmark | parameter << 16.
    pub const HANDOFF: u64 = 0x6111;
    /// A scope's execution after the plan. Value: which << 56 | nanoseconds.
    pub const SCOPE_CPU: u64 = 0x6112;
    /// A scope's pages after the plan. Value: which << 56 | pages.
    pub const SCOPE_PAGES: u64 = 0x6113;
    /// Times the auditor read the log, and how many of those found it empty.
    pub const AUDIT_POLLS: u64 = 0x6114;
    /// The supervisor's own time-stamp calibration. Value: hertz.
    pub const TSC_HZ: u64 = 0x6115;
    /// The plan was refused. Value: why.
    pub const PLAN_REFUSED: u64 = 0x6116;
    /// The engine was built. Value: microseconds from starting to build it to
    /// its ready signal.
    pub const ENGINE_LOAD_US: u64 = 0x6117;
    /// The run reached its end. Value: whether the client finished cleanly.
    pub const FINISHED: u64 = 0x61FF;
}

/// Which scope a `SCOPE_CPU` or `SCOPE_PAGES` note is about.
pub mod which {
    pub const CLIENT: u64 = 1;
    pub const SERVER: u64 = 2;
    pub const ENGINE: u64 = 3;
}

/// How long the whole plan may take before the supervisor stops waiting.
const RUN_DEADLINE_NS: u64 = 1_200_000_000_000;
/// How long the auditor waits in the kernel for a receipt before it looks for
/// a handoff instead: the bound on how long a handed-over entry waits to
/// start, which is not part of what any entry measures.
const AUDIT_SLEEP_NS: u64 = 500_000;

pub fn image_named(label: &str) -> Option<u64> {
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

/// Reads `destination.len()` bytes through a memory capability, in the pieces
/// of at most 256 bytes one `MEMORY_READ` carries.
pub fn read_all(memory: u64, offset: u64, destination: &mut [u8]) -> Result<(), i64> {
    for (index, piece) in destination.chunks_mut(256).enumerate() {
        k2::memory_read(memory, offset + (index * 256) as u64, piece)?;
    }
    Ok(())
}

/// Writes `source` through a memory capability, 256 bytes at a time.
pub fn write_all(memory: u64, offset: u64, source: &[u8]) -> Result<(), i64> {
    for (index, piece) in source.chunks(256).enumerate() {
        k2::memory_write(memory, offset + (index * 256) as u64, piece)?;
    }
    Ok(())
}

fn read_plan() -> Option<Plan> {
    let handle = image_named("k6plan")?;
    let mut bytes = [0u8; size_of::<Plan>()];
    // The plan is larger than one `MEMORY_READ` carries; K5's was not.
    let read = read_all(handle, 0, &mut bytes);
    let _ = k2::cap_close(handle);
    read.ok()?;
    let plan = Plan::read_from(&bytes, 0)?;
    if plan.magic != generated::PLAN_MAGIC || u64::from(plan.version) != generated::PLAN_VERSION {
        return None;
    }
    if u64::from(plan.count) > generated::PLAN_ENTRIES {
        return None;
    }
    Some(plan)
}

/// Binds a facet on an endpoint; the kernel assigns the facet number.
pub fn bind_facet(endpoint: u64, rights: u32) -> Option<u64> {
    k2::endpoint_bind_facet(endpoint, 0, rights | right::TRANSFER, 0).ok()
}

/// The time-stamp counter.
pub fn cycles() -> u64 {
    // SAFETY: `rdtsc` reads a counter and has no other effect.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Calibrates the counter against the kernel's clock, as the C side does.
fn calibrate() -> u64 {
    let t0 = k2::now_ns();
    let c0 = cycles();
    let mut t1 = t0;
    while t1 - t0 < 50_000_000 {
        t1 = k2::now_ns();
    }
    let c1 = cycles();
    ((u128::from(c1 - c0) * 1_000_000_000) / u128::from(t1 - t0)) as u64
}

/// A boot record for one role of the benchmark program.
pub fn bench_config(role: u64, helpers: u64, arg1: u64, shared: bool, seed: u64) -> Config {
    Config {
        magic: native::CONFIG_MAGIC,
        version: 1,
        role: role as u32,
        instance: 0,
        heap_pages: 64,
        arena_pages: 16,
        shared_bytes: if shared { generated::SHARED_BYTES } else { 0 },
        bulk_bytes: 0,
        xfer_bytes: 0,
        seed,
        arg0: helpers,
        arg1,
        arg2: 0,
        arg3: 0,
    }
}

pub fn limits(cpu_budget_ns: u64, memory_pages: u64, parallelism: u32) -> ScopeLimits {
    ScopeLimits {
        memory_pages,
        metadata_objects: 256,
        cpu_budget_ns,
        queue_bytes: 64 * 1024,
        closure_reserve_ns: 1_000_000,
        parallelism,
        reserved0: 0,
    }
}

fn plan_uses(plan: &Plan, wanted: &[u32]) -> bool {
    plan.bench[..plan.count as usize]
        .iter()
        .any(|id| wanted.contains(id))
}

/// Everything the plan's entries and the handed-over entries share.
pub struct Run {
    pub system: u64,
    pub supervision: u64,
    pub log: u64,
    pub bench_image: u64,
    pub window_ns: u64,
    pub tsc_hz: u64,
    pub seed: u64,
    pub engine: Option<engine::Engine>,
    pub engine_facet: u64,
    pub engine_load_us: u64,
    pub auditor: audit::Audit,
    pub drain: entries::DrainSamples,
    pub polls: u64,
    pub empty_polls: u64,
}

fn run() -> ! {
    let system = boot_handle(boot_slot::SELF_SCOPE);
    let log = boot_handle(boot_slot::CONTROL_LOG);
    if let Some(own) = image_named("k6super") {
        let _ = k2::cap_close(own);
    }
    let Some(plan) = read_plan() else {
        k2::note(note::PLAN_REFUSED, 1);
        rt::exit(1)
    };
    k2::note(note::STARTED, plan.seed);
    let tsc_hz = calibrate();
    k2::note(note::TSC_HZ, tsc_hz);
    let Ok(interface) = k2::limits() else {
        k2::note(note::BUILD_STEP_FAILED, 1);
        rt::exit(1)
    };
    let window = interface.cpu_window_ns;
    let Ok(supervision) = k2::scope_create_endpoint(system, 8, name16("supervise")) else {
        k2::note(note::BUILD_STEP_FAILED, 2);
        rt::exit(1)
    };
    let Some(bench_image) = image_named("nbench") else {
        k2::note(note::BUILD_STEP_FAILED, 3);
        rt::exit(1)
    };

    let mut run = Run {
        system,
        supervision,
        log,
        bench_image,
        window_ns: window,
        tsc_hz,
        seed: plan.seed,
        engine: None,
        engine_facet: 0,
        engine_load_us: 0,
        auditor: audit::Audit::default(),
        drain: entries::DrainSamples::new(),
        polls: 0,
        empty_polls: 0,
    };

    // --- the engine, when the plan asks it anything --------------------------
    let engine_entries = [
        bench::ENGINE_LOAD,
        bench::ENGINE_INFER,
        bench::ENGINE_CANCEL,
    ];
    if plan_uses(&plan, &engine_entries) {
        let (Some(engine_image), Some(model)) = (image_named("nengine"), image_named("k5model"))
        else {
            k2::note(note::BUILD_STEP_FAILED, 10);
            rt::exit(1)
        };
        let started = cycles();
        // A processor's worth of execution and no notes: what is timed is
        // the engine. K5 gives it half a window and reads every note.
        let built = engine::build(
            system,
            supervision,
            engine_image,
            model,
            plan.model_bytes,
            window,
            true,
            plan.seed,
        );
        let Some(built) = built else {
            k2::note(note::BUILD_STEP_FAILED, 11);
            rt::exit(1)
        };
        run.engine_load_us = (cycles() - started) * 1_000_000 / tsc_hz;
        k2::note(note::ENGINE_LOAD_US, run.engine_load_us);
        let _ = k2::cap_close(engine_image);
        let _ = k2::cap_close(model);
        run.engine_facet =
            bind_facet(built.endpoint, right::INSPECT | right::ENDPOINT_CALL).unwrap_or(0);
        run.engine = Some(built);
    }

    // --- the answering domain ------------------------------------------------
    let ipc_entries = [
        bench::IPC_CALL,
        bench::IPC_CAPS,
        bench::IPC_LINEAGE,
        bench::SCALE_IPC,
    ];
    let mut endpoints = [0u64; 4];
    let mut server_scope = 0u64;
    if plan_uses(&plan, &ipc_entries) {
        let Ok(scope) = k2::scope_create_child(system, limits(window * 4, 512, 4), name16("k6srv"))
        else {
            k2::note(note::BUILD_STEP_FAILED, 20);
            rt::exit(1)
        };
        server_scope = scope;
        for (index, slot) in endpoints.iter_mut().enumerate() {
            let Ok(endpoint) = k2::scope_create_endpoint(system, 8, name16("k6pair")) else {
                k2::note(note::BUILD_STEP_FAILED, 21 + index as u64);
                rt::exit(1)
            };
            *slot = endpoint;
        }
        let receive = right::INSPECT | right::ENDPOINT_RECEIVE;
        let installs = [
            launch::Install {
                slot: native::slot::INBOUND,
                handle: endpoints[0],
                rights: receive,
            },
            launch::Install {
                slot: generated::SLOT_PAIR1 as u32,
                handle: endpoints[1],
                rights: receive,
            },
            launch::Install {
                slot: generated::SLOT_PAIR2 as u32,
                handle: endpoints[2],
                rights: receive,
            },
            launch::Install {
                slot: generated::SLOT_PAIR3 as u32,
                handle: endpoints[3],
                rights: receive,
            },
        ];
        let recipe = launch::Recipe {
            name: "k6server",
            image: bench_image,
            scope,
            stack_pages: 32,
            threads: 3,
            config: bench_config(generated::ROLE_SERVER, 3, 4, false, plan.seed),
            fault_channel: supervision,
        };
        let Some(built) = launch::build(&recipe, &installs, |which, code| {
            k2::note(note::BUILD_STEP_FAILED, 40 + which);
            k2::note(thalyx_user_rt::k2::report::UNEXPECTED, code as u64);
        }) else {
            rt::exit(1)
        };
        let _ = k2::cap_close(built.work_signal);
        let _ = k2::cap_close(built.done_signal);
        let _ = k2::cap_close(built.domain);
        k2::note(note::SERVICE_BUILT, 2);
    }

    // --- the client ---------------------------------------------------------
    // Budgets are aggregate over a scope's threads: the client runs up to four
    // threads at once and the server answers on four endpoints, so each gets
    // four windows of execution per window -- a processor each -- and the
    // scaling benchmarks measure the processors rather than the budget.
    let Ok(client_scope) =
        k2::scope_create_child(system, limits(window * 4, 2048, 5), name16("k6cli"))
    else {
        k2::note(note::BUILD_STEP_FAILED, 60);
        rt::exit(1)
    };
    let Ok(shared) = k2::scope_create_memory(
        system,
        1,
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP,
        name16("k6shared"),
    ) else {
        k2::note(note::BUILD_STEP_FAILED, 61);
        rt::exit(1)
    };
    if write_all(shared, generated::SHARED_PLAN_OFFSET, plan.as_bytes()).is_err() {
        k2::note(note::BUILD_STEP_FAILED, 62);
        rt::exit(1)
    }
    let Ok(handoff) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 63);
        rt::exit(1)
    };
    let call = right::INSPECT | right::TRANSFER | right::DERIVE | right::ENDPOINT_CALL;
    let mut facets = [0u64; 4];
    for (index, facet) in facets.iter_mut().enumerate() {
        if endpoints[index] != 0 {
            *facet = bind_facet(endpoints[index], call).unwrap_or(0);
        }
    }
    let client_engine = run
        .engine
        .as_ref()
        .and_then(|engine| bind_facet(engine.endpoint, right::INSPECT | right::ENDPOINT_CALL))
        .unwrap_or(0);
    let mut installs: [launch::Install; 6] = core::array::from_fn(|_| launch::Install {
        slot: 0,
        handle: 0,
        rights: 0,
    });
    let mut count = 0usize;
    let pair_slots = [
        generated::SLOT_PAIR0,
        generated::SLOT_PAIR1,
        generated::SLOT_PAIR2,
        generated::SLOT_PAIR3,
    ];
    for (index, facet) in facets.iter().enumerate() {
        if *facet != 0 {
            installs[count] = launch::Install {
                slot: pair_slots[index] as u32,
                handle: *facet,
                rights: call,
            };
            count += 1;
        }
    }
    if client_engine != 0 {
        installs[count] = launch::Install {
            slot: generated::SLOT_ENGINE as u32,
            handle: client_engine,
            rights: right::INSPECT | right::TRANSFER | right::ENDPOINT_CALL,
        };
        count += 1;
    }
    installs[count] = launch::Install {
        slot: generated::SLOT_HANDOFF as u32,
        handle: handoff,
        rights: right::INSPECT | right::SIGNAL_RAISE | right::SIGNAL_WAIT,
    };
    count += 1;
    let recipe = launch::Recipe {
        name: "k6client",
        image: bench_image,
        scope: client_scope,
        stack_pages: 64,
        threads: 3,
        config: bench_config(generated::ROLE_CLIENT, 3, 0, true, plan.seed),
        fault_channel: supervision,
    };
    let Some(client) = launch::build_with(
        &recipe,
        &installs[..count],
        |domain| {
            k2::domain_map(
                domain,
                shared,
                addr::SHARED,
                0,
                1,
                right::MEMORY_READ | right::MEMORY_WRITE,
            )?;
            Ok(())
        },
        |which, code| {
            k2::note(note::BUILD_STEP_FAILED, 70 + which);
            k2::note(thalyx_user_rt::k2::report::UNEXPECTED, code as u64);
        },
    ) else {
        rt::exit(1)
    };
    let _ = k2::cap_close(client.work_signal);
    for facet in facets.iter().chain(core::iter::once(&client_engine)) {
        if *facet != 0 {
            let _ = k2::cap_close(*facet);
        }
    }
    k2::note(note::SERVICE_BUILT, 1);

    // --- audit and serve handoffs until the client is done ---------------------
    let deadline = k2::now_ns() + RUN_DEADLINE_NS;
    let mut finished_cleanly = false;
    let mut turn = 0u64;
    loop {
        // Drain whatever the log holds, sleeping in the kernel for the first
        // receipt when it holds nothing, and look for a handoff between
        // drains. Two earlier auditors were wrong in opposite ways: one that
        // polled the empty log spent this scope's half-window of execution
        // and was refused dispatch for the other half, with the handed-over
        // entries it times stalling in it; one that slept a tick between
        // looks let the log's sixty-four cells fill in a fraction of a
        // millisecond of the client's calls, and the admissions it was meant
        // to cover were refused for want of a cell. A read that waits costs
        // nothing while nothing happens and answers with the first receipt.
        entries::drain_timed(&mut run, k2::now_ns() + AUDIT_SLEEP_NS);
        let request = k2::signal_query(handoff)
            .is_ok_and(|info| info.bits & generated::HANDOFF_REQUEST_BIT != 0)
            && k2::signal_wait(
                handoff,
                generated::HANDOFF_REQUEST_BIT,
                k2::now_ns() + 1_000,
            )
            .is_ok_and(|seen| seen.bits & generated::HANDOFF_REQUEST_BIT != 0);
        turn += 1;
        if turn % 64 == 0 {
            audit::observe(log, &mut run.auditor);
        }
        if request {
            let mut bytes = [0u8; size_of::<Handoff>()];
            let status = match k2::memory_read(shared, generated::SHARED_HANDOFF_OFFSET, &mut bytes)
                .ok()
                .and_then(|_| Handoff::read_from(&bytes, 0))
            {
                Some(request) => {
                    k2::note(
                        note::HANDOFF,
                        u64::from(request.bench) | (u64::from(request.param) << 16),
                    );
                    entries::run(&mut run, &request)
                }
                None => 2,
            };
            let mut answer = Handoff::read_from(&bytes, 0).unwrap_or_else(Handoff::zeroed);
            answer.status = status;
            let _ = k2::memory_write(shared, generated::SHARED_HANDOFF_OFFSET, answer.as_bytes());
            let _ = k2::signal_raise(handoff, generated::HANDOFF_DONE_BIT);
        }
        if let Ok(info) = k2::domain_query(client.domain)
            && (info.state == domain_state::DEAD || info.state == domain_state::FAULTED)
        {
            if info.faults != 0 {
                k2::note(note::DOMAIN_FAULT, info.fault_vector);
            }
            k2::note(note::DOMAIN_STOPPED, info.exit_code);
            finished_cleanly = info.faults == 0 && info.exit_code == 0;
            break;
        }
        if k2::now_ns() > deadline {
            break;
        }
    }
    entries::drain_timed(&mut run, 0);

    // --- what it cost, in the kernel's numbers ------------------------------
    for (which, scope) in [(which::CLIENT, client_scope), (which::SERVER, server_scope)] {
        if scope != 0
            && let Ok(info) = k2::scope_query(scope)
        {
            k2::note(note::SCOPE_CPU, (which << 56) | info.cpu_total_ns);
            k2::note(note::SCOPE_PAGES, (which << 56) | info.memory_pages_used);
        }
    }
    if let Some(engine) = run.engine.as_ref() {
        engine::report(engine);
    }
    audit::report(&run.auditor);
    k2::note(note::AUDIT_POLLS, run.polls | (run.empty_polls << 32));
    k2::note(note::FINISHED, u64::from(finished_cleanly));
    rt::exit(u64::from(!finished_cleanly))
}

entry!(run);
