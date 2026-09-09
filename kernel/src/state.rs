//! Kernel object tables and the single lock that protects them.
//!
//! Every kernel object lives in a fixed-capacity table. That is not a shortcut
//! around the resource contract: refusing to create an object when its table is
//! full is exactly the "reject before a partial object becomes visible" rule,
//! and a fixed table makes the refusal path impossible to skip. What K2 adds on
//! top is the accounting the contract requires — every object is charged to a
//! scope, and the charge is checked against that scope and all of its ancestors
//! before the object exists.
//!
//! There is one lock. The concurrency contract allows a single short global lock
//! for metadata before SMP, and one lock cannot be acquired out of order. Its
//! two rules are absolute: no context switch happens while it is held, and it is
//! never taken from a context that can already hold it, which on a uniprocessor
//! machine with every kernel path running at IF=0 means it is never contended.
//!
//! The lock is also what makes the admission point of the authority contract
//! real. Validating a grant, checking the clock, reserving the obligations and
//! publishing the admission all happen inside one critical section, and a fence
//! takes the same lock, so an admission either wins the race and is recorded
//! before the barrier or loses it and fails.

use thalyx_abi::generated::ReceiptRecord;

use crate::arch::x86_64::fpu::FpuState;
use crate::arch::x86_64::paging::AddressSpace;
use crate::ctrl::ControlLog;
use crate::device::{Device, DmaGrant, IrqBinding, MAX_DEVICE_MAPS};
use crate::events::{Signal, Timer};
use crate::ipc::{Endpoint, Invocation, Message};
use crate::limits::{
    MAX_CONTROL_LOGS, MAX_CPUS, MAX_DEVICES, MAX_DMA_GRANTS, MAX_ENDPOINTS, MAX_GRANTS,
    MAX_INVOCATIONS, MAX_IRQ_BINDINGS, MAX_MAPS, MAX_MEMORY_OBJECTS, MAX_MESSAGES, MAX_SCOPES,
    MAX_SIGNALS, MAX_TIMERS,
};
use crate::memobj::{MapRecord, MemoryObject};
use crate::mm::frame::FrameAllocator;
use crate::obj::{CapTable, Grant, GrantId, NO_GRANT, ObjRef, ScopeId};
use crate::scope::Scope;
use crate::sync::SpinLock;

/// Re-exported so the modules that predate the table split keep one name for
/// each capacity.
pub use crate::limits::{MAX_DOMAINS, MAX_THREADS, MAX_THREADS_PER_DOMAIN};

/// Kernel stack slots, one per thread.
pub const MAX_KSTACKS: usize = MAX_THREADS;
/// Table index of the bootstrap processor's idle thread, which is the
/// bootstrap context itself.
pub const IDLE_THREAD: usize = 0;

/// Thread-table index of the idle thread of processor `cpu`.
///
/// The first [`MAX_CPUS`] slots are reserved for them. A processor's idle
/// thread is the context it runs when nothing else is eligible **on that
/// processor**, so there is one per processor and no domain owns any of them.
#[must_use]
pub const fn idle_thread(cpu: usize) -> usize {
    cpu
}

/// What one processor is doing, as the scheduler sees it.
///
/// This is the half of per-processor state that is kernel metadata: it is read
/// and written under the machine lock like every other object. The half the
/// entry paths need before they can take a lock lives in [`crate::percpu`].
#[derive(Clone, Copy, Debug)]
pub struct CpuSlot {
    /// Whether the processor completed its handshake and may be scheduled on.
    pub online: bool,
    /// Local APIC identifier, as the firmware reported it.
    pub apic_id: u32,
    /// The processor's own idle thread.
    pub idle_thread: usize,
    /// Thread currently dispatched here.
    pub current: usize,
    /// Round-robin cursor over the thread table.
    pub cursor: usize,
    /// Thread this processor switched away from and has not yet published as
    /// ready again.
    ///
    /// A thread whose stopped context is still being written must not be
    /// picked by another processor: it would resume from a stack pointer that
    /// has not been stored yet. So the outgoing thread stays unpickable until
    /// the incoming context on this processor publishes it.
    pub previous: usize,
    /// Timer interrupts this processor took.
    pub ticks: u64,
    /// Times this processor dispatched a user thread.
    pub dispatches: u64,
    /// Times this processor took a user thread away involuntarily.
    pub preemptions: u64,
    /// Dispatches this processor refused for want of budget or a parallelism
    /// slot.
    pub budget_stalls: u64,
    /// Nanoseconds of user execution this processor charged.
    pub user_ns: u64,
}

