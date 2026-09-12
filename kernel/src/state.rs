//! Kernel object tables and the lock that protects the control plane.
//!
//! Every kernel object lives in a fixed-capacity table. That is not a shortcut
//! around the resource contract: refusing to create an object when its table is
//! full is exactly the "reject before a partial object becomes visible" rule,
//! and a fixed table makes the refusal path impossible to skip. What K2 adds on
//! top is the accounting the contract requires -- every object is charged to a
//! scope, and the charge is checked against that scope and all of its ancestors
//! before the object exists.
//!
//! Until this revision there was one lock, and it was what K6 measured as the
//! limit of the machine: four independent IPC pairs serialised on it. What
//! remains under it is the **control plane**: creation and destruction of
//! domains, memory objects, mappings, devices and timers; fences and
//! retirements; anything that rewrites the *structure* of a table rather than
//! moving a counter or a message. The hot paths -- scheduling, the thread
//! table, scope accounting -- live in their own modules with their own rules
//! (`crate::thread`, `crate::sched`, `crate::scope`), and the objects of the
//! IPC path follow the same split. The lock order, outermost first, is the
//! control lock, then one object lock, then one domain's capability table,
//! then a thread's wait lock, then a run queue; `vault/architecture/
//! concurrency.md` records it.
//!
//! The control lock is still what makes a barrier linearisable against the
//! admissions that race it: a fence changes a monotonic state under this lock
//! and then sweeps the objects under theirs, and an admission re-reads that
//! state after publishing under the object's lock, so it either loses the
//! race and withdraws itself or wins it and is found by the sweep.

use thalyx_abi::generated::ReceiptRecord;

use crate::arch::x86_64::paging::AddressSpace;
use crate::ctrl::ControlLog;
use crate::device::{Device, DmaGrant, IrqBinding, MAX_DEVICE_MAPS};
use crate::events::{Signal, Timer};
use crate::ipc::{Endpoint, Invocation, Message};
use crate::limits::{
    MAX_CONTROL_LOGS, MAX_DEVICES, MAX_DMA_GRANTS, MAX_ENDPOINTS, MAX_GRANTS, MAX_INVOCATIONS,
    MAX_IRQ_BINDINGS, MAX_MAPS, MAX_MEMORY_OBJECTS, MAX_MESSAGES, MAX_SIGNALS, MAX_TIMERS,
};
use crate::memobj::{MapRecord, MemoryObject};
use crate::mm::frame::FrameAllocator;
use crate::obj::{CapTable, Grant, GrantId, NO_GRANT, ObjRef, ScopeId};
use crate::sync::SpinLock;

/// Re-exported so the modules that predate the table split keep one name for
/// each capacity.
pub use crate::limits::{MAX_DOMAINS, MAX_THREADS, MAX_THREADS_PER_DOMAIN};
/// Re-exported: the thread table moved to its own module, and these names are
/// the ones the rest of the kernel uses.
pub use crate::thread::{ThreadKind, ThreadState, Wait, idle_thread};

/// Kernel stack slots, one per thread.
pub const MAX_KSTACKS: usize = MAX_THREADS;
/// Table index of the bootstrap processor's idle thread, which is the
/// bootstrap context itself.
pub const IDLE_THREAD: usize = 0;

/// Lifecycle of a domain.
///
/// `Building` is the only state in which mappings, capabilities and threads may
/// be installed, and `Runnable` is reached exactly once, through the activation
/// gate. A domain that never reaches it never executes an instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomainState {
    /// Free table slot.
    Empty,
    /// Under construction: address space exists, requirements not yet met.
    Building,
    /// Activated and eligible to run.
    Runnable,
    /// Stopped by a fault of its own code; resources not yet reclaimed.
    Faulted,
    /// Stopped at its own request or by authority; resources not yet reclaimed.
    Stopping,
    /// Stopped and reclaimed.
    Dead,
}

impl DomainState {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            DomainState::Empty => "empty",
            DomainState::Building => "building",
            DomainState::Runnable => "runnable",
            DomainState::Faulted => "faulted",
            DomainState::Stopping => "stopping",
            DomainState::Dead => "dead",
        }
    }

    /// Interface value reported in a domain descriptor.
    #[must_use]
    pub const fn abi(self) -> u32 {
        use thalyx_abi::generated::domain_state;
        match self {
            DomainState::Empty => 0,
            DomainState::Building => domain_state::BUILDING,
            DomainState::Runnable => domain_state::RUNNABLE,
            DomainState::Faulted => domain_state::FAULTED,
            DomainState::Stopping => domain_state::STOPPING,
            DomainState::Dead => domain_state::DEAD,
        }
    }
}

/// What stopped a domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitReason {
    /// The domain executed an access its authority does not cover.
    UserFault,
    /// The domain asked to stop.
    Voluntary,
    /// An authority holding a capability over it stopped it.
    Terminated,
}

impl ExitReason {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ExitReason::UserFault => "user_fault",
            ExitReason::Voluntary => "voluntary",
            ExitReason::Terminated => "terminated",
        }
    }
}

