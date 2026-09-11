//! The work domain: Thalyx's Core, for one piece of work, on this kernel.
//!
//! Thalyx keeps intention, the human interface, module contracts, semantic
//! authorisation, knowledge, validation policy and the decision to publish. The
//! kernel provides isolation, object authority, the life of work and resources.
//! This program is where those two meet for one piece of work: it holds the
//! identity that may publish, opens the private workspace, drives the language
//! runtime, asks a tool for a verdict, and decides.
//!
//! What it holds is the argument. It has a facet of the state service and no
//! device capability; it has a facet of a launcher and no authority to build a
//! domain; it has a facet of the engine and no weights. Every sentence about
//! what this program cannot do is a sentence about its capability table, which
//! the supervisor built and the kernel enforces.
//!
//! The vertical is `vault/roadmap/phases.md`: obtain the context of a version,
//! execute private changes, validate with a real tool, publish or abandon,
//! consult the evidence, and survive a crash in publication.

#![no_std]
#![no_main]

mod content;
mod engine;
mod hacer;
mod store;
mod tree;
mod verbs;

use thalyx_abi::{boot_handle, right, status};
use thalyx_user_k4fmt::generated::{
    Policy, StoreRequest, Validation, object_type, result_outcome, store_op, store_status,
};
use thalyx_user_k4fmt::{self as k4, Binding, Pod};
use thalyx_user_k5pkg::proto::{
    WorkConfig, bit, engine_case, note, work_addr, work_role, work_slot,
};
use thalyx_user_k5pkg::thalyx::verdict;
use thalyx_user_rt::k2::{self, report};
use thalyx_user_rt::{self as rt, entry_on_stack};

use store::{Answer, Store};
use tree::Workspace;

/// The workspace, in `.bss` and not on the stack.
///
/// A domain of this kernel gets eight pages of stack and this structure is
/// twenty-five kilobytes: the first version of this program put it in a local
/// and the domain died of a page fault eight bytes past its guard page, which
/// is exactly the failure a guard page exists to make legible. There is no
/// allocator in a user domain, so the alternative to a local is a static.
static mut WORKSPACE: Workspace = Workspace::new();

/// The workspace.
///
/// # Safety
///
/// A work domain has one thread. Nothing here is reachable from two.
fn workspace() -> &'static mut Workspace {
    // SAFETY: as the doc comment says.
    unsafe { &mut *(&raw mut WORKSPACE) }
}

/// A pattern this domain plants in its FP registers and finds again, so that
/// "the run was preempted and continued" is checked and not assumed.
const FP_BASE: u64 = 0x5B21_7777_1111_9001;

/// The tool identity the validation policy requires.
const TOOL_ID: u64 = 0x4B35_2001;

/// How long a rival waits for the publisher's seed version.
const SEED_WAIT_NS: u64 = 60_000_000_000;

/// The coverage a validation must claim, and the coverage the tool reports.
const MIN_COVERAGE_PPM: u64 = 900_000;
const COVERAGE_PPM: u64 = 1_000_000;

/// A fixed instant the validation records carry, so a record is a function of
/// its inputs and not of when the run happened.
const EVIDENCE_EPOCH_NS: u64 = 1_700_000_000_000_000_000;

/// The role this domain was built as, written by the supervisor into a page it
/// mapped read-only. What a domain is, is not the program's choice.
fn configuration() -> WorkConfig {
    // SAFETY: the supervisor maps one readable page there before activation.
    let bytes = unsafe {
        core::slice::from_raw_parts(work_addr::CONFIG as *const u8, size_of::<WorkConfig>())
    };
    WorkConfig::read_from(bytes, 0).unwrap_or(WorkConfig {
        role: 0,
        principal: 0,
        scenario: 0,
        leg: 0,
        seed: 0,
        uses_runtime: 0,
        uses_engine: 0,
        inferences: 0,
        reserved0: 0,
    })
}

/// The policy every version in this run is published under.
///
/// It requires a validation, names the tool identity that may produce one, and
/// says which principals may publish. Authority still comes from the facet: this
/// only says which of the principals the service authenticated is admitted.
fn policy() -> Policy {
    let mut config_digest = [0u8; 32];
    for (index, byte) in config_digest.iter_mut().enumerate() {
        *byte = 0x35 ^ (index as u8);
    }
    Policy {
        format: 1,
        requires_validation: 1,
        publish_principals: (1 << 1) | (1 << 2),
        validation_tool_id: TOOL_ID,
        min_coverage_ppm: MIN_COVERAGE_PPM,
        tool_config_digest: config_digest,
    }
}

