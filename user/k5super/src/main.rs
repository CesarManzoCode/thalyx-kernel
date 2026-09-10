//! The K5 supervisor.
//!
//! K5 is the port: Thalyx's semantic surface running on this kernel's own
//! mechanisms instead of on Linux's. This program is what stands the port up.
//! It holds the only boot authority in the image and hands each domain exactly
//! what its role needs, which is what makes "the language runtime has no
//! filesystem" a fact about its capability table rather than a claim about its
//! source.
//!
//! The phases of the port arrive here as stages, and each one is only added
//! once the stage before it runs:
//!
//!   * `smoke` -- the native C target itself: an image built by the host
//!     cross-toolchain, entered, standing on a stack this program mapped,
//!     talking to the kernel, doing hardware floating point, and growing a heap
//!     out of memory objects it creates against its own scope.
//!
//! The stage is chosen by the first argument of the boot record this
//! supervisor is given, which the host writes into the image. A supervisor that
//! decided for itself which stage it was would make every run its own.

#![no_std]
#![no_main]

mod audit;
mod launch;
mod launcher;
mod services;
mod surface;
mod work;

use thalyx_abi::{boot_handle, handle as make_handle};
use thalyx_abi::{boot_slot, domain_state, memory_state, right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::native::{self, Config, addr};
use thalyx_user_rt::k2::{self, name16, report};
use thalyx_user_rt::{self as rt, entry};

/// Boot slots that may hold a sealed image object.
pub const MAX_IMAGES: u32 = 16;

/// Notes this supervisor emits. Their own range, so a K5 note can never be read
/// as a K2, K3 or K4 one by a gate reading one log.
pub mod note {
    /// The supervisor started. Value: the stage it was told to run.
    pub const STAGE: u64 = 0x5000;
    /// A native image was read and published a record. Value: its trampoline.
    pub const IMAGE_READ: u64 = 0x5010;
    /// A native domain was built and activated. Value: its role.
    pub const NATIVE_UP: u64 = 0x5011;
    /// A build step refused. Value: the step number.
    pub const BUILD_STEP_FAILED: u64 = 0x5012;
    /// A domain reported a fault. Value: the vector.
    pub const DOMAIN_FAULT: u64 = 0x5013;
    /// A domain the supervisor was waiting for has stopped. Value: exit code.
    pub const DOMAIN_STOPPED: u64 = 0x5014;
    /// The scope of a domain, after it ran. Value: pages it held.
    pub const SCOPE_PAGES: u64 = 0x5015;
    /// The scope of a domain, after it ran. Value: nanoseconds it spent.
    pub const SCOPE_CPU: u64 = 0x5016;
    /// A service of the port was built. Value: which one.
    pub const SERVICE_BUILT: u64 = 0x5017;
    /// The state service reported it is admitting requests.
    pub const STORE_READY: u64 = 0x5018;
    /// A work domain was built. Value: its role.
    pub const WORK_BUILT: u64 = 0x5019;
    /// The run directive the host wrote on the medium. Value: packed.
    pub const DIRECTIVE: u64 = 0x501A;
    /// The state service asked for the run to end at a named point.
    pub const CUT: u64 = 0x501B;
    /// The supervisor's own view of a scope after its domain ran.
    pub const SCOPE_AFTER: u64 = 0x501C;
    /// An effect receipt the auditor read. Value: the object it names.
    pub const AUDIT_EFFECT: u64 = 0x501D;
    /// Receipts read and acknowledged over the run.
    pub const AUDIT_DRAINED: u64 = 0x501E;
    /// Effect receipts among them.
    pub const AUDIT_EFFECTS: u64 = 0x501F;
    /// The fullest the control log was observed, and any gaps in its sequence.
    pub const AUDIT_HIGH_WATER: u64 = 0x5020;
    /// Receipts the kernel had to drop.
    pub const AUDIT_LOST: u64 = 0x5021;
    /// The run reached its end. Value: domains that finished cleanly.
    pub const DONE: u64 = 0x50FF;
}

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

/// The plan the host built into this image.
///
/// K2's supervisor took no configuration; K5's has to, because one image runs
/// several stages of the port and which one is not the supervisor's choice. The
/// plan arrives as a module of the package like every other, so the supervisor
/// finds it the same way it finds an image: by asking each sealed object for
/// its label.
fn plan() -> Option<native::Plan> {
    let handle = image_named("k5plan")?;
    let mut bytes = [0u8; size_of::<native::Plan>()];
    let read = k2::memory_read(handle, 0, &mut bytes);
    // The plan is read once and never again; the handle is a grant somebody
    // else will need a slot for.
    let _ = k2::cap_close(handle);
    read.ok()?;
    let plan = native::Plan::read_from(&bytes, 0)?;
    if plan.magic != native::PLAN_MAGIC {
        return None;
    }
    Some(plan)
}

/// Binds a facet on an endpoint and says which one the kernel assigned.
///
/// The facet number is the kernel's to give, not the caller's to choose: the
/// request carries zero and the grant comes back carrying the number. That is
/// what makes a facet an identity rather than a claim.
pub fn bind_facet(endpoint: u64, rights: u32) -> Option<(u64, u64)> {
    let handle = k2::endpoint_bind_facet(endpoint, 0, rights | right::TRANSFER, 0).ok()?;
    let info = k2::cap_inspect(handle).ok()?;
    Some((handle, info.facet))
}

fn scope_report(scope: u64) {
    if let Ok(info) = k2::scope_query(scope) {
        k2::note(note::SCOPE_PAGES, info.memory_pages_used);
        k2::note(note::SCOPE_CPU, info.cpu_total_ns);
    }
}

/// Waits until a native program says it has finished, then reads the exit code
/// off the domain itself.
///
/// Two facts and not one: the runtime raises a bit when `th_main` returns, and
/// the kernel records the exit code when the domain actually leaves. Waiting on
/// the signal is what keeps this from being a spin; reading the domain is what
/// keeps the answer from being the program's own account of itself.
fn await_stop(done_signal: u64, domain: u64, deadline_ns: u64) -> Option<u64> {
    if k2::signal_wait(done_signal, native::DONE_BIT, deadline_ns).is_err() {
        return None;
    }
    loop {
        let Ok(info) = k2::domain_query(domain) else {
            return None;
        };
        if info.state == domain_state::DEAD || info.state == domain_state::FAULTED {
            if info.faults != 0 {
                k2::note(note::DOMAIN_FAULT, info.fault_vector);
            }
            return Some(info.exit_code);
        }
        if k2::now_ns() > deadline_ns {
            return None;
        }
    }
}

fn smoke_stage(system: u64, supervision: u64, seed: u64) -> bool {
    let Some(image) = image_named("nsmoke") else {
        k2::note(note::BUILD_STEP_FAILED, 0);
        return false;
    };
    let Some(inspected) = launch::inspect(image) else {
        k2::note(note::BUILD_STEP_FAILED, 1);
        return false;
    };
    k2::note(note::IMAGE_READ, inspected.header.thread_trampoline);

    let limits = thalyx_abi::ScopeLimits {
        memory_pages: 1024,
        metadata_objects: 48,
        cpu_budget_ns: thalyx_abi::limit::CPU_WINDOW_NS / 4,
        queue_bytes: 8 * 1024,
        closure_reserve_ns: 500_000,
        parallelism: 2,
        reserved0: 0,
    };
    let Ok(scope) = k2::scope_create_child(system, limits, name16("smoke")) else {
        k2::note(note::BUILD_STEP_FAILED, 2);
        return false;
    };

    let mut config = Config {
        magic: native::CONFIG_MAGIC,
        version: 1,
        role: native::role::SMOKE,
        instance: 0,
        heap_pages: 4096,
        arena_pages: 64,
        shared_bytes: 0,
        bulk_bytes: 0,
        xfer_bytes: 0,
        seed,
        arg0: 0,
        arg1: 0,
        arg2: 0,
        arg3: 0,
    };
    config.arg0 = seed ^ 0x5A5A;

    let recipe = launch::Recipe {
        name: "nsmoke",
        image,
        scope,
        stack_pages: 64,
        threads: 0,
        config,
        fault_channel: supervision,
    };
    let Some(built) = launch::build(&recipe, &[], |which, code| {
        k2::note(note::BUILD_STEP_FAILED, which);
        k2::note(report::UNEXPECTED, code as u64);
    }) else {
        return false;
    };
    k2::note(note::NATIVE_UP, u64::from(native::role::SMOKE));
    // This role has no worker threads, so the handle on the signal they would
    // have waited on is a grant nobody will name again.
    let _ = k2::cap_close(built.work_signal);

    let deadline = k2::now_ns() + 20_000_000_000;
    let stopped = await_stop(built.done_signal, built.domain, deadline);
    match stopped {
        Some(code) => k2::note(note::DOMAIN_STOPPED, code),
        None => k2::note(note::DOMAIN_STOPPED, u64::MAX),
    }
    scope_report(scope);
    let _ = addr::CONFIG;
    stopped == Some(0)
}

fn run() -> ! {
    let system = boot_handle(boot_slot::SELF_SCOPE);
    let own_domain = boot_handle(boot_slot::SELF_DOMAIN);
    let _ = own_domain;
    let _ = make_handle(0, 0);
    let Some(plan) = plan() else {
        k2::note(note::BUILD_STEP_FAILED, 89);
        rt::exit(1)
    };
    k2::note(note::STAGE, u64::from(plan.stage));

    // A supervision endpoint: every domain this supervisor builds reports its
    // faults here, so a domain that dies is a message and not a silence.
    let Ok(supervision) = k2::scope_create_endpoint(system, 8, name16("supervise")) else {
        k2::note(note::BUILD_STEP_FAILED, 90);
        rt::exit(1)
    };

    let ok = match plan.stage {
        native::stage::SMOKE => smoke_stage(system, supervision, plan.seed),
        native::stage::SURFACE => surface::run(
            system,
            supervision,
            &plan,
            &surface::Shape {
                uses_runtime: false,
                uses_engine: false,
            },
        ),
        native::stage::WORK => surface::run(
            system,
            supervision,
            &plan,
            &surface::Shape {
                uses_runtime: true,
                uses_engine: false,
            },
        ),
        _ => {
            k2::note(note::BUILD_STEP_FAILED, 88);
            false
        }
    };
    k2::note(note::DONE, u64::from(ok));
    let _ = status::OK;
    rt::exit(u64::from(!ok))
}

entry!(run);
