//! Driving the language runtime, and answering what it asks.
//!
//! `hacer` in Thalyx is one composed intention executed as one transaction: a
//! program runs inside a reversible boundary, what it looks at decides what it
//! does next, and the transaction commits or rolls back on the result. This is
//! that, on this system's mechanisms.
//!
//! The program is **not a second authority**. Every call it makes arrives here
//! and goes through [`crate::verbs`] -- the same function, against the same
//! workspace, that the `surface` stage's script goes through. A program is not a
//! way to reach a verb that is not exposed, and if the two callers went through
//! different code that sentence would be a hope.
//!
//! ## The shared region, and why the bytes are copied
//!
//! A message carries at most `MAX_INLINE_PAYLOAD`, and a program's answers are
//! larger than that, so the runtime and this work share a region: the runtime
//! writes a request into it and says how long it is, and the answer comes back
//! the same way. The region is writable in both domains, which means the
//! runtime can change it while this side is reading -- so this side copies a
//! request into its own memory *before* it looks at it. Parsing bytes an
//! untrusted domain can still edit is the classic way to check one thing and
//! act on another.
//!
//! ## Where the latch lives
//!
//! On this side. A failed assertion is recorded here, and every call after it
//! is refused here, so a program cannot catch its way past the thing that
//! stopped it: the thing that stops it is not in the language.

use thalyx_abi::{cap_op, right, status};
use thalyx_user_k4fmt::Pod;
use thalyx_user_k5pkg::proto::{
    CANDIDATE_ENTRIES, CANDIDATE_FLAG_FUNCTION_BODY, CANDIDATE_MAGIC_LOW, CandidateEntry,
    CandidateHeader, HostReply, HostRequest, LaunchReply, LaunchRequest, RunMetrics, ToolReport,
    host_op, launch_op, launch_status, note, verdict, work_addr, work_slot,
};
use thalyx_user_k5pkg::thalyx::Json;
use thalyx_user_rt::k2::{self, name16};

use crate::tree::Workspace;
use crate::verbs;

/// How long the work waits for one call from the runtime before looking around.
const RECEIVE_SLICE_NS: u64 = 200_000_000;
/// How long a whole program may take before this work gives up on it.
const PROGRAM_DEADLINE_NS: u64 = 120_000_000_000;
/// How long a call to the launcher may take.
const LAUNCH_DEADLINE_NS: u64 = 90_000_000_000;

/// The region shared with the language runtime.
///
/// # Safety
///
/// The supervisor maps `CHANNEL_PAGES` writable pages there before activating
/// this domain, and maps them nowhere else in it.
fn channel() -> &'static mut [u8] {
    // SAFETY: as the doc comment says.
    unsafe {
        core::slice::from_raw_parts_mut(
            work_addr::CHANNEL as *mut u8,
            (work_addr::CHANNEL_PAGES as usize) * 4096,
        )
    }
}

/// What a run of the language runtime came to.
pub struct Run {
    /// How it ended.
    pub metrics: RunMetrics,
    /// Whether an assertion latched.
    pub latched: bool,
    /// The verdict of the last validation it asked for, or `NOT_PROVEN`.
    pub verdict: u32,
    /// Checks the tool ran, and how many did not hold.
    pub checks_run: u32,
    /// Checks that did not hold.
    pub checks_failed: u32,
    /// What the tool cost, as the kernel accounted for it.
    pub tool_cpu_ns: u64,
    /// The tool identity that produced the verdict.
    pub tool_id: u64,
    /// The digest of its configuration.
    pub tool_config_digest: [u8; 32],
}

/// Everything one program run needs from the work around it.
pub struct Driver<'a> {
    /// The verb surface's context: the workspace and the version it is against.
    pub context: verbs::Context<'a>,
    /// A facet of the launcher.
    pub launcher: u64,
    /// The endpoint the runtime calls this work on.
    pub inbound: u64,
    /// A facet of the resident engine, or zero when the role has none.
    pub engine: u64,
    /// The buffer this work lends to the engine.
    pub prompt: u64,
    /// The run's seed.
    pub seed: u64,
    /// The candidate root the last validation was about.
    pub validated_tree: [u8; 32],
}

fn write_answer(bytes: usize) -> u32 {
    bytes as u32
}

/// Copies a request out of the shared region before anything looks at it.
fn take_request(len: usize, into: &mut [u8]) -> usize {
    let width = len
        .min(into.len())
        .min(thalyx_user_k5pkg::proto::CHANNEL_REQUEST_MAX);
    into[..width]
        .copy_from_slice(&channel()[thalyx_user_k5pkg::proto::CHANNEL_REQUEST_OFFSET..][..width]);
    width
}