/// A validation record: a limited claim about named inputs and a named version.
///
/// It names the tool that produced it and the configuration that tool ran
/// under, because a record that only said "a check passed" would be a record
/// nobody could re-run. When a real tool produced the verdict, the identity is
/// the one the launcher reported having run, not one this program chose.
fn validation_for(
    tree: [u8; 32],
    generation: u64,
    result: u32,
    tool_id: u64,
    tool_config_digest: [u8; 32],
) -> Validation {
    Validation {
        format: 1,
        result,
        input_tree_digest: tree,
        base_generation: generation,
        tool_id,
        tool_config_digest,
        coverage_ppm: COVERAGE_PPM,
        produced_ns: EVIDENCE_EPOCH_NS,
    }
}

/// What one attempt at a version came to.
#[derive(Clone, Copy)]
struct Candidate {
    root: [u8; 32],
    policy: [u8; 32],
    validation: [u8; 32],
    tree: [u8; 32],
}

/// Stages every entry of the workspace and freezes it into a candidate root.
///
/// `claims` is the version the evidence says it was computed against, separate
/// from the version the workspace was forked over, because a control that wants
/// to be refused for a stale expectation has to offer evidence naming the stale
/// expectation.
fn freeze(
    store: &mut Store,
    workspace: &Workspace,
    fork_over: u64,
    claims: u64,
    result: u32,
    names_inputs: bool,
    tool: (u64, [u8; 32]),
) -> Option<Candidate> {
    /// Says which step of a freeze refused before answering `None`.
    ///
    /// Written after the first version of this function answered `None` from
    /// six places and the whole vertical stopped at the first one with nothing
    /// in the record to say which. A helper that can fail silently is a helper
    /// that will.
    fn at(step: u64) -> Option<Candidate> {
        k2::note(note::WORK_UNEXPECTED, 0x0F00 | step);
        None
    }

    let Some(policy_digest) = store.put_object(object_type::POLICY, policy().as_bytes()) else {
        return at(1);
    };

    let mut digests = [[0u8; 32]; tree::MAX_ENTRIES];
    for (index, entry) in workspace.entries().iter().enumerate() {
        match store.put_object(object_type::BYTES, entry.bytes()) {
            Some(digest) => digests[index] = digest,
            None => return at(2),
        }
    }

    let mut bindings =
        [Binding::new(b"x", object_type::BYTES, 0, 0, [0u8; 32]).ok()?; tree::MAX_ENTRIES];
    let Some(count) = workspace.bindings(&digests, &mut bindings) else {
        return at(3);
    };
    let mut scratch = [0u8; 2048];
    let Ok(written) = k4::encode_tree(&mut scratch, &bindings[..count]) else {
        return at(4);
    };
    let tree_digest = k4::object_digest(object_type::TREE, &scratch[..written]);

    let named = if names_inputs {
        tree_digest
    } else {
        k4::object_digest(object_type::TREE, b"not these inputs")
    };
    let validation = validation_for(named, claims, result, tool.0, tool.1);
    let Some(validation_digest) = store.put_object(object_type::VALIDATION, validation.as_bytes())
    else {
        return at(5);
    };
    k2::note(note::VALIDATION_STAGED, u64::from(result));

    let Some(handle) = store.fork(fork_over) else {
        return at(6);
    };
    k2::note(note::WORKSPACE_OPEN, handle);
    for (index, entry) in workspace.entries().iter().enumerate() {
        if store.bind(handle, entry.name(), digests[index]).is_none() {
            return at(7);
        }
        k2::note(note::WORKSPACE_WROTE, entry.bytes().len() as u64);
    }
    let Some(frozen) = store.freeze(handle, policy_digest, validation_digest) else {
        return at(8);
    };
    store.discard(handle);
    if frozen.digest1 != tree_digest {
        // The service encoded the same bindings and got other bytes. Nothing
        // after this would mean anything.
        k2::note(note::WORK_UNEXPECTED, 0x0BEC_7002);
        return None;
    }
    k2::note(note::CANDIDATE_FROZEN, count as u64);
    Some(Candidate {
        root: frozen.digest0,
        policy: policy_digest,
        validation: validation_digest,
        tree: tree_digest,
    })
}

