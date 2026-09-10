//! What the K4 package's programs agree on.
//!
//! Four programs share a supervisor's layout decisions: which capability slot
//! holds what, where a mapping lands, which signal bit means what, and the
//! shape of the two protocols the package speaks. They are stated once here
//! because a layout two programs disagree about is a bug neither of them can
//! see.
//!
//! This is not authority. A slot number names nothing until the supervisor
//! installs a capability there, and an address is only reachable because a
//! mapping was made.
//!
//! It lives in the format crate rather than in the runtime because the block
//! protocol carries the fault modes the format defines, and because every
//! program that speaks either protocol already depends on the format.

use crate::Pod;

/// Facet values the supervisor binds on the store's endpoint.
///
/// The facet is the principal. Nothing in a request says who sent it: the
/// service reads the facet the kernel authenticated on the message, and a
/// program that wrote another principal's number into its own request would
/// have changed nothing.
pub mod facet {
    /// The publisher that owns the ordinary sequence of publications.
    pub const PUBLISHER: u64 = 1;
    /// A second publisher, so two of them race for the same generation.
    pub const RIVAL: u64 = 2;
    /// A principal the policy does not admit as a publisher.
    pub const READER: u64 = 3;
}

/// How many principals the service keeps durable state for.
pub const PRINCIPALS: usize = 4;

/// Capability slots the supervisor fills in the block driver.
pub mod disk_slot {
    /// The device, narrowed to mapping, interrupts and DMA.
    pub const DEVICE: u32 = 1;
    /// The signal the device's interrupt raises.
    pub const IRQ: u32 = 2;
    /// The driver's private rings, which it grants to the device.
    pub const RING: u32 = 3;
    /// The buffer it shares with the service, also granted to the device.
    pub const IOBUF: u32 = 4;
    /// The endpoint the driver receives block requests on.
    pub const SERVICE: u32 = 5;
}

/// Capability slots the supervisor fills in the state service.
pub mod store_slot {
    /// A facet of the block driver's endpoint.
    pub const DISK: u32 = 1;
    /// The buffer the service stages blocks in. Read once, to confirm it is
    /// the shape the mapping assumes, and then closed: a handle held for the
    /// life of a run is a grant nothing else can have.
    pub const IOBUF: u32 = 2;
    /// The endpoint the service receives store requests on.
    pub const SERVICE: u32 = 3;
    /// The control log, for control-plane receipts.
    pub const LOG: u32 = 4;
    /// A facet of the broker's endpoint, for outbox intents.
    pub const BROKER: u32 = 5;
    /// The signal the service raises to ask the run to end or be replaced.
    pub const CRASH: u32 = 6;
}

/// Capability slots the supervisor fills in a client.
pub mod client_slot {
    /// A facet of the state service's endpoint.
    pub const STORE: u32 = 1;
    /// The client's own staging buffer, which it lends by capability.
    pub const STAGE: u32 = 2;
    /// The endpoint a broker receives on.
    pub const SERVICE: u32 = 3;
    /// The signal a client raises when it has finished.
    pub const DONE: u32 = 4;
}

/// Where the block driver finds each device register window.
pub const REGION_VADDR: [u64; 4] = [0x4000_0000, 0x4001_0000, 0x4002_0000, 0x4003_0000];
/// Where the driver finds its private rings.
pub const RING_VADDR: u64 = 0x5000_0000;
/// Pages of that object. The last is the driver's own and is not granted.
pub const RING_PAGES: u64 = 4;
/// Pages of it the device may reach.
pub const RING_GRANTED_PAGES: u64 = 3;
/// Where the driver and the service both find the shared buffer.
pub const IOBUF_VADDR: u64 = 0x5100_0000;
/// Pages of that buffer, each one block.
pub const IOBUF_PAGES: u64 = 4;
/// Where a client finds its role, read-only.
pub const CONFIG_VADDR: u64 = 0x2000_0000;
/// Where a client finds its staging buffer.
pub const STAGE_VADDR: u64 = 0x2010_0000;
/// Pages of that buffer.
pub const STAGE_PAGES: u64 = 2;

/// Signal bits the package uses.
pub mod bit {
    /// The device raised its interrupt.
    pub const DEVICE: u64 = 1 << 0;
    /// A client has finished its script.
    pub const CLIENT_DONE: u64 = 1 << 1;
    /// The service asks the supervisor to end the run at a fault point.
    pub const CRASH: u64 = 1 << 2;
    /// The service asks the supervisor to stop it and start a new one.
    pub const RESTART: u64 = 1 << 3;
    /// The service finished recovery and is admitting requests.
    pub const READY: u64 = 1 << 4;
}