impl CpuSlot {
    const fn empty() -> Self {
        Self {
            online: false,
            apic_id: u32::MAX,
            idle_thread: usize::MAX,
            current: usize::MAX,
            cursor: 0,
            previous: usize::MAX,
            ticks: 0,
            dispatches: 0,
            preemptions: 0,
            budget_stalls: 0,
            user_ns: 0,
        }
    }
}

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

/// Lifecycle of a thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadState {
    /// Free table slot.
    Empty,
    /// Eligible to be dispatched.
    Ready,
    /// Currently on the CPU.
    Running,
    /// Waiting inside a kernel entry for a reply, a message or a signal.
    Blocked,
    /// Stopped; its stack may still be in use until the switch away completes.
    Dead,
}

/// What a thread is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadKind {
    /// The bootstrap context, which becomes the idle thread. Never enters ring
    /// 3 and belongs to no domain.
    Idle,
    /// A user thread of a domain.
    User,
}

/// What a blocked thread is waiting for.
///
/// Every wait names the object it depends on **and** that object's generation,
/// so a completion that arrives for a recycled slot wakes nobody.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wait {
    /// Not waiting.
    None,
    /// Waiting for the reply of an invocation.
    Reply(u16, u32),
    /// Waiting for a message on an endpoint.
    Receive(u16, u32),
    /// Waiting for any of a mask of signal bits.
    Signal(u16, u32, u64),
}

/// An execution context.
pub struct Thread {
    /// Lifecycle state.
    pub state: ThreadState,
    /// What the thread is for.
    pub kind: ThreadKind,
    /// Owning domain index; meaningless for the idle thread.
    pub domain: usize,
    /// Generation of the owning domain when the thread was created.
    pub domain_generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Scope that pays for the thread's own domain.
    pub owner_scope: ScopeId,
    /// Scope the thread's current work is charged to. Equal to `owner_scope`
    /// except while the thread is bound to another scope's invocation.
    pub effective_scope: ScopeId,
    /// Whether the current work is charged to the closure reserve.
    pub recovery: bool,
    /// Invocation whose origin the thread adopted, if any.
    pub bound_invocation: Option<(u16, u32)>,
    /// Scope currently holding a parallelism slot for this thread.
    pub parallelism_scope: Option<ScopeId>,
    /// What the thread is waiting for.
    pub wait: Wait,
    /// Monotonic deadline of that wait, or zero for none.
    pub wait_deadline_ns: u64,
    /// Status a woken thread returns from its kernel entry.
    pub wake_status: i64,
    /// Auxiliary value a woken thread returns.
    pub wake_aux: u64,
    /// Kernel stack slot index.
    pub kstack_slot: usize,
    /// Top of the thread's kernel stack.
    pub kstack_top: u64,
    /// Stack pointer of the thread's stopped context.
    pub saved_rsp: u64,
    /// CR3 value for the thread's address space.
    pub cr3: u64,
    /// Eagerly saved x87/SSE state.
    pub fpu: FpuState,
    /// Nanoseconds of CPU charged to the thread.
    pub cpu_ns: u64,
    /// Monotonic time at which the thread was last dispatched.
    pub dispatched_ns: u64,
    /// Timer ticks left in the current quantum.
    pub quantum_ticks: u32,
    /// Execution reserved in the effective scope for the current dispatch.
    pub dispatch_reserved_ns: u64,
    /// Window that reservation belongs to.
    pub dispatch_window: u64,
    /// Scope the reservation was taken in. Held separately from
    /// `effective_scope` because a thread may be rebound while it runs, and the
    /// reservation must be returned where it was taken.
    pub dispatch_scope: ScopeId,
    /// Whether the reservation came from the closure reserve.
    pub dispatch_recovery: bool,
    /// Whether the thread currently holds a dispatch reservation.
    pub dispatched: bool,
    /// Processor the thread last ran on.
    pub last_cpu: usize,
    /// Times the thread was dispatched on a processor other than the last one.
    pub migrations: u64,
    /// Times the timer took the CPU away from the thread.
    pub preemptions: u64,
    /// Detailed preemption records already emitted for this thread.
    pub preempt_records: u32,
    /// Kernel entries the thread performed through `syscall`.
    pub syscalls: u64,
    /// Whether a frame from this thread has been observed at privilege level 3.
    pub ring3_confirmed: bool,
}

