//! What the K3 package's programs agree on.
//!
//! Three programs have to share a few numbers: where the supervisor maps things
//! in a worker or a driver, which capability slot holds what, and what a
//! worker's role is. They are here rather than repeated in each program because
//! a layout two programs disagree about is a bug neither of them can see.
//!
//! None of this is authority. A worker that knows the address its shared page
//! *would* be mapped at still cannot reach it unless the supervisor mapped it,
//! and a slot number names nothing until a capability is installed there.

/// Where a worker finds its own configuration, read-only.
pub const CONFIG_VADDR: u64 = 0x2000_0000;
/// Where a worker finds the page every worker shares, writable.
pub const SHARED_VADDR: u64 = 0x2010_0000;
/// Where a worker finds the page the supervisor is going to take away.
pub const PROBE_VADDR: u64 = 0x2020_0000;
/// Where a worker's second thread finds its stack.
pub const SECOND_STACK_VADDR: u64 = 0x2030_0000;
/// Pages of that stack.
pub const SECOND_STACK_PAGES: u64 = 4;

/// Where the driver finds each device register window.
pub const REGION_VADDR: [u64; 4] = [0x4000_0000, 0x4001_0000, 0x4002_0000, 0x4003_0000];
/// Where the driver finds the memory it shares with the device.
pub const RING_VADDR: u64 = 0x5000_0000;
/// Pages of that memory. Three are granted to the device; the fourth is the
/// driver's own scratch, and is deliberately not.
pub const RING_PAGES: u64 = 4;
/// Pages of that memory the device may reach.
pub const RING_GRANTED_PAGES: u64 = 3;

/// Capability slots the supervisor fills in a worker.
pub mod worker_slot {
    /// The endpoint facet a calling worker uses.
    pub const ENDPOINT: u32 = 1;
    /// The signal a worker waits on to be told to stop.
    pub const STOP: u32 = 2;
    /// The worker's own scope, for reading its own accounting.
    pub const SCOPE: u32 = 3;
}

/// Capability slots the supervisor fills in the driver.
pub mod driver_slot {
    /// The device, narrowed to mapping, interrupts and DMA.
    pub const DEVICE: u32 = 1;
    /// The signal the device's interrupt raises.
    pub const IRQ: u32 = 2;
    /// The memory the driver shares with the device.
    pub const RING: u32 = 3;
    /// The signal the driver raises when it has finished with the device.
    pub const DONE: u32 = 4;
    /// The control log, for the saturation case.
    pub const LOG: u32 = 5;
}

/// What a worker was built to do.
pub mod role {
    /// Burn processor time under a shared budget.
    pub const SPIN: u64 = 1;
    /// Read a page until the supervisor takes it away.
    pub const PROBE: u64 = 2;
    /// Write a page until the supervisor seals it.
    pub const WRITER: u64 = 3;
    /// Call an endpoint until its origin is fenced.
    pub const CALLER: u64 = 4;
}

/// Signal bits the K3 package uses.
pub mod bit {
    /// The device raised its interrupt.
    pub const DEVICE: u64 = 1 << 0;
    /// A worker should stop what it is doing.
    pub const STOP: u64 = 1 << 1;
    /// The driver should try what its old session no longer permits.
    pub const STALE: u64 = 1 << 2;
    /// The driver has finished with the device.
    pub const DRIVER_DONE: u64 = 1 << 3;
}

/// The configuration a worker reads from its own page.
///
/// Written by the supervisor into a memory object before the worker exists, and
/// mapped read-only, so a worker cannot change what it was built to be.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerConfig {
    /// One of [`role`].
    pub role: u64,
    /// Which instance of that role this is.
    pub instance: u64,
    /// Intervals of work to perform.
    pub rounds: u64,
    /// Iterations of integer work in one interval.
    pub burn: u64,
    /// Slot of the shared page this worker writes.
    pub slot: u64,
    /// Reserved.
    pub reserved0: u64,
}

const _: () = assert!(core::mem::size_of::<WorkerConfig>() == 48);