/// What a client was built to do in this run.
pub mod role {
    /// Publish a sequence of versions, and check what came back.
    pub const PUBLISHER: u64 = 1;
    /// Publish against the same generation, so one of the two must lose.
    pub const RIVAL: u64 = 2;
    /// Read versions and try what a reader is not allowed to do.
    pub const READER: u64 = 3;
    /// Answer outbox intents, sometimes with a result nobody can assert.
    pub const BROKER: u64 = 4;
}

/// Operations the block driver serves.
pub mod disk_op {
    /// Read `count` blocks into the shared buffer.
    pub const READ: u32 = 1;
    /// Write `count` blocks out of the shared buffer.
    pub const WRITE: u32 = 2;
    /// Flush the device's write cache.
    pub const FLUSH: u32 = 3;
    /// Refuse every further write, whatever it is. Scaffolding.
    pub const LATCH: u32 = 4;
    /// Report what the driver did and what it was made to do to it.
    pub const STATS: u32 = 5;
}

/// What the block driver answers with.
pub mod disk_status {
    /// The request was carried out.
    pub const OK: u32 = 0;
    /// The request did not name a legal block, count or offset.
    pub const INVALID: u32 = 1;
    /// The device reported a failure, or the harness made it look like one.
    pub const IO_ERROR: u32 = 2;
    /// Writes are latched: nothing further will reach the medium.
    pub const LATCHED: u32 = 3;
    /// The device did not complete within the deadline.
    pub const TIMED_OUT: u32 = 4;
}

/// A request to the block driver, carried inline in one call.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskRequest {
    /// One of [`disk_op`].
    pub op: u32,
    /// A `FaultMode` to apply to this one request, or `NONE`.
    pub fault_mode: u32,
    /// Absolute device block.
    pub block: u64,
    /// Blocks to transfer.
    pub count: u32,
    /// Offset into the shared buffer, in blocks.
    pub buffer_block: u32,
    /// Argument of the fault mode, where it takes one.
    pub fault_arg: u64,
}

/// What the block driver answers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskReply {
    /// One of [`disk_status`].
    pub status: u32,
    /// The fault mode that was applied, or `NONE`.
    pub applied: u32,
    /// Blocks the device was actually asked to transfer.
    pub transferred: u64,
    /// Writes issued to the device since the driver started.
    pub writes_issued: u64,
    /// Writes the harness suppressed, in whole or in part.
    pub writes_suppressed: u64,
    /// Flushes the device acknowledged.
    pub flushes: u64,
    /// Sectors written, which is what a torn write is measured in.
    pub sectors_written: u64,
}

// SAFETY: both are `repr(C)` integer structures with no implicit padding: the
// assertions below are what keeps that true.
unsafe impl Pod for DiskRequest {}
// SAFETY: as above.
unsafe impl Pod for DiskReply {}

const _: () = assert!(core::mem::size_of::<DiskRequest>() == 32);
const _: () = assert!(core::mem::size_of::<DiskReply>() == 48);

impl DiskRequest {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

impl DiskReply {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// The role a client reads out of the page the supervisor mapped it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientConfig {
    /// One of [`role`].
    pub role: u64,
    /// The principal this client speaks as, which is also its facet.
    pub principal: u64,
    /// The scenario the run was started with.
    pub scenario: u64,
    /// Which leg of that scenario this is.
    pub leg: u64,
    /// Publications this client attempts.
    pub rounds: u64,
    /// Reserved.
    pub reserved0: u64,
}

// SAFETY: as above.
unsafe impl Pod for ClientConfig {}

const _: () = assert!(core::mem::size_of::<ClientConfig>() == 48);

impl ClientConfig {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// What the supervisor tells a state service about the run it is part of.
///
/// A replacement service is not a first one. It is told so here rather than
/// working it out from the medium, because "am I the run the directive was
/// written for?" is a question about the harness and not about the store, and
/// a service that re-applied a fault it already applied would never finish
/// recovering.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreConfig {
    /// 0 for the first service of a run, 1 for the first replacement, and so on.
    pub instance: u64,
    /// Which leg of the scenario this run is.
    pub leg: u64,
    /// The scenario the run was started with.
    pub scenario: u64,
    /// Scaffolding: 1 means read the fault directive and obey it.
    pub apply_directive: u64,
    /// Reserved.
    pub reserved0: u64,
    /// Reserved.
    pub reserved1: u64,
}

// SAFETY: as above.
unsafe impl Pod for StoreConfig {}

const _: () = assert!(core::mem::size_of::<StoreConfig>() == 48);

impl StoreConfig {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Where a state service finds the configuration the supervisor wrote.
pub const STORE_CONFIG_VADDR: u64 = 0x2000_0000;

/// Diagnostic note kinds K4 uses.
///
/// Numbered in their own range so a K4 note and a K2 or K3 note can never be
/// confused by a gate reading one log. A note is a diagnostic, not a durable
/// receipt and not a control receipt: what the gate decides the durable claims
/// from is the medium itself.
pub mod note {
    /// The driver brought the transport up. Value: the queue size.
    pub const DISK_READY: u64 = 0x4001;
    /// A write reached the device. Value: block, with the count above bit 48.
    pub const DISK_WRITE: u64 = 0x4002;
    /// A flush was acknowledged. Value: flushes so far.
    pub const DISK_FLUSH: u64 = 0x4003;
    /// A write was suppressed by the harness. Value: mode, block above bit 8.
    pub const DISK_SUPPRESSED: u64 = 0x4004;
    /// Writes are latched: nothing further reaches the medium. Value: issued.
    pub const DISK_LATCHED: u64 = 0x4005;
    /// A write was refused as a medium failure. Value: the block.
    pub const DISK_IO_ERROR: u64 = 0x4006;
    /// A write was held and issued after the next one. Value: the block.
    pub const DISK_REORDERED: u64 = 0x4007;
    /// A read reached the device. Value: block, with the count above bit 48.
    pub const DISK_READ: u64 = 0x4008;

