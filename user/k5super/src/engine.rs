//! The resident engine, built the way a service is built.
//!
//! `vault/integration/thalyx.md` gives the shape in one row: *dominio de
//! servicio, pesos patrocinados y tickets por inferencia*. This file is the
//! first two. The engine is a domain in a scope of its own; the weights are a
//! sealed object mapped read-only into it and nowhere else; and what the engine
//! loads and keeps -- the tensors it reads out of that object, its context, its
//! compute buffers -- is charged to that scope, which is what "the weights stay
//! charged to the engine" means in numbers the kernel keeps. The tickets are
//! the invocations works send it, and the engine binds a worker to each one.
//!
//! What the engine domain holds is its endpoint, a signal to say it is ready,
//! and the mapping. No state service, no device, no launcher, no log: an engine
//! that could publish, or build a tool, would be the model reaching past the
//! confirmation that is the whole reason Thalyx keeps it outside the core.

use thalyx_abi::{ScopeLimits, domain_state, memory_state, right};
use thalyx_user_k5pkg::native::{self, Config, addr};
use thalyx_user_k5pkg::proto::{bit, engine_case, note as k5note};
use thalyx_user_rt::k2::{self, name16};

use crate::launch::{self, Install, Recipe};
use crate::note;

/// Context the engine builds, in tokens: the fixture's number, which the host
/// reference is run with too, because both sides have to answer the same
/// question.
pub const CONTEXT_TOKENS: u64 = engine_case::CONTEXT_TOKENS;
/// Threads llama.cpp computes a graph with. One, so every instruction of an
/// inference runs on the worker bound to the invocation and is charged to the
/// caller; a second ggml thread would be a thread nobody bound.
pub const COMPUTE_THREADS: u64 = engine_case::COMPUTE_THREADS;
/// How long loading may take before the supervisor stops waiting. The weights
/// are small; ggml's tables and llama.cpp's graph reservation are not free on
/// an emulated processor.
const LOAD_DEADLINE_NS: u64 = 180_000_000_000;
/// How long one wait between checks is.
const POLL_NS: u64 = 20_000_000;

/// What the supervisor keeps of the engine it built.
pub struct Engine {
    /// The endpoint works reach it through, by facets bound on it.
    pub endpoint: u64,
    /// Its scope, narrowed to reading what it has been charged.
    pub scope: u64,
}

/// Ceilings of the engine's scope. Memory is the one sized by the workload,
/// and the workload is Thalyx's configuration -- a batch of 512 -- not a
/// smaller one chosen to fit. The image maps about twelve hundred pages, and
/// what llama.cpp allocates at load and context creation comes on top. On
/// Linux an allocation nobody touches is address space; here every page of the
/// heap is a memory object charged to this scope, so the ceiling has to cover
/// what llama.cpp asks for, not what it uses. The first ceiling, sixteen
/// megabytes, ended in `GGML_ASSERT(ctx->mem_buffer != NULL)`. What residency
/// really costs is read from the scope after the run.
fn engine_limits(cpu_window_ns: u64) -> ScopeLimits {
    ScopeLimits {
        memory_pages: 16384,
        metadata_objects: 160,
        cpu_budget_ns: cpu_window_ns / 2,
        queue_bytes: 16384,
        // The engine finishes an inference whose caller has gone with this
        // reserve, a token at a time, until it notices and stops.
        closure_reserve_ns: 2_000_000,
        parallelism: 3,
        reserved0: 0,
    }
}