/// Machine state captured at a user fault.
#[derive(Clone, Copy, Debug, Default)]
pub struct FaultRecord {
    /// Interrupt vector.
    pub vector: u64,
    /// CPU-pushed error code.
    pub error_code: u64,
    /// Faulting linear address, meaningful for #PF only.
    pub cr2: u64,
    /// Instruction that faulted.
    pub rip: u64,
    /// Stack pointer at the fault.
    pub rsp: u64,
    /// Code selector at the fault. Its privilege level is the classification.
    pub cs: u64,
}

/// A protection domain: one address space, one capability table.
pub struct Domain {
    /// Lifecycle state.
    pub state: DomainState,
    /// Diagnostic identity, unique within the boot epoch.
    pub id: u64,
    /// Generation of this table slot, so a stale reference is refused.
    pub generation: u32,
    /// Name from the boot package or the creator. Diagnostics only, never
    /// authority.
    pub name: [u8; 32],
    /// Bytes of `name` that are significant.
    pub name_len: usize,
    /// Scope that pays for this domain's infrastructure.
    pub owner_scope: ScopeId,
    /// The domain's address space, present from `Building` until reclamation.
    pub space: Option<AddressSpace>,
    /// The domain's threads.
    pub threads: [Option<usize>; MAX_THREADS_PER_DOMAIN],
    /// The domain's capability table. Empty unless someone installed something.
    pub caps: CapTable,
    /// Endpoint a fault of this domain is reported on.
    pub fault_endpoint: Option<ObjRef>,
    /// Grant the fault message is admitted under.
    pub fault_grant: GrantId,
    /// Facet stamped on the fault message.
    pub fault_facet: u64,
    /// Whether a queue cell is reserved for that report.
    pub fault_reserved: bool,
    /// Entry point installed at creation.
    pub entry: u64,
    /// Loadable segments mapped.
    pub segments: usize,
    /// Pages the image's segments occupy, from the validated program headers.
    pub image_pages: u64,
    /// Whether the initial user stack is mapped.
    pub stack_mapped: bool,
    /// Whether the domain was built through the K2 capability path.
    pub managed: bool,
    /// Pages reserved in the owner scope for this domain's infrastructure.
    pub reserved_pages: u64,
    /// Metadata units reserved in the owner scope for this domain and its
    /// threads. Capability slots are counted separately, as they are installed.
    pub reserved_metadata: u64,
    /// Fault that stopped the domain, if any.
    pub fault: Option<FaultRecord>,
    /// Faults observed.
    pub faults: u32,
    /// Why the domain stopped, if it has.
    pub exit_reason: Option<ExitReason>,
    /// Exit code the domain reported, when it stopped voluntarily.
    pub exit_code: u64,
    /// Diagnostic notes the domain emitted through K1 scaffolding.
    pub notes: u64,
    /// Kernel entries this domain performed.
    pub invocations: u64,
    /// Kernel entries this domain performed that were refused.
    pub refusals: u64,
    /// Refusal records already emitted before the plane starts coalescing.
    pub refusal_records: u32,
    /// Capability entries naming this domain, wherever they are held.
    pub refs: u32,
}

impl Domain {
    /// A free table slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: DomainState::Empty,
            id: 0,
            generation: 0,
            name: [0; 32],
            name_len: 0,
            owner_scope: 0,
            space: None,
            threads: [None; MAX_THREADS_PER_DOMAIN],
            caps: CapTable::new(),
            fault_endpoint: None,
            fault_grant: NO_GRANT,
            fault_facet: 0,
            fault_reserved: false,
            entry: 0,
            segments: 0,
            image_pages: 0,
            stack_mapped: false,
            managed: false,
            reserved_pages: 0,
            reserved_metadata: 0,
            fault: None,
            faults: 0,
            exit_reason: None,
            exit_code: 0,
            notes: 0,
            invocations: 0,
            refusals: 0,
            refusal_records: 0,
            refs: 0,
        }
    }

    /// The domain's name as text.
    #[must_use]
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
    }

    /// The domain's first thread, if it has one.
    #[must_use]
    pub fn first_thread(&self) -> Option<usize> {
        self.threads.iter().copied().flatten().next()
    }

    /// Number of threads the domain holds.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.threads.iter().flatten().count()
    }
}