/// Reads a published version's whole content into the workspace.
///
/// This is `contexto`'s substrate and the workspace's baseline in one: what a
/// work is against is a version, read once, and everything it then does is a
/// difference from that.
fn load_version(store: &mut Store, root: [u8; 32], workspace: &mut Workspace) -> Option<usize> {
    // The root is a manifest; its tree digest names the bindings.
    let manifest_len = size_of::<k4::generated::Manifest>() as u64;
    let got = store.read_object(root, manifest_len)?;
    if got != manifest_len {
        return None;
    }
    let manifest = k4::generated::Manifest::read_from(store::stage(), 0)?;

    let mut scratch = [0u8; 2048];
    let tree_len = (k4::generated::TreeHeader::SIZE
        + manifest.entry_count as usize * k4::generated::TreeEntry::SIZE) as u64;
    let got = store.read_object(manifest.tree_digest, tree_len)?;
    if got != tree_len {
        return None;
    }
    scratch[..tree_len as usize].copy_from_slice(&store::stage()[..tree_len as usize]);

    let mut bindings =
        [Binding::new(b"x", object_type::BYTES, 0, 0, [0u8; 32]).ok()?; tree::MAX_ENTRIES];
    let count = k4::decode_tree(&scratch[..tree_len as usize], &mut bindings).ok()?;

    for binding in bindings.iter().take(count) {
        let length = binding.length;
        let got = store.read_object(binding.digest, length)?;
        if got != length {
            return None;
        }
        let mut bytes = [0u8; tree::MAX_BYTES];
        bytes[..length as usize].copy_from_slice(&store::stage()[..length as usize]);
        workspace
            .put(binding.name(), &bytes[..length as usize], true)
            .ok()?;
    }
    Some(count)
}

/// Publishes the seed version: the state the work is then done against.
///
/// It is a publication like any other -- conditioned on generation zero,
/// validated, and refused if anything about it is wrong -- because a seed that
/// arrived by a different path would be a version nothing had checked.
fn publish_seed(store: &mut Store, config: &WorkConfig) -> Option<u64> {
    let seed = workspace();
    seed.put(b"module.js", content::MODULE, true).ok()?;
    seed.put(b"module.test.js", content::TESTS, true).ok()?;
    // The program the version carries is the one for what this run has: with a
    // resident engine to ask, the program asks it.
    let program = if config.uses_engine != 0 {
        content::PROGRAM_ENGINE
    } else {
        content::PROGRAM
    };
    seed.put(b"program.js", program, true).ok()?;
    seed.put(b"notes.md", content::NOTES, true).ok()?;

    // The seed version is published under a validation this program asserts
    // about itself, and the evidence says so. Nothing has run a tool over it:
    // it is the state the work is then done *against*, and the work's own
    // publication is the one a real tool decides.
    let candidate = freeze(
        store,
        seed,
        0,
        0,
        verdict::PASSED,
        true,
        (TOOL_ID, policy().tool_config_digest),
    )?;
    store.sequence += 1;
    let sequence = store.sequence;
    match store.publish(
        sequence,
        0,
        candidate.root,
        candidate.policy,
        candidate.validation,
        0,
        [0u8; 32],
    ) {
        Answer::Reply(reply) if reply.status == store_status::OK => {
            k2::note(note::PUBLISHED, reply.generation);
            k2::note(note::ROOT_PREFIX, prefix(&candidate.root));
            Some(reply.generation)
        }
        Answer::Reply(reply) => {
            k2::note(note::PUBLISH_REFUSED, u64::from(reply.status));
            let _ = config;
            None
        }
        Answer::Outcome(code) | Answer::Gone(code) => {
            k2::note(note::PUBLISH_REFUSED, (-code) as u64 | (1 << 32));
            None
        }
    }
}

/// The first eight bytes of a digest, as one number a note can carry.
fn prefix(digest: &[u8; 32]) -> u64 {
    let mut value = 0u64;
    for byte in digest.iter().take(8) {
        value = (value << 8) | u64::from(*byte);
    }
    value
}

/// The tree digest of a workspace, computed without the service.
///
/// Pure arithmetic over the same encoder the service uses, so a validation can
/// name the inputs it was about *before* anything is staged. That the freeze
/// afterwards produces the same digest is what ties the verdict to the
/// candidate: a program that changed the workspace after validating it gets a
/// tree the evidence does not name, and the publication is refused for exactly
/// that reason rather than accepted with a stale claim.
pub fn candidate_digest(workspace: &Workspace) -> Option<[u8; 32]> {
    let mut digests = [[0u8; 32]; tree::MAX_ENTRIES];
    for (index, entry) in workspace.entries().iter().enumerate() {
        digests[index] = k4::object_digest(object_type::BYTES, entry.bytes());
    }
    let mut bindings =
        [Binding::new(b"x", object_type::BYTES, 0, 0, [0u8; 32]).ok()?; tree::MAX_ENTRIES];
    let count = workspace.bindings(&digests, &mut bindings)?;
    let mut scratch = [0u8; 2048];
    let written = k4::encode_tree(&mut scratch, &bindings[..count]).ok()?;
    Some(k4::object_digest(object_type::TREE, &scratch[..written]))
}

