//! What the link domain and the supervisor agree on.
//!
//! The link is the domain that carries a consumer outside this machine -- the
//! real Thalyx, on Linux, for EXP-13 -- to the services inside it: it drives
//! the virtio-console function the kernel assigned, speaks Thalyx's managed
//! protocol on each of its ports, and expresses every request as calls on the
//! K4 state service and on a work-scope service the supervisor serves. It is
//! not authority: a slot names nothing until the supervisor installs a
//! capability there, and an address is reachable only because a mapping was
//! made.
//!
//! Everything here fits in `MAX_INLINE_PAYLOAD`; what does not travels in a
//! memory object whose capability the message carries.

use thalyx_user_k4fmt::Pod;

/// Capability slots the supervisor fills in the link domain.
pub mod link_slot {
    /// The console function, narrowed to mapping, interrupts and DMA.
    pub const DEVICE: u32 = 1;
    /// The signal its interrupt raises.
    pub const IRQ: u32 = 2;
    /// The virtqueue rings and control buffers, granted to the device.
    pub const RING: u32 = 3;
    /// The receive and transmit buffers, granted to the device.
    pub const DATA: u32 = 4;
    /// The staging buffer it lends to the state service, one call at a time.
    pub const STAGE: u32 = 5;
    /// The one facet of the K4 state service the link writes the medium
    /// through: one service, one writer.
    pub const STORE: u32 = 6;
    /// A child scope the link creates each work's scope under, held with
    /// `SCOPE_CREATE` and `SCOPE_FENCE` and nothing that reaches the medium.
    pub const WORK_PARENT: u32 = 7;
    /// Own scope: read what it was charged, derive a work's grant under a
    /// work scope's life.
    pub const SELF_SCOPE: u32 = 8;
    /// The control log, for the receipts a reader audits.
    pub const LOG: u32 = 9;
    /// The signal the link raises when it is up, and the supervisor waits on.
    pub const READY: u32 = 10;
}

/// Where the link domain finds its mappings.
pub mod link_addr {
    /// The configuration page, read-only.
    pub const CONFIG: u64 = 0x2000_0000;
    /// The staging buffer.
    pub const STAGE: u64 = 0x2010_0000;
    /// Pages of it. K4's object ceiling is under one page; two is what the
    /// K4 client lends and what the service copies from.
    pub const STAGE_PAGES: u64 = 2;
    /// The rings: one 0x300-byte slot per virtqueue, then the control buffers.
    pub const RING: u64 = 0x5000_0000;
    /// Pages of the ring object. Sixteen queues of 0x300 bytes fit in three
    /// pages; the fourth holds the control-queue buffers.
    pub const RING_PAGES: u64 = 4;
    /// The data buffers: per port, `RX_BUFFERS` receive pages then
    /// `TX_PAGES` transmit pages.
    pub const DATA: u64 = 0x5200_0000;
    /// Where each device register window lands, in structure order.
    pub const REGION: [u64; 4] = [0x4000_0000, 0x4001_0000, 0x4002_0000, 0x4003_0000];
}

/// Ports the link drives, and how a port maps to the state service.
///
/// Port zero of a multiport console is reserved for a console by the
/// specification and is not used; ports one to `MAX_PORTS` are the lines a
/// consumer connects to. Port `p` is served as principal `p` of the state
/// service, so the facet the kernel authenticated is the identity, and
/// nothing a request says about itself is.
pub const MAX_PORTS: u32 = 5;
/// Worker ports: lines a consumer connects to. The last port is the harness's
/// control line and publishes nothing.
pub const LINES: u32 = 4;
/// The port the harness drives: fences, statistics, and the end of the run.
pub const CONTROL_PORT: u32 = 5;
/// Receive pages posted per port at once.
pub const RX_BUFFERS: u64 = 4;
/// Transmit pages per port, as one contiguous buffer.
pub const TX_PAGES: u64 = 16;
/// Pages of the data object.
pub const DATA_PAGES: u64 = (MAX_PORTS as u64) * (RX_BUFFERS + TX_PAGES);
/// The most one frame in either direction may carry.
pub const FRAME_MAX: usize = 256 * 1024;

/// Magic of the link configuration page, `"K5LINK01"` little-endian.
pub const LINK_MAGIC: u64 = 0x3130_4B4E_494C_354B;

/// What the supervisor tells the link about the run.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LinkConfig {
    /// [`LINK_MAGIC`].
    pub magic: u64,
    /// Run seed, for the store identity the link reports.
    pub seed: u64,
    /// Scenario from the medium's directive, so the link can say it.
    pub scenario: u64,
    /// Record version.
    pub version: u32,
    /// Ports the device has, as the supervisor read them from its
    /// configuration; the link opens `min(ports, MAX_PORTS)`.
    pub ports: u32,
    /// Reserved, zero.
    pub reserved0: u32,
    /// The line each worker port serves, indexed by port. Zero means the port
    /// serves its own number as its line -- one store per port. The rival
    /// scenarios map two ports to one line, so that two principals contend on
    /// one generation. Index zero and the control port are unused; the last
    /// element is padding that keeps the record free of implicit padding.
    pub port_line: [u32; MAX_PORTS as usize + 2],
}

