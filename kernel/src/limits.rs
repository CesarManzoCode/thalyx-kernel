//! Fixed capacities of every kernel table.
//!
//! K2 still has no kernel heap. Every object lives in a fixed-capacity table,
//! and refusing to create one when its table is full is exactly the "reject
//! before a partial object becomes visible" rule of the resource contract. A
//! fixed table makes that refusal path impossible to skip, and it makes the
//! metadata a scope is charged for a real, countable quantity rather than an
//! estimate.
//!
//! These numbers are implementation capacities, not interface limits. The
//! interface limits — descriptor size, inline payload, capabilities per
//! message, scope depth, derivation depth, handles per domain — come from
//! `abi/schema/v0.json` and are reported by the limits query.

use thalyx_abi::limit;

/// Processors the kernel can hold at once, the bootstrap processor included.
///
/// A capacity, not a count: the number that actually run is what the firmware
/// describes and the handshake confirms, and a machine with more processors
/// than this leaves the extras offline and says so rather than silently
/// scheduling on a table it does not have.
pub const MAX_CPUS: usize = 8;
/// Domains the kernel can hold at once.
pub const MAX_DOMAINS: usize = 10;
/// Threads the kernel can hold at once, one idle thread per processor
/// included.
pub const MAX_THREADS: usize = 8 + 24;
/// Threads one domain may hold.
pub const MAX_THREADS_PER_DOMAIN: usize = 4;
/// Capability slots in one domain's table.
pub const MAX_CAPS: usize = limit::MAX_HANDLES_PER_DOMAIN as usize;
/// Scopes the kernel can hold at once.
pub const MAX_SCOPES: usize = 16;
/// Grant nodes the kernel can hold at once, tombstones included.
pub const MAX_GRANTS: usize = 128;
/// Memory objects the kernel can hold at once.
pub const MAX_MEMORY_OBJECTS: usize = 24;
/// Pages one memory object may hold.
pub const MAX_OBJECT_PAGES: u64 = limit::MAX_MEMORY_PAGES_PER_OBJECT;
/// Mapping records, which are also the reverse index a seal walks.
pub const MAX_MAPS: usize = 64;
/// Endpoints the kernel can hold at once.
pub const MAX_ENDPOINTS: usize = 12;
/// Messages in flight across every endpoint.
pub const MAX_MESSAGES: usize = 32;
/// Invocations the kernel can hold at once.
pub const MAX_INVOCATIONS: usize = 32;
/// Signals the kernel can hold at once.
pub const MAX_SIGNALS: usize = 12;
/// Timers the kernel can hold at once.
pub const MAX_TIMERS: usize = 12;
/// Control logs the kernel can hold at once.
pub const MAX_CONTROL_LOGS: usize = 2;
/// Receipts one control log retains.
pub const CONTROL_LOG_CAPACITY: usize = limit::CONTROL_LOG_CAPACITY as usize;
/// Cells of a control log reserved for closing authority, which ordinary
/// traffic can never occupy. A full log must never stop a revocation.
pub const CONTROL_LOG_RESERVED: usize = limit::CONTROL_LOG_RESERVED as usize;
/// Scheduling window over which a scope's CPU budget is measured.
pub const CPU_WINDOW_NS: u64 = limit::CPU_WINDOW_NS;

const _: () = assert!(CONTROL_LOG_RESERVED < CONTROL_LOG_CAPACITY);
/// Device functions the kernel can assign at once.
pub const MAX_DEVICES: usize = 4;
/// Memory regions of one device a driver may be given.
pub const MAX_DEVICE_REGIONS: usize = 4;
/// Distinct DMA grants the kernel can hold at once.
pub const MAX_DMA_GRANTS: usize = 16;
/// Interrupt bindings the kernel can hold at once.
pub const MAX_IRQ_BINDINGS: usize = 8;

const _: () = assert!(MAX_THREADS_PER_DOMAIN * MAX_DOMAINS >= MAX_THREADS - MAX_CPUS);
const _: () = assert!(MAX_CPUS <= 64);