/// Bytes one verb answer may occupy. In `.bss`, for the reason the workspace
/// is: a domain of this kernel has eight pages of stack.
const ANSWER_BYTES: usize = 8192;

static mut ANSWER: [u8; ANSWER_BYTES] = [0; ANSWER_BYTES];

/// Where a candidate is assembled before it is sealed. In `.bss` for the same
/// reason everything else large is.
static mut CANDIDATE: [u8; thalyx_user_k5pkg::proto::CANDIDATE_MAX] =
    [0; thalyx_user_k5pkg::proto::CANDIDATE_MAX];

/// The candidate buffer.
///
/// # Safety
///
/// One thread, and the borrow ends before the next caller takes it.
fn candidate_buffer() -> &'static mut [u8] {
    // SAFETY: as the doc comment says.
    unsafe { &mut *(&raw mut CANDIDATE) }
}

/// The answer buffer.
///
/// # Safety
///
/// A work domain has one thread, and every caller is done with the borrow
/// before the next takes it.
fn answer_buffer() -> &'static mut [u8; ANSWER_BYTES] {
    // SAFETY: as the doc comment says.
    unsafe { &mut *(&raw mut ANSWER) }
}

/// The value of a numeric field of a JSON answer, if it has one.
///
/// A bounded scan and not a parser: what this reads is what the verb surface in
/// this same program wrote, and a general JSON reader here would be a second
/// opinion about a format neither side chose freely.
fn json_number(answer: &[u8], key: &[u8]) -> Option<u64> {
    let mut pattern = [0u8; 32];
    if key.len() + 3 > pattern.len() {
        return None;
    }
    pattern[0] = b'"';
    pattern[1..1 + key.len()].copy_from_slice(key);
    pattern[1 + key.len()] = b'"';
    pattern[2 + key.len()] = b':';
    let at = tree::find_bytes(answer, &pattern[..3 + key.len()])? + key.len() + 3;
    let mut value = 0u64;
    let mut any = false;
    for byte in &answer[at..] {
        if byte.is_ascii_digit() {
            value = value * 10 + u64::from(byte - b'0');
            any = true;
        } else {
            break;
        }
    }
    any.then_some(value)
}

/// Whether an answer says the verb worked.
fn json_ok(answer: &[u8]) -> bool {
    tree::find_bytes(answer, b"\"ok\":true").is_some()
}