fn put_answer(bytes: &[u8]) {
    let at = thalyx_user_k5pkg::proto::CHANNEL_ANSWER_OFFSET;
    let width = bytes
        .len()
        .min(thalyx_user_k5pkg::proto::CHANNEL_ANSWER_MAX);
    channel()[at..at + width].copy_from_slice(&bytes[..width]);
}

/// Decodes `len u32, bytes` fields out of a copied request.
fn field<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    if *at + 4 > bytes.len() {
        return None;
    }
    let width = u32::from_le_bytes(bytes[*at..*at + 4].try_into().ok()?) as usize;
    *at += 4;
    if *at + width > bytes.len() {
        return None;
    }
    let out = &bytes[*at..*at + width];
    *at += width;
    Some(out)
}

/// Assembles the candidate a tool is given, seals it, and asks the launcher to
/// run the tool over it.
///
/// Sealed before it is lent: the kernel withdraws every writable mapping, so
/// what the tool reads is what the verdict is about. Nothing in this work can
/// change those bytes afterwards, which is the whole reason the seal is here
/// and not a promise in a comment.
fn validate(driver: &mut Driver<'_>, scratch: &mut [u8]) -> Option<LaunchReply> {
    /// Says which step of a validation refused before answering `None`.
    ///
    /// A helper that can fail silently is a helper that will, and a validation
    /// that answers `not_proven` without saying why is the least useful answer
    /// this system can give.
    fn refused_at(step: u64) -> Option<LaunchReply> {
        k2::note(note::WORK_UNEXPECTED, 0x0600 | step);
        None
    }

    let workspace: &Workspace = driver.context.workspace;
    if workspace.entries().len() > CANDIDATE_ENTRIES {
        return refused_at(1);
    }
    let header_len = size_of::<CandidateHeader>();
    let table_len = CANDIDATE_ENTRIES * size_of::<CandidateEntry>();
    let mut at = header_len + table_len;
    for (index, entry) in workspace.entries().iter().enumerate() {
        if at + entry.bytes().len() > scratch.len() {
            return refused_at(2);
        }
        let mut name = [0u8; 32];
        name[..entry.name().len()].copy_from_slice(entry.name());
        // A program is a function body and not a script: it ends in a `return`,
        // which is a syntax error at the top level of a script and is exactly
        // right inside the wrapper the runtime executes it in. The tool is told
        // which it is rather than made to guess from the name.
        let flags = if entry.name() == crate::content::NAME_PROGRAM {
            CANDIDATE_FLAG_FUNCTION_BODY as u32
        } else {
            0
        };
        let record = CandidateEntry {
            offset: at as u64,
            length: entry.bytes().len() as u64,
            name_len: entry.name().len() as u32,
            flags,
            name,
        };
        record
            .write_to(scratch, header_len + index * size_of::<CandidateEntry>())
            .ok()?;
        scratch[at..at + entry.bytes().len()].copy_from_slice(entry.bytes());
        at += entry.bytes().len();
    }
    // What the verdict will be about, computed here from the same encoder the
    // service uses, so the record can name it before anything is staged.
    let Some(tree) = crate::candidate_digest(workspace) else {
        return refused_at(3);
    };
    driver.validated_tree = tree;
    let header = CandidateHeader {
        magic: CANDIDATE_MAGIC_LOW as u32,
        version: 1,
        count: workspace.entries().len() as u32,
        reserved0: 0,
        seed: driver.seed,
        total_bytes: at as u64,
        root_digest: tree,
    };
    header.write_to(scratch, 0).ok()?;

    let pages = (at as u64).div_ceil(4096);
    let Ok(object) = k2::scope_create_memory(
        thalyx_abi::boot_handle(work_slot::SELF_SCOPE),
        pages.max(1),
        // Sealing is in the object's maximum rights because sealing is what
        // this object is for: it is written once and then made immutable, and
        // an object whose maximum rights did not include the seal could never
        // become the thing a tool is allowed to trust.
        right::MEMORY_READ | right::MEMORY_WRITE | right::MEMORY_MAP | right::MEMORY_SEAL,
        name16("candidate"),
    ) else {
        return refused_at(4);
    };
    let mut written = 0usize;
    while written < at {
        let take = (at - written).min(256);
        if k2::memory_write(object, written as u64, &scratch[written..written + take]).is_err() {
            let _ = k2::cap_close(object);
            return refused_at(5);
        }
        written += take;
    }
    if k2::memory_seal(object).is_err() {
        let _ = k2::cap_close(object);
        return refused_at(6);
    }
    k2::note(note::CANDIDATE_SEALED, at as u64);

    let Ok(lent) = k2::derive(
        object,
        right::INSPECT | right::TRANSFER | right::MEMORY_READ | right::MEMORY_MAP,
        0,
        0,
    ) else {
        let _ = k2::cap_close(object);
        return refused_at(7);
    };

    let request = LaunchRequest {
        op: launch_op::RUN_TOOL,
        tool_id: 0x4B35_2001,
        candidate_len: at as u64,
        candidate_digest: driver.validated_tree,
        seed: driver.seed,
        deadline_ns: LAUNCH_DEADLINE_NS / 2,
    };
    let answer = k2::endpoint_call(
        driver.launcher,
        1,
        request.as_bytes(),
        &[(lent, cap_op::MOVE)],
        k2::now_ns() + LAUNCH_DEADLINE_NS,
        false,
    );
    let _ = k2::cap_close(object);
    let reply = answer
        .ok()
        .and_then(|result| LaunchReply::read_from(&result.payload, 0))?;
    k2::note(note::TOOL_VERDICT, u64::from(reply.exit_code));
    k2::note(note::TOOL_COST, reply.cpu_ns);
    // What the tool read and what it read it as. A verdict that named neither
    // could be a verdict about anything.
    k2::note(
        note::TOOL_READ,
        reply.bytes_read
            | (u64::from(reply.checks_run) << 32)
            | (u64::from(reply.checks_failed) << 48),
    );
    k2::note(note::TOOL_SUM, reply.candidate_sum);
    Some(reply)
}

