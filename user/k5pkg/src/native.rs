//! The native target's contract, from the side that builds a native domain.
//!
//! Mirrors `user/native/include/thalyx/nrt.h` and
//! `user/native/include/thalyx/image.h`. Two definitions of one layout is
//! exactly the arrangement that drifts, so the checker regenerates neither:
//! it compares them, field by field, against the sizes asserted on both sides.

use thalyx_user_k4fmt::Pod;

/// Where every native domain finds the things a supervisor maps for it.
pub mod addr {
    /// The image, which the linker script places here.
    pub const IMAGE_BASE: u64 = 0x0040_0000;
    /// One read-only page: the boot record.
    pub const CONFIG: u64 = 0x1000_0000;
    /// The role-specific shared region.
    pub const SHARED: u64 = 0x1001_0000;
    /// Main stack region. `_start` moves the stack pointer to the top before
    /// it calls anything, so this must be mapped before activation.
    pub const STACK_BASE: u64 = 0x2000_0000;
    /// Top of the main stack.
    pub const STACK_TOP: u64 = 0x2020_0000;
    /// First heap arena. The program maps its own; the supervisor does not.
    pub const HEAP_BASE: u64 = 0x3000_0000;
    /// One slot per built thread, each aligned to its own size.
    pub const THREAD_STACKS: u64 = 0x4000_0000;
    /// Bytes in one thread's slot.
    pub const THREAD_SLOT: u64 = 0x0004_0000;
    /// Large read-only data: model weights, corpora.
    pub const BULK: u64 = 0x5000_0000;
    /// Bounded transfer buffers.
    pub const XFER: u64 = 0x6000_0000;
}

/// Pages of the main stack region. A language runtime's parser recurses, so
/// the eight pages the kernel maps under every domain are not enough.
pub const STACK_PAGES: u64 = (addr::STACK_TOP - addr::STACK_BASE) / 4096;

/// Threads a native domain may have besides the first one.
pub const THREAD_MAX: u32 = 3;

/// Bit the native runtime raises on its done signal when `th_main` returns.
pub const DONE_BIT: u64 = 1;

/// Capability slots a native domain is built with.
pub mod slot {
    /// Own work scope: create memory, read the budget.
    pub const SELF_SCOPE: u32 = 0;
    /// Own domain: map what it created.
    pub const SELF_DOMAIN: u32 = 1;
    /// The endpoint facet this program calls.
    pub const SERVICE: u32 = 2;
    /// The endpoint this program receives on.
    pub const INBOUND: u32 = 3;
    /// Control log, when the role may append.
    pub const LOG: u32 = 4;
    /// One bit per worker: there is work for you.
    pub const SIGNAL_WORK: u32 = 5;
    /// One bit per worker: I have finished.
    pub const SIGNAL_DONE: u32 = 6;
    /// Sealed bulk object.
    pub const BULK: u32 = 7;
    /// Transfer buffer the program lends.
    pub const XFER: u32 = 8;
    /// Role-specific.
    pub const AUX0: u32 = 9;
    /// Role-specific.
    pub const AUX1: u32 = 10;
    /// Role-specific.
    pub const AUX2: u32 = 11;
    /// Role-specific.
    pub const AUX3: u32 = 12;
    /// Role-specific.
    pub const AUX4: u32 = 13;
    /// Role-specific.
    pub const AUX5: u32 = 14;
}

/// Magic of the boot record, `"ZTHALXK5"` little-endian.
pub const CONFIG_MAGIC: u64 = 0x354B_584C_4148_545A;

/// The page a native program reads before it does anything else.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Config {
    /// [`CONFIG_MAGIC`]. A program that does not find it stops.
    pub magic: u64,
    /// Record version.
    pub version: u32,
    /// What this domain is. Written by the supervisor, never chosen.
    pub role: u32,
    /// Which one of that role.
    pub instance: u64,
    /// Pages the program may hold across all its heap arenas.
    pub heap_pages: u64,
    /// Pages in one arena: the growth step.
    pub arena_pages: u64,
    /// Bytes mapped at [`addr::SHARED`], zero when none.
    pub shared_bytes: u64,
    /// Bytes mapped at [`addr::BULK`], zero when none.
    pub bulk_bytes: u64,
    /// Bytes mapped at [`addr::XFER`], zero when none.
    pub xfer_bytes: u64,
    /// Run seed. A program cannot precompute a run it does not know the seed of.
    pub seed: u64,
    /// Role-specific.
    pub arg0: u64,
    /// Role-specific.
    pub arg1: u64,
    /// Role-specific.
    pub arg2: u64,
    /// Role-specific.
    pub arg3: u64,
    /// What the runtime itself should do, independent of the role. See
    /// [`flag`].
    pub flags: u64,
}