/// The whole vertical for the role that carries it through.
///
/// Every change this makes to the workspace goes through the same verb surface
/// a program in the language runtime reaches, and through no other path. The
/// `surface` stage drives that surface from a script written here; the `work`
/// stage drives the same surface from a program the runtime executes. If the
/// two went through different code, "a program can reach exactly what its calls
/// could have reached one at a time" would be a hope rather than a fact.
fn run_publisher(store: &mut Store, config: &WorkConfig) -> bool {
    let Some(state) = store.query() else {
        k2::note(note::WORK_UNEXPECTED, 0x0E00);
        return false;
    };
    k2::note(note::VERSION_SEEN, state.generation);

    if state.generation == 0 && publish_seed(store, config).is_none() {
        return false;
    }

    let Some(state) = store.query() else {
        k2::note(note::WORK_UNEXPECTED, 0x0E01);
        return false;
    };
    let workspace = workspace();
    *workspace = Workspace::new();
    let Some(entries) = load_version(store, state.digest0, workspace) else {
        k2::note(note::WORK_UNEXPECTED, 0x0E02);
        return false;
    };
    k2::note(
        note::VERSION_SEEN,
        state.generation | ((entries as u64) << 32),
    );

    let mut mark = [0u8; content::MARK_LEN];
    content::mark_of(config.seed, &mut mark);
    let mut context = verbs::Context {
        generation: state.generation,
        root: state.digest0,
        seed: config.seed,
        role: config.role,
        open: true,
        workspace,
    };

    // The script. In the `surface` stage it is a fixture and the evidence says
    // so; what the host chooses is the seed, and the mark the script writes is
    // derived from it, so nothing about the published bytes could have been
    // computed before the run began.
    let script: [(&[u8], &[&[u8]]); 7] = [
        (b"estado", &[]),
        (b"contexto", &[b"checksum"]),
        (b"leer", &[content::NAME_MODULE]),
        (
            b"sustituir",
            &[content::NAME_MODULE, content::ZERO_MARK, &mark],
        ),
        (b"leer", &[content::NAME_MODULE]),
        (b"cambios", &[]),
        (b"buscar", &[&mark]),
    ];

    let mut uses = 0u64;
    let mut changed = 0u64;
    let mut hits = 0u64;
    for (name, args) in script {
        verbs::note_call(name);
        let written = verbs::answer(&mut context, name, args, answer_buffer());
        let answer = &answer_buffer()[..written];
        if !json_ok(answer) {
            k2::note(note::WORK_UNEXPECTED, 0x0E10);
            return false;
        }
        if name == b"contexto" {
            uses = json_number(answer, b"uses").unwrap_or(0);
            k2::note(note::CONTEXT_ANSWERED, uses);
        }
        if name == b"cambios" {
            changed = json_number(answer, b"count").unwrap_or(0);
        }
        if name == b"buscar" {
            hits = json_number(answer, b"total").unwrap_or(0);
        }
    }
    if uses < 2 || changed != 1 || hits < 1 {
        // The script asserted three things about what it did. A run that got
        // here with any of them false has changed something other than what it
        // meant to, and publishing that would be publishing an accident.
        k2::note(
            note::WORK_UNEXPECTED,
            0x0E11 | (uses << 16) | (changed << 32) | (hits << 48),
        );
        return false;
    }
    k2::note(note::WORKSPACE_WROTE, changed);

    let Some(candidate) = freeze(
        store,
        context.workspace,
        state.generation,
        state.generation,
        verdict::PASSED,
        true,
        (TOOL_ID, policy().tool_config_digest),
    ) else {
        return false;
    };

    store.sequence += 1;
    let sequence = store.sequence;
    let published = match store.publish(
        sequence,
        state.generation,
        candidate.root,
        candidate.policy,
        candidate.validation,
        0,
        [0u8; 32],
    ) {
        Answer::Reply(reply) if reply.status == store_status::OK => {
            k2::note(note::PUBLISHED, reply.generation);
            k2::note(note::ROOT_PREFIX, prefix(&candidate.root));
            true
        }
        Answer::Reply(reply) => {
            k2::note(note::PUBLISH_REFUSED, u64::from(reply.status));
            false
        }
        Answer::Outcome(code) | Answer::Gone(code) => {
            k2::note(note::PUBLISH_REFUSED, (-code) as u64 | (1 << 32));
            false
        }
    };
    let _ = candidate.tree;

    // The evidence a caller can consult after the fact: the published version,
    // read back through the service by the path anybody else would use, and
    // checked for the mark this run wrote.
    if published && let Some(final_state) = store.query() {
        k2::note(note::FINAL_GENERATION, final_state.generation);
        let check = context.workspace;
        *check = Workspace::new();
        if let Some(count) = load_version(store, final_state.digest0, check) {
            let marked = check
                .read(content::NAME_MODULE)
                .is_some_and(|bytes| tree::find_bytes(bytes, &mark).is_some());
            k2::note(
                note::EVIDENCE_READ,
                (count as u64) | (u64::from(marked) << 32),
            );
        }
    }
    published
}

/// Finds the first request identity this principal has not spent.
///
/// A work that was cut has to find out what became of what it may already have
/// asked for before it asks for anything else. Guessing is not available: a
/// request identity with a durable result is answered with that result
/// forever, and reusing one for different inputs is a conflict rather than a
/// retry. So the work walks its own sequences forward, asking the service
/// about each, and starts again after the last one that is spent -- whether it
/// committed, was aborted by recovery, or is one the store cannot say about.
/// Only `NEVER_SEEN` is free.
fn resume_after(store: &mut Store, limit: u64) -> u64 {
    let mut spent = 0u64;
    let mut outcome = result_outcome::NEVER_SEEN;
    while spent < limit {
        let Some(reply) = store.result(spent + 1) else {
            break;
        };
        if reply.outcome == result_outcome::NEVER_SEEN {
            break;
        }
        spent += 1;
        outcome = reply.outcome;
    }
    k2::note(note::WORK_RESUMED, spent | (u64::from(outcome) << 8));
    spent
}