/// Everything the kernel arbitrates.
pub struct Machine {
    /// Physical frame allocator, present after memory bootstrap.
    pub memory: Option<FrameAllocator>,
    /// The kernel's own address space, whose upper half every domain shares.
    pub kernel_space: Option<AddressSpace>,
    /// Domain table.
    pub domains: [Domain; MAX_DOMAINS],
    /// Authority tree.
    pub grants: [Grant; MAX_GRANTS],
    /// Where the search for a free grant node starts. Every node below it is
    /// in use, or was the last time the search passed; a release below it
    /// moves it back. The search is still the whole table when it has to be,
    /// so nothing is refused that a full scan would have found -- what the
    /// hint removes is reading a hundred lines of nodes known to be taken to
    /// find the one after them, on every derivation.
    pub grant_hint: usize,
    /// Memory objects.
    pub memories: [MemoryObject; MAX_MEMORY_OBJECTS],
    /// Reverse index of installed mappings.
    pub maps: [MapRecord; MAX_MAPS],
    /// Endpoints.
    pub endpoints: [Endpoint; MAX_ENDPOINTS],
    /// Messages in flight.
    pub messages: [Message; MAX_MESSAGES],
    /// Admitted work.
    pub invocations: [Invocation; MAX_INVOCATIONS],
    /// Signals.
    pub signals: [Signal; MAX_SIGNALS],
    /// Timers.
    pub timers: [Timer; MAX_TIMERS],
    /// Control-receipt rings.
    pub logs: [ControlLog; MAX_CONTROL_LOGS],
    /// Assigned device functions.
    pub devices: [Device; MAX_DEVICES],
    /// Mappings of device register windows.
    pub device_maps: [crate::device::MapRecord; MAX_DEVICE_MAPS],
    /// Pages devices may reach.
    pub dma_grants: [DmaGrant; MAX_DMA_GRANTS],
    /// Device interrupts routed to signals.
    pub irqs: [IrqBinding; MAX_IRQ_BINDINGS],
    /// The configuration window, once it is mapped.
    pub ecam: Option<crate::pci::Ecam>,
    /// Whether firmware described a remapping unit at all.
    pub iommu_described: bool,
    /// Whether this kernel has translation enabled for assigned devices. It
    /// does not, and the profile is refused rather than approximated.
    pub iommu_translating: bool,
    /// Occupancy of the kernel stack slots.
    pub kstack_used: [bool; MAX_KSTACKS],
    /// Frames reclaimed from loader and module memory after bootstrap.
    pub reclaimed_frames: usize,
    /// User faults contained.
    pub user_faults: u64,
    /// Modules the loader offered that validation refused.
    pub modules_rejected: u32,
    /// Next diagnostic object identity. Never wraps: exhaustion refuses
    /// creation instead.
    pub next_object_id: u64,
    /// Boot epoch from the loader. Not an identifier and not entropy.
    pub boot_epoch: u64,
    /// Whether the boot package selected the K2 supervisor path.
    pub managed_boot: bool,
    /// Root scope of the resource tree, once it exists.
    pub root_scope: Option<ScopeId>,
    /// The system control log, once it exists.
    pub system_log: Option<u16>,
    /// The first supervisor, once it exists. It has no supervisor of its own,
    /// so its fault ends the run rather than being reported to anyone.
    pub supervisor: Option<usize>,
}

impl Machine {
    const fn new() -> Self {
        Self {
            memory: None,
            kernel_space: None,
            domains: [const { Domain::empty() }; MAX_DOMAINS],
            grants: [Grant::empty(); MAX_GRANTS],
            grant_hint: 0,
            memories: [const { MemoryObject::empty() }; MAX_MEMORY_OBJECTS],
            maps: [MapRecord::empty(); MAX_MAPS],
            endpoints: [const { Endpoint::empty() }; MAX_ENDPOINTS],
            messages: [const { Message::empty() }; MAX_MESSAGES],
            invocations: [Invocation::empty(); MAX_INVOCATIONS],
            signals: [Signal::empty(); MAX_SIGNALS],
            timers: [Timer::empty(); MAX_TIMERS],
            logs: [const { ControlLog::empty() }; MAX_CONTROL_LOGS],
            devices: [const { Device::empty() }; MAX_DEVICES],
            device_maps: [crate::device::MapRecord::empty(); MAX_DEVICE_MAPS],
            dma_grants: [DmaGrant::empty(); MAX_DMA_GRANTS],
            irqs: [IrqBinding::empty(); MAX_IRQ_BINDINGS],
            ecam: None,
            iommu_described: false,
            iommu_translating: false,
            kstack_used: [false; MAX_KSTACKS],
            reclaimed_frames: 0,
            user_faults: 0,
            modules_rejected: 0,
            next_object_id: 1,
            boot_epoch: 0,
            managed_boot: false,
            root_scope: None,
            system_log: None,
            supervisor: None,
        }
    }

    /// Frame allocator, which exists from the memory bootstrap onward.
    pub fn allocator(&mut self) -> &mut FrameAllocator {
        self.memory
            .as_mut()
            .expect("frame allocator established during bootstrap")
    }

    /// Next diagnostic identity, or `None` once the space is exhausted.
    ///
    /// Exhaustion refuses creation. A silent wrap would make two objects share
    /// an identity that evidence is expected to distinguish.
    pub fn next_id(&mut self) -> Option<u64> {
        if self.next_object_id == u64::MAX {
            return None;
        }
        let id = self.next_object_id;
        self.next_object_id += 1;
        Some(id)
    }
}

/// A control receipt as the interface reports it.
pub type Receipt = ReceiptRecord;

/// The control lock: what protects the tables above.
pub static MACHINE: SpinLock<Machine> =
    SpinLock::of_class(Machine::new(), crate::sync::LockClass::Control);