/// Builds the engine over the sealed model and waits until it has loaded.
///
/// Answers `None` with a note naming the step when anything refuses, and when
/// the engine stops before it is ready -- which is what a model llama.cpp will
/// not load looks like from here.
pub fn build(
    system: u64,
    supervision: u64,
    image: u64,
    model: u64,
    model_bytes: u64,
    cpu_window_ns: u64,
    seed: u64,
) -> Option<Engine> {
    let Ok(info) = k2::memory_query(model) else {
        k2::note(note::BUILD_STEP_FAILED, 300);
        return None;
    };
    // Sealed, and checked: weights that could change under the engine would
    // make every answer an answer about something else.
    if info.state != memory_state::SEALED {
        k2::note(note::BUILD_STEP_FAILED, 301);
        return None;
    }
    // The length the plan states has to fall in the object's last page. A
    // longer one would let the engine read past the model; a shorter one would
    // hand it a truncated file and a load failure nobody could explain.
    let pages = u64::from(info.pages);
    if model_bytes == 0 || model_bytes > pages * 4096 || model_bytes <= (pages - 1) * 4096 {
        k2::note(note::BUILD_STEP_FAILED, 305);
        return None;
    }
    let Ok(scope) = k2::scope_create_child(system, engine_limits(cpu_window_ns), name16("engine"))
    else {
        k2::note(note::BUILD_STEP_FAILED, 302);
        return None;
    };
    let Ok(endpoint) = k2::scope_create_endpoint(system, 8, name16("engine")) else {
        k2::note(note::BUILD_STEP_FAILED, 303);
        return None;
    };
    let Ok(ready) = k2::scope_create_signal(system) else {
        k2::note(note::BUILD_STEP_FAILED, 304);
        return None;
    };

    let config = Config {
        magic: native::CONFIG_MAGIC,
        version: 1,
        role: native::role::ENGINE,
        instance: 0,
        heap_pages: 12288,
        arena_pages: 256,
        shared_bytes: 0,
        bulk_bytes: model_bytes,
        xfer_bytes: 0,
        seed,
        arg0: CONTEXT_TOKENS,
        arg1: COMPUTE_THREADS,
        arg2: 0,
        arg3: 0,
    };
    let recipe = Recipe {
        name: "nengine",
        image,
        scope,
        stack_pages: 256,
        // One thread besides the first: llama.cpp's `common` library starts a
        // logging worker the first time something logs through it, and a
        // program that could not start one would fail there rather than here.
        threads: 1,
        config,
        fault_channel: supervision,
    };
    let installs = [
        Install {
            slot: native::slot::INBOUND,
            handle: endpoint,
            rights: right::INSPECT | right::ENDPOINT_RECEIVE,
        },
        Install {
            slot: native::slot::AUX0,
            handle: ready,
            rights: right::INSPECT | right::SIGNAL_RAISE,
        },
    ];
    let built = launch::build_with(
        &recipe,
        &installs,
        |domain| {
            k2::domain_map(
                domain,
                model,
                addr::BULK,
                0,
                pages as u32,
                right::MEMORY_READ,
            )?;
            Ok(())
        },
        |which, code| {
            k2::note(note::BUILD_STEP_FAILED, 320 + which);
            k2::note(k5note::LAUNCH_REFUSED, (-code) as u64);
        },
    )?;
    let _ = k2::cap_close(built.work_signal);
    k2::note(note::SERVICE_BUILT, 3);

    let deadline = k2::now_ns() + LOAD_DEADLINE_NS;
    loop {
        if let Ok(seen) = k2::signal_wait(ready, bit::ENGINE_READY, k2::now_ns() + POLL_NS)
            && seen.bits & bit::ENGINE_READY != 0
        {
            break;
        }
        if let Ok(state) = k2::domain_query(built.domain)
            && (state.state == domain_state::DEAD || state.state == domain_state::FAULTED)
        {
            k2::note(note::DOMAIN_STOPPED, state.exit_code);
            if state.faults != 0 {
                k2::note(note::DOMAIN_FAULT, state.fault_vector);
            }
            return None;
        }
        if k2::now_ns() > deadline {
            k2::note(note::BUILD_STEP_FAILED, 399);
            return None;
        }
    }
    let _ = k2::cap_close(built.done_signal);
    // Said once, and named once: the ready signal and the domain handle have
    // no further use here -- the engine is never stopped by this supervisor,
    // and the kernel's own summary says how it ended -- and a handle is a
    // grant in a table of thirty-two slots.
    let _ = k2::cap_close(ready);
    let _ = k2::cap_close(built.domain);
    let scope_view = k2::derive(scope, right::INSPECT, 0, 0).unwrap_or(0);
    let _ = k2::cap_close(scope);
    k2::note(note::ENGINE_READY, model_bytes);
    Some(Engine {
        endpoint,
        scope: scope_view,
    })
}

/// What the engine's residency cost, as the kernel counted it.
pub fn report(engine: &Engine) {
    if engine.scope != 0
        && let Ok(info) = k2::scope_query(engine.scope)
    {
        k2::note(note::ENGINE_SCOPE_PAGES, info.memory_pages_used);
        k2::note(note::ENGINE_SCOPE_CPU, info.cpu_total_ns);
    }
}
