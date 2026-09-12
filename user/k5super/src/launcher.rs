//! `ProgramLaunch`, as a service rather than as an ambient power.
//!
//! Two of the things the port has to do are launches: running the language
//! runtime over a program, and running a validation tool over a candidate. In
//! Thalyx both are `fork`/`execve` inside a sandbox the Core sets up, and the
//! authority to do it is the Core's. Here it is neither ambient nor the work's:
//! a work domain holds no capability that can build a domain, and asks.
//!
//! That is not ceremony. It is what makes two sentences true at once: the work
//! decides *what* is validated, and the launcher decides *what a tool is
//! allowed to be*. A tool gets a scope of its own with its own ceilings, a
//! sealed candidate and a place to write a report, and nothing else -- no state
//! service, no engine, no device, no authority to build anything further. When
//! it is done its scope is retired, and what it cost is read from the kernel's
//! accounting rather than from what the tool says about itself.
//!
//! The launcher serves more than one work, and it tells them apart the way the
//! state service tells principals apart: by the facet the kernel authenticated
//! on the request, never by a field in it. Each work has a slot -- the region it
//! shares with its runtime, the endpoint that runtime calls it on, and the
//! runtime while one is up -- and a work's request reaches its own slot and no
//! other. A work that asked to stop "the runtime" stops its runtime.