/// Whether the published version already carries this work's change.
///
/// After a cut the answer decides everything: a change that is published is
/// not redone -- the retry is answered from the durable result, as K4's
/// evidence puts it -- and a change that is not is the whole vertical again.
fn already_published(workspace: &Workspace, config: &WorkConfig) -> bool {
    let mut mark = [0u8; content::MARK_LEN];
    verbs::mark_for(config.seed, config.role, &mut mark);
    let intent = verbs::intent_of(config.role);
    workspace
        .read(intent.target)
        .is_some_and(|bytes| tree::find_bytes(bytes, &mark).is_some())
}

/// The vertical, driven by a program the language runtime executes.
///
/// The difference from the `surface` stage is not the verb surface -- that is
/// the same code -- but *who decides what to call*, and *what decides whether
/// to publish*. Here a program does the first, and a real tool run in a domain
/// of its own does the second: the publication happens only when that tool
/// exited zero over the exact candidate the record names.
///
/// Two things around it are what EXP-10 asks for. The work may be starting
/// again after a cut, in which case it first finds its spent request
/// identities and looks at whether its change is already published. And its
/// publication may be refused because another work published first, in which
/// case it starts the whole thing again -- context, program, tool -- over the
/// version that won, once: a second refusal is a fact the run records.
fn run_with_runtime(store: &mut Store, config: &WorkConfig) -> bool {
    let Some(state) = store.query() else {
        k2::note(note::WORK_UNEXPECTED, 0x0E00);
        return false;
    };
    k2::note(note::VERSION_SEEN, state.generation);
    if config.leg > 1 {
        store.sequence = resume_after(store, 8);
    }
    if config.role == work_role::RIVAL {
        // A rival is a second work over a published version. It waits for
        // one: the seed is the publisher's to publish, and a rival that raced
        // it for the seed would be racing the fixture rather than the work.
        let by = k2::now_ns() + SEED_WAIT_NS;
        loop {
            let Some(now) = store.query() else {
                k2::note(note::WORK_UNEXPECTED, 0x0E04);
                return false;
            };
            if now.generation >= 1 {
                break;
            }
            if k2::now_ns() > by {
                k2::note(note::WORK_UNEXPECTED, 0x0E05);
                return false;
            }
            let _ = k2::signal_wait(
                boot_handle(work_slot::DONE),
                1 << 63,
                k2::now_ns() + 20_000_000,
            );
        }
        probe_authority(store);
    } else if state.generation == 0 && publish_seed(store, config).is_none() {
        return false;
    }

    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let Some(state) = store.query() else {
            k2::note(note::WORK_UNEXPECTED, 0x0E01);
            return false;
        };
        let workspace = workspace();
        *workspace = Workspace::new();
        let Some(entries) = load_version(store, state.digest0, workspace) else {
            k2::note(note::WORK_UNEXPECTED, 0x0E02);
            return false;
        };
        k2::note(
            note::VERSION_SEEN,
            state.generation | ((entries as u64) << 32),
        );

        if config.leg > 1 && already_published(workspace, config) {
            // The cut fell after the commit: the version exists and this is
            // what recovery adopted. There is nothing to redo, and redoing it
            // would publish a second version that says the same thing.
            k2::note(note::WORK_RECOVERED, state.generation);
            k2::note(note::FINAL_GENERATION, state.generation);
            k2::note(note::EVIDENCE_READ, (entries as u64) | (1 << 32));
            return true;
        }

        match attempt_with_runtime(store, config, state.generation, state.digest0) {
            Attempt::Published => return true,
            Attempt::Failed => return false,
            Attempt::Abandoned(went_through) => return went_through,
            Attempt::Stale if attempts < 2 => {
                // Another work published first. What this work did is done
                // against a version that is no longer the one, so it is not
                // reused: the workspace is dropped and the vertical starts
                // again over what is published now.
                if let Some(now) = store.query() {
                    k2::note(note::WORK_REBASED, now.generation);
                }
            }
            Attempt::Stale => return false,
        }
    }
}

/// A control the rival runs before it does anything: it asks the service for
/// the one operation its policy reserves to the publisher, and expects to be
/// refused. Its facet is its principal; nothing it can put in a request makes
/// it the other work, and the service is what says so.
fn probe_authority(store: &mut Store) {
    match store.call(
        &StoreRequest {
            op: store_op::COMPACT,
            ..StoreRequest::zeroed()
        },
        0,
    ) {
        Answer::Reply(reply) if reply.status == store_status::FORBIDDEN => {
            k2::note(
                report::REFUSED_AS_EXPECTED,
                u64::from(store_status::FORBIDDEN),
            );
        }
        Answer::Reply(reply) => k2::note(report::NOT_REFUSED, u64::from(reply.status)),
        Answer::Outcome(code) | Answer::Gone(code) => {
            k2::note(report::NOT_REFUSED, (-code) as u64 | (1 << 32));
        }
    }
}