    /// The service found no store and wrote one. Value: the epoch.
    pub const STORE_FORMATTED: u64 = 0x4010;
    /// Recovery finished. Value: the published generation it adopted.
    pub const STORE_RECOVERED: u64 = 0x4011;
    /// Recovery read a valid prefix. Value: records in it.
    pub const RECOVERY_SCANNED: u64 = 0x4012;
    /// Recovery resolved an unresolved prepare. Value: its sequence.
    pub const RECOVERY_ABORTED: u64 = 0x4013;
    /// A commit named an object the medium does not hold. Value: its sequence.
    pub const RECOVERY_DEPENDENCY_MISSING: u64 = 0x4014;
    /// A prefix that was supposed to be durable did not verify. Value: where.
    pub const RECOVERY_INTEGRITY_FAILED: u64 = 0x4015;
    /// A publication became durable. Value: the new generation.
    pub const PUBLISHED: u64 = 0x4016;
    /// A publication was refused. Value: the status.
    pub const PUBLISH_REFUSED: u64 = 0x4017;
    /// A record was written. Value: its sequence, with its kind above bit 32.
    pub const RECORD_WRITTEN: u64 = 0x4018;
    /// A checkpoint opened an arena. Value: the arena.
    pub const CHECKPOINT_WRITTEN: u64 = 0x4019;
    /// A superblock was switched. Value: its generation.
    pub const SUPERBLOCK_SWITCHED: u64 = 0x401A;
    /// Compaction copied the reachable set. Value: objects copied.
    pub const COMPACTED: u64 = 0x401B;
    /// A durable result was answered. Value: outcome, sequence above bit 8.
    pub const RESULT_ANSWERED: u64 = 0x401C;
    /// The guest rebuilt the golden vectors. Value: checked, failed above 32.
    pub const GOLDEN_VERIFIED: u64 = 0x401D;
    /// The kernel admitted the effect of a publication. Value: invocation.
    pub const EFFECT_ADMITTED: u64 = 0x401E;
    /// The kernel refused it, so no publication began. Value: the status.
    pub const EFFECT_REFUSED: u64 = 0x401F;
    /// An outbox intent was resolved durably. Value: the outbox status.
    pub const OUTBOX_RECORDED: u64 = 0x4020;
    /// The directive this run was started with. Value: point, mode above 8,
    /// leg above 16, scenario above 24.
    pub const FAULT_DIRECTIVE: u64 = 0x4021;
    /// The service reached the named point and applied the directive.
    pub const FAULT_APPLIED: u64 = 0x4022;
    /// Admission was refused to keep the reserve. Value: free blocks.
    pub const EXHAUSTED: u64 = 0x4023;
    /// The service is admitting requests. Value: the published generation.
    pub const STORE_READY: u64 = 0x4024;
    /// The first eight bytes of the published root, so a log can be matched
    /// against a medium without claiming to carry the digest itself.
    pub const ROOT_HEAD: u64 = 0x4025;
    /// A principal's durable high-water mark. Value: principal, mark above 8.
    pub const HIGH_WATER: u64 = 0x4026;
    /// The service refused to serve because integrity failed. Value: status.
    pub const INTEGRITY_REFUSED: u64 = 0x4027;
    /// The control plane lost receipts and the service says so. Value: lost.
    pub const CONTROL_INCOMPLETE: u64 = 0x4028;
    /// A request identity was reused for different inputs. Value: sequence.
    pub const CONFLICT_REFUSED: u64 = 0x4029;
    /// The service ended where the harness cut it. Value: writes issued.
    pub const SERVICE_STOPPED: u64 = 0x402A;
    /// No workspace was free to fork into. Value: the principal owning each
    /// live workspace, one nibble per slot from the low end.
    pub const WORKSPACES_EXHAUSTED: u64 = 0x402B;
    /// Staged objects nothing could still name were dropped. Value: how many.
    pub const STAGING_RECLAIMED: u64 = 0x402C;