impl Thread {
    const fn empty() -> Self {
        Self {
            state: ThreadState::Empty,
            kind: ThreadKind::Idle,
            domain: usize::MAX,
            domain_generation: 0,
            id: 0,
            owner_scope: 0,
            effective_scope: 0,
            recovery: false,
            bound_invocation: None,
            parallelism_scope: None,
            wait: Wait::None,
            wait_deadline_ns: 0,
            wake_status: 0,
            wake_aux: 0,
            kstack_slot: usize::MAX,
            kstack_top: 0,
            saved_rsp: 0,
            cr3: 0,
            fpu: FpuState::zeroed(),
            cpu_ns: 0,
            dispatched_ns: 0,
            quantum_ticks: 0,
            dispatch_reserved_ns: 0,
            dispatch_window: 0,
            dispatch_scope: 0,
            dispatch_recovery: false,
            dispatched: false,
            last_cpu: usize::MAX,
            migrations: 0,
            preemptions: 0,
            preempt_records: 0,
            syscalls: 0,
            ring3_confirmed: false,
        }
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
    /// Thread table.
    pub threads: [Thread; MAX_THREADS],
    /// Resource tree.
    pub scopes: [Scope; MAX_SCOPES],
    /// Authority tree.
    pub grants: [Grant; MAX_GRANTS],
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
    /// Processors, dense-indexed. Slot zero is the bootstrap processor.
    pub cpus: [CpuSlot; MAX_CPUS],
    /// Processors that completed their handshake, the bootstrap processor
    /// included.
    pub cpus_online: usize,
    /// Timer ticks since the timer was armed, summed over every processor.
    pub ticks: u64,
    /// Involuntary switches away from a user thread.
    pub preemptions: u64,
    /// Dispatches refused because a scope had no budget or no parallelism slot.
    pub budget_stalls: u64,
    /// Frames reclaimed from loader and module memory after bootstrap.
    pub reclaimed_frames: usize,
    /// User faults contained.
    pub user_faults: u64,
    /// Modules the loader offered that validation refused.
    pub modules_rejected: u32,
    /// Detailed preemption records already emitted, before coalescing.
    pub preempt_records: u32,
    /// Next diagnostic object identity. Never wraps: exhaustion refuses
    /// creation instead.
    pub next_object_id: u64,
    /// Boot epoch from the loader. Not an identifier and not entropy.
    pub boot_epoch: u64,
    /// Whether the boot package selected the K2 supervisor path.
    pub managed_boot: bool,
    /// One bit per assigned operation, set when the dispatch reached it.
    pub operations_reached: u64,
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
            threads: [const { Thread::empty() }; MAX_THREADS],
            scopes: [const { Scope::empty() }; MAX_SCOPES],
            grants: [Grant::empty(); MAX_GRANTS],
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
            cpus: [CpuSlot::empty(); MAX_CPUS],
            cpus_online: 0,
            ticks: 0,
            preemptions: 0,
            budget_stalls: 0,
            reclaimed_frames: 0,
            user_faults: 0,
            modules_rejected: 0,
            preempt_records: 0,
            next_object_id: 1,
            boot_epoch: 0,
            managed_boot: false,
            operations_reached: 0,
            root_scope: None,
            system_log: None,
            supervisor: None,
        }
    }

    /// Thread dispatched on the processor executing this call.
    ///
    /// Reading the processor's own identity rather than a single global field
    /// is the whole difference between a uniprocessor scheduler and this one:
    /// there is no "the" running thread any more.
    #[must_use]
    pub fn current(&self) -> usize {
        let cpu = crate::percpu::index();
        let index = self.cpus[cpu.min(MAX_CPUS - 1)].current;
        if index == usize::MAX {
            idle_thread(cpu)
        } else {
            index
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

/// The single lock protecting every kernel object.
pub static MACHINE: SpinLock<Machine> = SpinLock::new(Machine::new());