const _: () = assert!(size_of::<LinkConfig>() == 64);

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for LinkConfig {}

/// The work-scope service the supervisor serves the link.
///
/// A work is one Thalyx transaction. The link asks for a scope when the
/// consumer forks one, derives the grants it will act through under that
/// scope's life, and asks for the scope to be closed when the transaction
/// settles. Fencing is the supervisor's, on the harness's word or the
/// scenario's: once a work's scope is fenced, every grant derived under it is
/// refused by the kernel, and that refusal is what the consumer is told.
pub mod works_op {
    /// Open a work: create its scope. Replies with the scope handle.
    pub const OPEN: u32 = 1;
    /// Fence a work's scope now.
    pub const FENCE: u32 = 2;
    /// Close a work: fence, drain and retire its scope. Replies with what it
    /// was charged.
    pub const CLOSE: u32 = 3;
    /// What a work's scope has been charged, and whether it is open.
    pub const QUERY: u32 = 4;
    /// The consumer asked for the run to end; the supervisor finishes.
    pub const SHUTDOWN: u32 = 5;
}

/// Statuses of the work-scope service.
pub mod works_status {
    pub const OK: u32 = 0;
    pub const INVALID: u32 = 1;
    pub const NO_SUCH_WORK: u32 = 2;
    pub const EXHAUSTED: u32 = 3;
    pub const REFUSED: u32 = 4;
}

/// Works the supervisor keeps at once.
pub const MAX_WORKS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorksRequest {
    /// [`works_op`].
    pub op: u32,
    /// Which line asked.
    pub line: u32,
    /// The work, as the supervisor numbered it at `OPEN`; zero at `OPEN`.
    pub work: u64,
    /// Reserved, zero.
    pub reserved0: u64,
    /// The scope label at `OPEN`: the transaction, truncated.
    pub name: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorksReply {
    /// [`works_status`].
    pub status: u32,
    /// Scope state as `ScopeInfo` reports it, at `QUERY` and `CLOSE`.
    pub state: u32,
    /// The work's number.
    pub work: u64,
    /// Nanoseconds charged to the scope.
    pub cpu_ns: u64,
    /// Pages held at most.
    pub pages_peak: u64,
    /// Metadata objects held at most.
    pub metadata_peak: u64,
    /// The kernel clock when the request was served.
    pub served_ns: u64,
}

impl WorksRequest {
    pub const SIZE: usize = size_of::<Self>();

    pub const fn zeroed() -> Self {
        Self {
            op: 0,
            line: 0,
            work: 0,
            reserved0: 0,
            name: [0; 16],
        }
    }
}

impl WorksReply {
    pub const SIZE: usize = size_of::<Self>();

    pub const fn zeroed() -> Self {
        Self {
            status: 0,
            state: 0,
            work: 0,
            cpu_ns: 0,
            pages_peak: 0,
            metadata_peak: 0,
            served_ns: 0,
        }
    }
}

const _: () = assert!(size_of::<WorksRequest>() == 40);
const _: () = assert!(size_of::<WorksReply>() == 48);

// SAFETY: `repr(C)`, integers only, no padding, every bit pattern valid.
unsafe impl Pod for WorksRequest {}
// SAFETY: as above.
unsafe impl Pod for WorksReply {}

/// Notes the link emits. Their own range beside the supervisor's.
pub mod note {
    /// The console transport is up. Value: ports the device reported.
    pub const LINK_READY: u64 = 0x5100;
    /// A port was opened to the host. Value: the port.
    pub const PORT_OPEN: u64 = 0x5101;
    /// A frame arrived. Value: port | length << 8.
    pub const FRAME_IN: u64 = 0x5102;
    /// A frame was sent. Value: port | length << 8.
    pub const FRAME_OUT: u64 = 0x5103;
    /// The state service refused. Value: status | op << 8.
    pub const STORE_REFUSED: u64 = 0x5104;
    /// A managed publication committed. Value: the generation.
    pub const PUBLISHED: u64 = 0x5105;
    /// A managed request was refused with a word. Value: the request kind.
    pub const REFUSED: u64 = 0x5106;
    /// A work's grants were found closed by the kernel. Value: the work.
    pub const WORK_CLOSED: u64 = 0x5107;
    /// The link rebuilt its index from the published root. Value: objects.
    pub const INDEX_REBUILT: u64 = 0x5108;
    /// Something the link did not expect. Value: a code.
    pub const LINK_UNEXPECTED: u64 = 0x5109;
    /// A work scope was opened for a transaction. Value: the work.
    pub const WORK_OPENED: u64 = 0x510A;
    /// The kernel clock when a fence was observed by the link, in a
    /// refusal. Value: nanoseconds.
    pub const FENCE_SEEN_NS: u64 = 0x510B;
    /// The host asked for the run to end.
    pub const SHUTDOWN: u64 = 0x510C;
    /// Frames served in total. Value: the count.
    pub const FRAMES: u64 = 0x510D;
    /// A control-line request. Value: its kind.
    pub const CONTROL: u64 = 0x510E;
}