const _: () = assert!(size_of::<Config>() == 112);

/// Bits of [`Config::flags`].
pub mod flag {
    /// The runtime keeps its own startup evidence instead of writing it.
    ///
    /// Every note a program writes is a record the kernel puts on the
    /// diagnostic plane synchronously, and on this platform that costs about
    /// two milliseconds each -- measured, in `diag.summary`. Five of them
    /// while a domain starts is five of them inside whatever a launcher is
    /// timing. K5 reads those notes and asks for them; K6 times the start and
    /// does not, so K6 asks for the quiet runtime and the evidence it does
    /// consume is unaffected.
    pub const QUIET_STARTUP: u64 = 1;
}

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for Config {}

/// Magic of the image record, `"HLTXIMG5"` little-endian.
pub const IMAGE_MAGIC: u64 = 0x3547_4D49_5854_4C48;

/// What an image publishes to whoever launches it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ImageHeader {
    /// [`IMAGE_MAGIC`].
    pub magic: u64,
    /// Record version.
    pub version: u32,
    /// Reserved, zero.
    pub flags: u32,
    /// Entry point; equals the ELF entry.
    pub start: u64,
    /// Where a built thread other than the first one begins.
    pub thread_trampoline: u64,
    /// First byte of `.bss`.
    pub bss_start: u64,
    /// One past the last byte of `.bss`.
    pub bss_end: u64,
    /// Reserved, zero.
    pub reserved: u64,
}

const _: () = assert!(size_of::<ImageHeader>() == 56);

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for ImageHeader {}

/// Roles a native domain can be built as.
pub mod role {
    /// The target smoke program: does this target run at all.
    pub const SMOKE: u32 = 1;
    /// The language runtime that executes a model's program.
    pub const HACER: u32 = 2;
    /// The validation tool, launched confined for one candidate.
    pub const CHECK: u32 = 3;
    /// The resident inference engine.
    pub const ENGINE: u32 = 4;
}

/// Magic of the package plan, `"K5PLAN01"` little-endian.
pub const PLAN_MAGIC: u64 = 0x3130_4E41_4C50_354B;

/// What the host decided about this image, as a module of the package.
///
/// The stage is a build-time fact and lives here; anything that varies from one
/// run to the next lives on the medium instead, where the host writes it
/// without rebuilding an image. Keeping the two apart is what lets a case
/// matrix say "the same image, cut in a different place".
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Plan {
    /// [`PLAN_MAGIC`].
    pub magic: u64,
    /// Record version.
    pub version: u32,
    /// Which stage of the port this image runs.
    pub stage: u32,
    /// Seed the host fixed for this build.
    pub seed: u64,
    /// Stage-specific.
    pub arg0: u64,
    /// Stage-specific.
    pub arg1: u64,
    /// Stage-specific.
    pub arg2: u64,
    /// Stage-specific.
    pub arg3: u64,
}

const _: () = assert!(size_of::<Plan>() == 56);

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for Plan {}

/// Stages of the port, in the order they were made to run.
pub mod stage {
    /// The native C target itself.
    pub const SMOKE: u32 = 1;
    /// The first native Thalyx surface, driven by fixtures and an external
    /// agent. Partial integration, and labelled as such.
    pub const SURFACE: u32 = 2;
    /// A real bounded program and a real native tool.
    pub const WORK: u32 = 3;
    /// The resident inference engine and the toolchain the workload needs.
    pub const ENGINE: u32 = 4;
    /// The real Thalyx, outside the machine, over the services inside it:
    /// the state service, a work scope per transaction and the link that
    /// carries its managed protocol in. EXP-13's third arm.
    pub const THALYX: u32 = 5;
}
