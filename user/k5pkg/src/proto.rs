//! What the K5 package's programs say to each other.
//!
//! Five protocols meet in this package and only one of them is inherited: the
//! managed-state protocol is K4's, unchanged, because K5's obligation is to use
//! the durable service that exists rather than to grow a second one. The four
//! new ones are the port's own.
//!
//! Every message here fits in `MAX_INLINE_PAYLOAD`. That is not a coincidence
//! and not a constraint worked around: anything larger travels in a memory
//! object whose capability the message carries, so a large transfer is an
//! authority a receiver was given rather than a copy nobody accounted for.

use thalyx_user_k4fmt::Pod;

pub use crate::generated::{
    ANSWER_MAX, CANDIDATE_ENTRIES, CANDIDATE_FLAG_FUNCTION_BODY, CANDIDATE_MAGIC_LOW,
    CANDIDATE_MAX, CHANNEL_ANSWER_MAX, CHANNEL_ANSWER_OFFSET, CHANNEL_PROGRAM_MAX,
    CHANNEL_PROGRAM_OFFSET, CHANNEL_REQUEST_MAX, CHANNEL_REQUEST_OFFSET, CandidateEntry,
    CandidateHeader, EngineReply, EngineRequest, HostReply, HostRequest, LaunchReply,
    LaunchRequest, PROMPT_MAX, RunMetrics, ToolReport, engine_case, engine_op, engine_status,
    finish, host_op, launch_op, launch_status, note, scenario, verdict,
};

/// Capability slots the supervisor fills in a work domain.
///
/// A work domain is Thalyx's Core for one piece of work: it holds the identity
/// that may publish, the workspace, the language runtime it drives, and the
/// tool it asks for a verdict. It holds no device capability and no authority
/// to build a domain: launching is a service it calls.
pub mod work_slot {
    /// A facet of the managed-state service. This is the principal.
    pub const STORE: u32 = 1;
    /// The staging buffer this work lends to the state service.
    pub const STAGE: u32 = 2;
    /// A facet of the launcher, for running a tool over a sealed candidate.
    pub const LAUNCH: u32 = 3;
    /// The signal this work raises when it has finished.
    pub const DONE: u32 = 4;
    /// The control log, for control-plane receipts.
    pub const LOG: u32 = 5;
    /// The endpoint the language runtime calls this work on.
    pub const HOST: u32 = 6;
    /// The region shared with the language runtime.
    pub const CHANNEL: u32 = 7;
    /// Own scope: create the objects a candidate is sealed into.
    pub const SELF_SCOPE: u32 = 8;
    /// Own domain: map what it created.
    pub const SELF_DOMAIN: u32 = 9;
    /// A facet of the resident engine.
    pub const ENGINE: u32 = 10;
    /// The buffer this work lends to the engine, and nobody else's.
    pub const PROMPT: u32 = 11;
}

/// Where a work domain finds its mappings.
pub mod work_addr {
    /// The role, read-only.
    pub const CONFIG: u64 = 0x2000_0000;
    /// The staging buffer it lends to the state service.
    pub const STAGE: u64 = 0x2010_0000;
    /// Pages of that buffer.
    pub const STAGE_PAGES: u64 = 2;
    /// The region shared with the language runtime.
    pub const CHANNEL: u64 = 0x2020_0000;
    /// Pages of that region.
    pub const CHANNEL_PAGES: u64 = 8;
    /// The buffer it lends to the engine.
    pub const PROMPT: u64 = 0x2030_0000;
    /// Pages of that buffer.
    pub const PROMPT_PAGES: u64 = 2;
    /// Where a candidate is assembled before being sealed.
    pub const CANDIDATE: u64 = 0x2040_0000;
    /// Pages of a candidate.
    pub const CANDIDATE_PAGES: u64 = 4;
}

/// Signal bits the K5 package uses. K4's are separate and stay separate.
pub mod bit {
    /// A work domain has finished its script.
    pub const WORK_DONE: u64 = 1 << 1;
    /// A work domain is about to ask the engine for a long inference. Raised
    /// on its done signal, so a supervisor that means to close it while the
    /// engine computes for it knows when that is.
    pub const WORK_ASKING: u64 = 1 << 2;
    /// A work domain is asked to stop what it is doing.
    pub const CANCEL: u64 = 1 << 6;
    /// The engine has loaded its weights and is admitting requests.
    pub const ENGINE_READY: u64 = 1 << 7;
}

/// What a work domain was built to do.
pub mod work_role {
    /// The work that carries the vertical through to a publication.
    pub const PUBLISHER: u32 = 1;
    /// A second work over the same version, so two of them race.
    pub const RIVAL: u32 = 2;
    /// A work whose validation fails, so a refusal is observed rather than
    /// argued about.
    pub const FAILING: u32 = 3;
    /// A work that abandons on purpose after changing its workspace.
    pub const ABANDONER: u32 = 4;
    /// A work that asks the engine for one long inference and is closed by
    /// its supervisor while the engine computes for it. It never publishes.
    pub const ASKER: u32 = 5;
}

/// The script a work follows, written by the supervisor from the run's plan.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorkConfig {
    /// [`work_role`].
    pub role: u32,
    /// The facet number this work's store capability carries.
    pub principal: u32,
    /// Which scenario of the matrix this run is.
    pub scenario: u32,
    /// Which leg of the run this is, after a cut.
    pub leg: u32,
    /// Run seed. A work cannot precompute a run it does not know the seed of.
    pub seed: u64,
    /// Whether this work drives the language runtime.
    pub uses_runtime: u32,
    /// Whether this work asks the resident engine.
    pub uses_engine: u32,
    /// How many inferences it asks for.
    pub inferences: u32,
    /// Reserved, zero.
    pub reserved0: u32,
}

const _: () = assert!(size_of::<WorkConfig>() == 40);

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for WorkConfig {}