/// What one attempt at the vertical came to.
enum Attempt {
    Published,
    Failed,
    Abandoned(bool),
    Stale,
}

/// One attempt: the program, the tool, the publication, against `generation`.
fn attempt_with_runtime(
    store: &mut Store,
    config: &WorkConfig,
    generation: u64,
    root: [u8; 32],
) -> Attempt {
    let workspace = workspace();
    // The program comes out of the published version, like everything else the
    // work is about. In Thalyx it arrives from an inference; here it is content
    // under a name, and the evidence says which of the two happened.
    let mut program = [0u8; tree::MAX_BYTES];
    let program_len = match workspace.read(content::NAME_PROGRAM) {
        Some(bytes) => {
            program[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        }
        None => {
            k2::note(note::WORK_UNEXPECTED, 0x0E03);
            return Attempt::Failed;
        }
    };

    let mut driver = hacer::Driver {
        context: verbs::Context {
            generation,
            root,
            seed: config.seed,
            role: config.role,
            open: true,
            workspace,
        },
        launcher: boot_handle(work_slot::LAUNCH),
        inbound: boot_handle(work_slot::HOST),
        engine: if config.uses_engine != 0 {
            boot_handle(work_slot::ENGINE)
        } else {
            0
        },
        prompt: boot_handle(work_slot::PROMPT),
        seed: config.seed,
        validated_tree: [0u8; 32],
    };

    let outcome = hacer::run(
        &mut driver,
        &program[..program_len],
        candidate_buffer(),
        answer_buffer(),
    );
    k2::note(
        note::PROGRAM_FINISH,
        u64::from(outcome.metrics.finish)
            | (u64::from(outcome.metrics.requests) << 8)
            | (u64::from(outcome.metrics.validations) << 16)
            | (u64::from(outcome.metrics.assertions) << 24)
            | (outcome.metrics.answer_bytes << 32),
    );

    // Two things have to be true to publish, and they are different things: the
    // program ran to the end, and a tool said the candidate holds. A program
    // that stopped short did not produce a wrong answer, and a tool that could
    // not run did not say no -- so neither is folded into the other.
    let went_through = thalyx_user_k5pkg::thalyx::went_through(outcome.metrics.finish);
    let passed = outcome.verdict == verdict::PASSED;
    if !went_through || !passed {
        // Abandoning: the workspace is dropped and nothing published moves. The
        // evidence of the attempt survives, which is the whole point of the
        // distinction between a workspace and a version.
        k2::note(
            note::ABANDONED,
            u64::from(outcome.metrics.finish) | (u64::from(outcome.verdict) << 8),
        );
        if let Some(final_state) = store.query() {
            k2::note(note::FINAL_GENERATION, final_state.generation);
        }
        return Attempt::Abandoned(
            went_through || outcome.metrics.finish == thalyx_user_k5pkg::proto::finish::ASSERTION,
        );
    }

    let Some(candidate) = freeze(
        store,
        driver.context.workspace,
        generation,
        generation,
        verdict::PASSED,
        true,
        (outcome.tool_id, outcome.tool_config_digest),
    ) else {
        // A freeze forks over the version the work is against, and the service
        // refuses a fork over a generation that is no longer the published one.
        // That is the same refusal a publication would have met, one step
        // earlier: the version moved under this work.
        if let Some(now) = store.query()
            && now.generation != generation
        {
            k2::note(
                note::PUBLISH_REFUSED,
                u64::from(store_status::GENERATION_STALE),
            );
            return Attempt::Stale;
        }
        return Attempt::Failed;
    };
    // The tree the tool was given, and the tree the service encoded. A program
    // that changed the workspace after validating it produces two different
    // ones, and the service refuses the publication for exactly that reason.
    k2::note(
        note::VALIDATION_STAGED,
        u64::from(driver.validated_tree == candidate.tree),
    );

    store.sequence += 1;
    let sequence = store.sequence;
    let published = match store.publish(
        sequence,
        generation,
        candidate.root,
        candidate.policy,
        candidate.validation,
        0,
        [0u8; 32],
    ) {
        Answer::Reply(reply) if reply.status == store_status::OK => {
            k2::note(note::PUBLISHED, reply.generation);
            k2::note(note::ROOT_PREFIX, prefix(&candidate.root));
            true
        }
        Answer::Reply(reply) if reply.status == store_status::GENERATION_STALE => {
            k2::note(note::PUBLISH_REFUSED, u64::from(reply.status));
            return Attempt::Stale;
        }
        Answer::Reply(reply) => {
            k2::note(note::PUBLISH_REFUSED, u64::from(reply.status));
            false
        }
        Answer::Outcome(code) | Answer::Gone(code) => {
            k2::note(note::PUBLISH_REFUSED, (-code) as u64 | (1 << 32));
            false
        }
    };

    if published && let Some(final_state) = store.query() {
        k2::note(note::FINAL_GENERATION, final_state.generation);
        let mut mark = [0u8; content::MARK_LEN];
        verbs::mark_for(config.seed, config.role, &mut mark);
        let intent = verbs::intent_of(config.role);
        let check = driver.context.workspace;
        *check = Workspace::new();
        if let Some(count) = load_version(store, final_state.digest0, check) {
            let marked = check
                .read(intent.target)
                .is_some_and(|bytes| tree::find_bytes(bytes, &mark).is_some());
            k2::note(
                note::EVIDENCE_READ,
                (count as u64) | (u64::from(marked) << 32),
            );
        }
    }
    if published {
        Attempt::Published
    } else {
        Attempt::Failed
    }
}

/// The work that is closed while the engine computes for it.
///
/// It reads the version it is against, says on its signal that it is about to
/// ask, and asks the engine for more tokens than anybody will wait for. Its
/// supervisor closes its scope while that runs. What happens then is the
/// kernel's and the engine's to do, not this program's: the call comes back
/// `CANCELLED`, or does not come back at all, and the work leaves.
fn run_asker(store: &mut Store, config: &WorkConfig) -> bool {
    let Some(state) = store.query() else {
        k2::note(note::WORK_UNEXPECTED, 0x0E00);
        return false;
    };
    k2::note(note::VERSION_SEEN, state.generation);
    let workspace = workspace();
    *workspace = Workspace::new();
    let mut driver = hacer::Driver {
        context: verbs::Context {
            generation: state.generation,
            root: state.digest0,
            seed: config.seed,
            role: config.role,
            open: false,
            workspace,
        },
        launcher: 0,
        inbound: 0,
        engine: boot_handle(work_slot::ENGINE),
        prompt: boot_handle(work_slot::PROMPT),
        seed: config.seed,
        validated_tree: [0u8; 32],
    };
    let predict = u64::from(engine_case::LONG_PREDICT);
    k2::note(note::WORK_ASKING, predict);
    let _ = k2::signal_raise(boot_handle(work_slot::DONE), bit::WORK_ASKING);
    let written = engine::answer(
        &mut driver,
        engine_case::LONG_PROMPTS[0].as_bytes(),
        predict,
        answer_buffer(),
    );
    let answer = &answer_buffer()[..written];
    let cancelled = tree::find_bytes(answer, b"\"error\":\"cancelled\"").is_some();
    k2::note(note::CANCELLED, u64::from(cancelled));
    cancelled
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let config = configuration();
    let mut store = Store {
        facet: boot_handle(work_slot::STORE),
        stage_cap: boot_handle(work_slot::STAGE),
        calls: 0,
        sequence: 0,
    };

    let done = boot_handle(work_slot::DONE);
    let ok = match config.role {
        work_role::PUBLISHER | work_role::RIVAL if config.uses_runtime != 0 => {
            run_with_runtime(&mut store, &config)
        }
        work_role::PUBLISHER => run_publisher(&mut store, &config),
        work_role::ASKER => run_asker(&mut store, &config),
        other => {
            k2::note(note::WORK_UNEXPECTED, u64::from(other));
            false
        }
    };

    if let Ok(info) = k2::scope_query(boot_handle(work_slot::SELF_SCOPE)) {
        k2::note(note::WORK_CPU, info.cpu_total_ns);
    }
    k2::note(
        note::WORK_DONE,
        u64::from(config.role) | (u64::from(ok) << 32),
    );
    let _ = k2::signal_raise(done, thalyx_user_k5pkg::proto::bit::WORK_DONE);
    let _ = report::DONE;
    let _ = right::INSPECT;
    let _ = status::OK;
    rt::exit(u64::from(!ok))
}

// A quarter of a megabyte, in this domain's own `.bss`. The kernel's eight
// pages are not enough for a call chain that carries a workspace, a tree
// encoder and a JSON writer at once.
entry_on_stack!(run, 256 * 1024);