use thalyx_abi::{ScopeLimits, domain_state, memory_state, right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::native::{self, Config, DONE_BIT, addr};
use thalyx_user_k5pkg::proto::{
    LaunchReply, LaunchRequest, ToolReport, launch_op, launch_status, note as k5note,
};
use thalyx_user_rt::k2::{self, name16};

use crate::launch::{self, Install, Recipe};
use crate::note;

/// The tool identity this launcher has an image for.
pub const TOOL_ID: u64 = 0x4B35_2001;

/// How long a tool may run before the launcher stops it.
const TOOL_DEADLINE_NS: u64 = 60_000_000_000;
/// How long one wait between checks is.
const POLL_NS: u64 = 5_000_000;

/// Works the launcher can serve at once. A slot per principal facet.
pub const MAX_WORKS: usize = 2;

/// What the launcher keeps for one work: the two objects a run of its language
/// runtime needs, and the runtime while one is up.
#[derive(Clone, Copy)]
pub struct WorkSlot {
    /// The region this work and its language runtime share, or zero when the
    /// slot is not in use.
    pub channel: u64,
    /// The facet of the endpoint the runtime calls this work on.
    pub host_facet: u64,
    /// The runtime's domain while it is up, or zero.
    pub runtime_domain: u64,
    /// Its scope while it is up, or zero.
    pub runtime_scope: u64,
    /// Its done signal while it is up, or zero.
    pub runtime_done: u64,
}

impl WorkSlot {
    pub const EMPTY: WorkSlot = WorkSlot {
        channel: 0,
        host_facet: 0,
        runtime_domain: 0,
        runtime_scope: 0,
        runtime_done: 0,
    };
}

/// Everything the launcher needs to name.
pub struct Launcher {
    /// The endpoint it receives launch requests on.
    pub endpoint: u64,
    /// The scope tool and runtime scopes are children of.
    pub system: u64,
    /// The validation tool's image.
    pub tool_image: u64,
    /// The language runtime's image.
    pub runtime_image: u64,
    /// Where a domain's faults are reported.
    pub supervision: u64,
    /// Where a tool writes its report. One page: tools run one at a time, and
    /// the launcher reads the report before it answers.
    pub report: u64,
    /// The run's seed.
    pub seed: u64,
    /// One slot per principal facet of the launch endpoint, facet one first.
    pub slots: [WorkSlot; MAX_WORKS],
    /// Tools run so far.
    pub tools_run: u64,
    /// The digest of the tool's configuration, for the validation record.
    pub tool_config_digest: [u8; 32],
}

fn refuse(status_code: u32) -> LaunchReply {
    let mut reply = LaunchReply::zeroed();
    reply.status = status_code;
    reply
}

fn tool_limits(cpu_window_ns: u64) -> ScopeLimits {
    // A tool gets a ceiling of its own, and it is not generous. What it is for
    // is that a tool which loops is a refusal in the tool's own accounting
    // rather than a run that never ends.
    ScopeLimits {
        memory_pages: 1024,
        metadata_objects: 64,
        cpu_budget_ns: cpu_window_ns / 4,
        queue_bytes: 4096,
        closure_reserve_ns: 200_000,
        parallelism: 1,
        reserved0: 0,
    }
}

/// Runs one tool over one sealed candidate.
fn run_tool(
    launcher: &mut Launcher,
    request: &LaunchRequest,
    candidate: u64,
    cpu_window_ns: u64,
) -> LaunchReply {
    if request.tool_id as u64 != TOOL_ID {
        // A tool this launcher does not have. The answer is that, and not the
        // tool it does have: a check answered by a weaker one would be the
        // fallback dressed up as the stronger profile.
        k2::note(
            k5note::LAUNCH_REFUSED,
            u64::from(launch_status::NO_SUCH_TOOL),
        );
        if candidate != 0 {
            let _ = k2::cap_close(candidate);
        }
        return refuse(launch_status::NO_SUCH_TOOL);
    }
    if candidate == 0 {
        k2::note(k5note::LAUNCH_NOT_SEALED, 0);
        return refuse(launch_status::NOT_SEALED);
    }
    // Sealed, and checked here rather than assumed: a tool that validated bytes
    // which could change under it would be validating nothing in particular.
    let Ok(info) = k2::memory_query(candidate) else {
        k2::note(k5note::LAUNCH_NOT_SEALED, 1);
        let _ = k2::cap_close(candidate);
        return refuse(launch_status::NOT_SEALED);
    };
    if info.state != memory_state::SEALED {
        k2::note(k5note::LAUNCH_NOT_SEALED, u64::from(info.state));
        let _ = k2::cap_close(candidate);
        return refuse(launch_status::NOT_SEALED);
    }

    let Ok(scope) =
        k2::scope_create_child(launcher.system, tool_limits(cpu_window_ns), name16("tool"))
    else {
        let _ = k2::cap_close(candidate);
        return refuse(launch_status::UNAVAILABLE);
    };

    let config = Config {
        magic: native::CONFIG_MAGIC,
        version: 1,
        role: native::role::CHECK,
        instance: launcher.tools_run,
        heap_pages: 512,
        arena_pages: 64,
        shared_bytes: 0,
        bulk_bytes: request.candidate_len,
        xfer_bytes: 4096,
        seed: if request.seed != 0 {
            request.seed
        } else {
            launcher.seed
        },
        arg0: request.tool_id.into(),
        arg1: 0,
        arg2: 0,
        arg3: 0,
        flags: 0,
    };
    let recipe = Recipe {
        name: "ncheck",
        image: launcher.tool_image,
        scope,
        stack_pages: 128,
        threads: 0,
        config,
        fault_channel: launcher.supervision,
    };
    let installs = [
        Install {
            slot: native::slot::BULK,
            handle: candidate,
            rights: right::INSPECT | right::MEMORY_READ,
        },
        Install {
            slot: native::slot::XFER,
            handle: launcher.report,
            rights: right::INSPECT | right::MEMORY_READ | right::MEMORY_WRITE,
        },
    ];

    // The candidate read-only where the tool expects it, and a page it may
    // write its report into. Both are mapped by the launcher, so what the tool
    // can reach is what the launcher decided and not what it asked for.
    let built = launch::build_with(
        &recipe,
        &installs,
        |domain| {
            k2::domain_map(
                domain,
                candidate,
                addr::BULK,
                0,
                info.pages as u32,
                right::MEMORY_READ,
            )?;
            k2::domain_map(
                domain,
                launcher.report,
                addr::XFER,
                0,
                1,
                right::MEMORY_READ | right::MEMORY_WRITE,
            )?;
            Ok(())
        },
        |which, code| {
            k2::note(note::BUILD_STEP_FAILED, 200 + which);
            k2::note(k5note::LAUNCH_REFUSED, (-code) as u64);
        },
    );
    let Some(built) = built else {
        let _ = k2::cap_close(candidate);
        let _ = k2::scope_fence(scope);
        let _ = k2::scope_retire(scope);
        return refuse(launch_status::UNAVAILABLE);
    };
    // A tool has no worker threads, so the signal they would have waited on
    // is a grant nobody will name again, and the candidate the work lent is
    // installed and mapped where the tool will read it. Both go now and not
    // after the run, because the table this launcher shares with its
    // supervisor is full at exactly the moment two works have runtimes up and
    // one of them asks for a tool.
    let _ = k2::cap_close(built.work_signal);
    let _ = k2::cap_close(candidate);
    launcher.tools_run += 1;
    k2::note(k5note::LAUNCH_BUILT, TOOL_ID);

    let deadline = k2::now_ns()
        + if request.deadline_ns == 0 {
            TOOL_DEADLINE_NS
        } else {
            request.deadline_ns
        };
    let finished = k2::signal_wait(built.done_signal, DONE_BIT, deadline).is_ok();

    let mut reply = LaunchReply::zeroed();
    reply.tool_id = TOOL_ID;
    reply.tool_config_digest = launcher.tool_config_digest;
    let mut exit_code = u32::MAX;
    let mut state = 0u32;
    for _ in 0..64 {
        if let Ok(info) = k2::domain_query(built.domain) {
            state = info.state;
            if info.state == domain_state::DEAD || info.state == domain_state::FAULTED {
                exit_code = info.exit_code as u32;
                if info.faults != 0 {
                    reply.status = launch_status::FAULTED;
                }
                break;
            }
        }
        let _ = k2::signal_wait(built.done_signal, DONE_BIT, k2::now_ns() + POLL_NS);
    }
    if !finished && exit_code == u32::MAX {
        reply.status = launch_status::TIMED_OUT;
        let _ = k2::domain_terminate(built.domain);
    }
    let _ = state;

    // What the tool said about itself, and what the kernel says it cost. The
    // second is not the tool's to report.
    let mut bytes = [0u8; size_of::<ToolReport>()];
    if k2::memory_read(launcher.report, 0, &mut bytes).is_ok()
        && let Some(told) = ToolReport::read_from(&bytes, 0)
    {
        reply.checks_run = told.checks_run;
        reply.checks_failed = told.checks_failed;
        reply.bytes_read = told.bytes_read;
        reply.candidate_sum = told.candidate_sum;
    }
    if let Ok(info) = k2::scope_query(scope) {
        reply.cpu_ns = info.cpu_total_ns;
        reply.pages = info.memory_pages_used;
    }
    reply.exit_code = exit_code;

    let _ = k2::cap_close(built.done_signal);
    let _ = k2::domain_terminate(built.domain);
    let _ = k2::cap_close(built.domain);
    let _ = k2::scope_fence(scope);
    let (_, drain) = k2::scope_retire(scope);
    k2::note(k5note::LAUNCH_RETIRED, drain.retained_pages);
    let _ = k2::cap_close(scope);

    k2::note(k5note::TOOL_VERDICT, u64::from(reply.exit_code));
    k2::note(k5note::TOOL_COST, reply.cpu_ns);
    reply
}

/// Builds the language runtime over the shared channel and answers at once.
///
/// At once, and that is the whole shape of it: the work that asked has to be
/// free to serve the calls the runtime is about to make, and a launcher that
/// waited for the runtime to finish would be holding the only thread that could
/// answer it.
fn start_runtime(
    launcher: &mut Launcher,
    which: usize,
    request: &LaunchRequest,
    cpu_window_ns: u64,
) -> LaunchReply {
    let slot = launcher.slots[which];
    if slot.runtime_domain != 0 || slot.channel == 0 {
        return refuse(launch_status::UNAVAILABLE);
    }
    let Ok(scope) = k2::scope_create_child(
        launcher.system,
        ScopeLimits {
            memory_pages: 2048,
            metadata_objects: 96,
            cpu_budget_ns: cpu_window_ns / 3,
            queue_bytes: 16384,
            closure_reserve_ns: 400_000,
            parallelism: 1,
            reserved0: 0,
        },
        name16("runtime"),
    ) else {
        return refuse(launch_status::UNAVAILABLE);
    };

    let config = Config {
        magic: native::CONFIG_MAGIC,
        version: 1,
        role: native::role::HACER,
        instance: 0,
        heap_pages: 1024,
        arena_pages: 128,
        shared_bytes: request.candidate_len,
        bulk_bytes: 0,
        xfer_bytes: 0,
        seed: if request.seed != 0 {
            request.seed
        } else {
            launcher.seed
        },
        // The program's ceilings, decided here and not by the program: wall,
        // ticks, calls, and the engine's own memory limit.
        arg0: 90_000_000_000,
        arg1: 40_000_000,
        arg2: 256,
        arg3: 3 * 1024 * 1024,
        flags: 0,
    };
    let recipe = Recipe {
        name: "nhacer",
        image: launcher.runtime_image,
        scope,
        stack_pages: 512,
        threads: 0,
        config,
        fault_channel: launcher.supervision,
    };
    let installs = [Install {
        slot: native::slot::SERVICE,
        handle: slot.host_facet,
        rights: right::INSPECT | right::ENDPOINT_CALL,
    }];
    let channel = slot.channel;
    let built = launch::build_with(
        &recipe,
        &installs,
        |domain| {
            k2::domain_map(
                domain,
                channel,
                addr::SHARED,
                0,
                thalyx_user_k5pkg::proto::work_addr::CHANNEL_PAGES as u32,
                right::MEMORY_READ | right::MEMORY_WRITE,
            )?;
            Ok(())
        },
        |which, code| {
            k2::note(note::BUILD_STEP_FAILED, 220 + which);
            k2::note(k5note::LAUNCH_REFUSED, (-code) as u64);
        },
    );
    let Some(built) = built else {
        let _ = k2::scope_fence(scope);
        let _ = k2::scope_retire(scope);
        return refuse(launch_status::UNAVAILABLE);
    };
    launcher.slots[which].runtime_domain = built.domain;
    launcher.slots[which].runtime_scope = scope;
    launcher.slots[which].runtime_done = built.done_signal;
    let _ = k2::cap_close(built.work_signal);
    k2::note(k5note::LAUNCH_BUILT, u64::from(native::role::HACER));

    let mut reply = LaunchReply::zeroed();
    reply.tool_id = u64::from(native::role::HACER);
    reply
}

/// How long a runtime that has sent its last answer is given to leave on its
/// own before it is stopped.
const RUNTIME_LEAVE_NS: u64 = 2_000_000_000;

fn stop_runtime(launcher: &mut Launcher, which: usize) -> LaunchReply {
    let mut reply = LaunchReply::zeroed();
    let slot = launcher.slots[which];
    if slot.runtime_domain == 0 {
        return reply;
    }
    // A runtime whose program has finished is still tearing its heap down when
    // the work asks for it to be stopped. Stopping it there is stopping it in
    // the middle of freeing memory the termination is withdrawing, so it is
    // given a bounded moment to leave by itself -- it raises its done bit when
    // it does -- and is stopped by authority only if it has not. Either way it
    // is stopped: this bounds the wait, it does not make stopping optional.
    let _ = k2::signal_wait(slot.runtime_done, DONE_BIT, k2::now_ns() + RUNTIME_LEAVE_NS);
    if let Ok(info) = k2::domain_query(slot.runtime_domain) {
        reply.exit_code = info.exit_code as u32;
        if info.faults != 0 {
            reply.status = launch_status::FAULTED;
        }
    }
    if let Ok(info) = k2::scope_query(slot.runtime_scope) {
        reply.cpu_ns = info.cpu_total_ns;
        reply.pages = info.memory_pages_used;
    }
    let _ = k2::domain_terminate(slot.runtime_domain);
    let _ = k2::cap_close(slot.runtime_domain);
    let _ = k2::cap_close(slot.runtime_done);
    let _ = k2::scope_fence(slot.runtime_scope);
    let (_, drain) = k2::scope_retire(slot.runtime_scope);
    k2::note(k5note::LAUNCH_RETIRED, drain.retained_pages);
    let _ = k2::cap_close(slot.runtime_scope);
    launcher.slots[which].runtime_domain = 0;
    launcher.slots[which].runtime_scope = 0;
    launcher.slots[which].runtime_done = 0;
    reply
}

/// Serves at most one launch request, waiting no longer than `deadline_ns`.
///
/// Answers whether anything was served, so the caller's loop can keep draining
/// the control plane between requests.
pub fn serve(launcher: &mut Launcher, cpu_window_ns: u64, deadline_ns: u64) -> bool {
    let Ok((message, invocation)) = k2::endpoint_receive(launcher.endpoint, deadline_ns, false)
    else {
        return false;
    };
    let request = LaunchRequest::read_from(&message.payload, 0);
    let lent = if message.cap_count > 0 {
        message.caps[0]
    } else {
        0
    };
    // The principal is the facet the kernel stamped, and its slot is the one
    // bound in that position. A facet with no slot is a caller this launcher
    // was never told about.
    let which = (message.header.facet as usize).wrapping_sub(1);
    let reply = match request {
        Some(_) if which >= MAX_WORKS => refuse(launch_status::UNAVAILABLE),
        Some(request) if request.op == launch_op::RUN_TOOL => {
            run_tool(launcher, &request, lent, cpu_window_ns)
        }
        Some(request) if request.op == launch_op::START_RUNTIME => {
            start_runtime(launcher, which, &request, cpu_window_ns)
        }
        Some(request) if request.op == launch_op::STOP_RUNTIME => stop_runtime(launcher, which),
        Some(_) => {
            let mut reply = LaunchReply::zeroed();
            reply.tool_id = TOOL_ID;
            reply.tool_config_digest = launcher.tool_config_digest;
            reply
        }
        None => refuse(launch_status::NO_SUCH_TOOL),
    };
    // The handle the caller lent is charged to this domain's table until it is
    // closed, and a launcher that kept one per call would run out of table
    // rather than out of anything interesting. A tool run closes it itself, as
    // soon as the tool has it.
    if lent != 0 && !matches!(request, Some(request) if request.op == launch_op::RUN_TOOL) {
        let _ = k2::cap_close(lent);
    }
    let _ = k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes());
    let _ = k2::cap_close(invocation);
    let _ = status::OK;
    true
}