    /// A client read what it was built to be. Value: role, principal above 8.
    pub const CLIENT_ROLE: u64 = 0x4030;
    /// A client's publication was accepted. Value: the generation.
    pub const CLIENT_PUBLISHED: u64 = 0x4031;
    /// A client's publication was refused. Value: the status.
    pub const CLIENT_REFUSED: u64 = 0x4032;
    /// A client asked what became of a request. Value: outcome, seq above 8.
    pub const CLIENT_RESULT: u64 = 0x4033;
    /// A client read a version back. Value: bytes read.
    pub const CLIENT_READ: u64 = 0x4034;
    /// An expectation about a repeated content digest was refused.
    pub const CLIENT_ABA: u64 = 0x4035;
    /// A broker answered an intent. Value: the outbox status.
    pub const BROKER_ANSWERED: u64 = 0x4036;
    /// A client offered evidence that does not name these inputs. Value: status.
    pub const VALIDATION_REFUSED: u64 = 0x4037;
    /// A client that may prepare content tried to publish. Value: the status.
    pub const PUBLISH_FORBIDDEN: u64 = 0x4038;

    /// The supervisor finished a build step. Value: the step.
    pub const SUPER_BUILT: u64 = 0x4040;
    /// The supervisor ended the run at a fault point. Value: point, mode above 8.
    pub const SUPER_CRASH: u64 = 0x4041;
    /// The supervisor started a replacement service. Value: how many so far.
    pub const SUPER_RESTARTED: u64 = 0x4042;
    /// The supervisor reached the end of its script. Value: 1.
    pub const SUPER_FINISHED: u64 = 0x4043;
    /// The auditor consumed receipts. Value: how many, in total.
    pub const AUDIT_DRAINED: u64 = 0x4044;
    /// The auditor saw an effect admission. Value: the invocation it covers.
    pub const AUDIT_EFFECT: u64 = 0x4045;
    /// The receipt sequence skipped. Value: the sequence that did not arrive.
    pub const AUDIT_GAP: u64 = 0x4046;
    /// The log says it dropped receipts. Value: how many, in total.
    pub const AUDIT_LOST: u64 = 0x4047;
    /// The deepest the log was seen, and the gaps found. Value: the high
    /// water mark, with the number of sequence gaps above bit 32.
    pub const AUDIT_HIGH_WATER: u64 = 0x4048;
}

/// What the service asks a broker to do, and the key it must be idempotent on.
///
/// The key is the request's identity: the same publication attempted twice
/// carries the same key, so a broker that has already accepted it can say so
/// rather than doing it again. The service does not trust that it did — what
/// the broker answers is recorded durably, `UNKNOWN` included, because "I do
/// not know whether that happened" is a result and not a failure to have one.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutboxIntent {
    /// Which broker-side effect this names.
    pub target: u64,
    /// The principal whose publication carries it.
    pub principal: u64,
    /// That publication's request sequence.
    pub request_sequence: u64,
    /// The generation the publication produced.
    pub generation: u64,
    /// The idempotency key: the request digest.
    pub key: [u8; 32],
}

/// What a broker answers about an intent.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutboxAnswer {
    /// One of the schema's `OutboxStatus` values.
    pub status: u32,
    /// How many times the broker has seen this key.
    pub attempts: u32,
    /// Reserved.
    pub reserved0: u64,
}

// SAFETY: as above.
unsafe impl Pod for OutboxIntent {}
// SAFETY: as above.
unsafe impl Pod for OutboxAnswer {}

const _: () = assert!(core::mem::size_of::<OutboxIntent>() == 64);
const _: () = assert!(core::mem::size_of::<OutboxAnswer>() == 16);

impl OutboxIntent {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

impl OutboxAnswer {
    /// Encoded size in bytes.
    pub const SIZE: usize = core::mem::size_of::<Self>();
}