fn validation_answer(reply: Option<&LaunchReply>, out: &mut [u8]) -> (usize, u32) {
    let mut json = Json::new(out);
    let mut which = verdict::NOT_PROVEN;
    json.open();
    match reply {
        Some(reply) if reply.status == launch_status::OK && reply.exit_code == 0 => {
            which = verdict::PASSED;
            json.field_bool("ok", true);
            json.field_string("verdict", b"passed");
        }
        Some(reply) if reply.status == launch_status::OK => {
            which = verdict::FAILED;
            json.field_bool("ok", true);
            json.field_string("verdict", b"failed");
        }
        // A check that could not run is **never** a check that passed. The word
        // is different because the decision it supports is different.
        _ => {
            json.field_bool("ok", false);
            json.field_string("verdict", b"not_proven");
        }
    }
    if let Some(reply) = reply {
        json.field_number("checks_run", u64::from(reply.checks_run));
        json.field_number("checks_failed", u64::from(reply.checks_failed));
        json.field_number("exit_code", u64::from(reply.exit_code));
        json.field_number("cpu_ns", reply.cpu_ns);
        json.field_number("tool_id", reply.tool_id);
        json.field_string("coverage", b"parses_and_asserts");
        json.field_bool("type_checked", false);
    }
    json.close();
    (json.finish().unwrap_or(0), which)
}

/// Runs one program in the language runtime and answers everything it asks.
pub fn run(driver: &mut Driver<'_>, program: &[u8], scratch: &mut [u8], answer: &mut [u8]) -> Run {
    let mut outcome = Run {
        metrics: RunMetrics::zeroed(),
        latched: false,
        verdict: verdict::NOT_PROVEN,
        checks_run: 0,
        checks_failed: 0,
        tool_cpu_ns: 0,
        tool_id: 0,
        tool_config_digest: [0u8; 32],
    };

    let at = thalyx_user_k5pkg::proto::CHANNEL_PROGRAM_OFFSET;
    let width = program
        .len()
        .min(thalyx_user_k5pkg::proto::CHANNEL_PROGRAM_MAX);
    channel()[at..at + width].copy_from_slice(&program[..width]);

    let start = LaunchRequest {
        op: launch_op::START_RUNTIME,
        tool_id: 0,
        candidate_len: width as u64,
        candidate_digest: [0u8; 32],
        seed: driver.seed,
        deadline_ns: PROGRAM_DEADLINE_NS,
    };
    if k2::endpoint_call(
        driver.launcher,
        2,
        start.as_bytes(),
        &[],
        k2::now_ns() + LAUNCH_DEADLINE_NS,
        false,
    )
    .is_err()
    {
        outcome.metrics.finish = thalyx_user_k5pkg::proto::finish::REFUSED;
        return outcome;
    }

    let deadline = k2::now_ns() + PROGRAM_DEADLINE_NS;
    let mut request_bytes = [0u8; 4096];
    loop {
        if k2::now_ns() > deadline {
            outcome.metrics.finish = thalyx_user_k5pkg::proto::finish::EXHAUSTED;
            break;
        }
        let Ok((message, invocation)) =
            k2::endpoint_receive(driver.inbound, k2::now_ns() + RECEIVE_SLICE_NS, false)
        else {
            continue;
        };
        let Some(framed) = HostRequest::read_from(&message.payload, 0) else {
            let _ = k2::invocation_reply(invocation, 0, HostReply::zeroed().as_bytes());
            let _ = k2::cap_close(invocation);
            continue;
        };
        let copied = take_request(framed.request_len as usize, &mut request_bytes);
        let mut reply = HostReply::zeroed();
        reply.stopped = u32::from(outcome.latched);
        let mut finished = false;

        match framed.op {
            _ if outcome.latched && framed.op != host_op::FINISH => {
                reply.status = 1;
            }
            host_op::REQUEST => {
                let mut cursor = 0usize;
                let mut names = [b"".as_slice(); 8];
                let verb = field(&request_bytes[..copied], &mut cursor).unwrap_or(b"");
                let count = if cursor + 4 <= copied {
                    let value = u32::from_le_bytes(
                        request_bytes[cursor..cursor + 4]
                            .try_into()
                            .unwrap_or([0; 4]),
                    ) as usize;
                    cursor += 4;
                    value.min(names.len())
                } else {
                    0
                };
                for slot in names.iter_mut().take(count) {
                    *slot = field(&request_bytes[..copied], &mut cursor).unwrap_or(b"");
                }
                verbs::note_call(verb);
                let written = verbs::answer(&mut driver.context, verb, &names[..count], answer);
                put_answer(&answer[..written]);
                reply.answer_len = write_answer(written);
            }
            host_op::CHANGED => {
                let written = verbs::answer(&mut driver.context, b"cambios", &[], answer);
                put_answer(&answer[..written]);
                reply.answer_len = write_answer(written);
            }
            host_op::VALIDATE => {
                let launched = validate(driver, scratch);
                if let Some(reply) = &launched {
                    outcome.checks_run = reply.checks_run;
                    outcome.checks_failed = reply.checks_failed;
                    outcome.tool_cpu_ns = reply.cpu_ns;
                    outcome.tool_id = reply.tool_id;
                    outcome.tool_config_digest = reply.tool_config_digest;
                }
                let (written, which) = validation_answer(launched.as_ref(), answer);
                outcome.verdict = which;
                k2::note(note::VALIDATION_STAGED, u64::from(which));
                put_answer(&answer[..written]);
                reply.answer_len = write_answer(written);
            }
            host_op::MODEL => {
                let written =
                    crate::engine::answer(driver, &request_bytes[..copied], framed.arg0, answer);
                put_answer(&answer[..written]);
                reply.answer_len = write_answer(written);
            }
            host_op::LOG => {
                let mut packed = 0u64;
                for byte in request_bytes[..copied.min(8)].iter() {
                    packed = (packed << 8) | u64::from(*byte);
                }
                k2::note(note::HOSTCALL, packed);
            }
            host_op::ASSERT_FAILED => {
                outcome.latched = true;
                reply.stopped = 1;
                k2::note(note::PROGRAM_LATCHED, framed.cookie);
            }
            host_op::NEEDS_MODEL => {
                outcome.metrics.finish = thalyx_user_k5pkg::proto::finish::NEEDS_MODEL;
            }
            host_op::FINISH => {
                if let Some(metrics) = RunMetrics::read_from(&request_bytes[..copied], 0) {
                    outcome.metrics = metrics;
                }
                k2::note(note::PROGRAM_FINISH, u64::from(outcome.metrics.finish));
                finished = true;
            }
            _ => {
                reply.status = 1;
            }
        }

        let _ = k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes());
        let _ = k2::cap_close(invocation);
        if finished {
            break;
        }
    }

    let stop = LaunchRequest {
        op: launch_op::STOP_RUNTIME,
        tool_id: 0,
        candidate_len: 0,
        candidate_digest: [0u8; 32],
        seed: driver.seed,
        deadline_ns: 0,
    };
    let _ = k2::endpoint_call(
        driver.launcher,
        3,
        stop.as_bytes(),
        &[],
        k2::now_ns() + LAUNCH_DEADLINE_NS,
        false,
    );
    let _ = status::OK;
    let _ = ToolReport::zeroed();
    outcome
}
