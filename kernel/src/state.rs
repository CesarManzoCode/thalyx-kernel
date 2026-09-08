//! Kernel object tables and the single lock that protects them.
//!
//! K1 keeps every kernel object in fixed-capacity tables. That is not a
//! shortcut around the resource contract: refusing to create an object when its
//! table is full is exactly the "reject before a partial object becomes
//! visible" rule, and a fixed table makes the refusal path impossible to skip.
//! The slab-backed, quota-charged metadata allocator the contract describes
//! arrives with scopes in K2.
//!
//! There is one lock. The concurrency contract allows K1 a single short global
//! lock for metadata, and one lock cannot be acquired out of order. Its two
//! rules are absolute: no context switch happens while it is held, and it is
//! never taken from a context that can already hold it, which on a uniprocessor
//! machine with every kernel path running at IF=0 means it is never contended.

use crate::arch::x86_64::fpu::FpuState;
use crate::arch::x86_64::paging::AddressSpace;
use crate::mm::frame::FrameAllocator;
use crate::sync::SpinLock;

/// Domains the kernel can hold at once.
pub const MAX_DOMAINS: usize = 8;
/// Threads the kernel can hold at once, the idle thread included.
pub const MAX_THREADS: usize = 8;
/// Kernel stack slots, one per thread.
pub const MAX_KSTACKS: usize = MAX_THREADS;
/// Table index of the idle thread, which is the bootstrap context itself.
pub const IDLE_THREAD: usize = 0;

/// Lifecycle of a domain.
///
/// `Building` is the only state in which mappings may be installed, and
/// `Runnable` is reached exactly once, through the activation gate. A domain
/// that never reaches it never executes an instruction.
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
    /// Stopped at its own request; resources not yet reclaimed.
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
}

/// What stopped a domain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitReason {
    /// The domain executed an access its authority does not cover.
    UserFault,
    /// The domain asked to stop.
    Voluntary,
}

impl ExitReason {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ExitReason::UserFault => "user_fault",
            ExitReason::Voluntary => "voluntary",
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

/// A protection domain: one address space, one capability boundary.
pub struct Domain {
    /// Lifecycle state.
    pub state: DomainState,
    /// Name from the boot package, for diagnostics only. Never authority.
    pub name: [u8; 32],
    /// Bytes of `name` that are significant.
    pub name_len: usize,
    /// The domain's address space, present from `Building` until reclamation.
    pub space: Option<AddressSpace>,
    /// The domain's thread, if one has been created.
    pub thread: Option<usize>,
    /// Entry point installed at creation.
    pub entry: u64,
    /// Loadable segments mapped.
    pub segments: usize,
    /// Pages the image's segments occupy, from the validated program headers.
    pub image_pages: u64,
    /// Whether the initial user stack is mapped.
    pub stack_mapped: bool,
    /// Fault that stopped the domain, if any.
    pub fault: Option<FaultRecord>,
    /// Why the domain stopped, if it has.
    pub exit_reason: Option<ExitReason>,
    /// Exit code the domain reported, when it stopped voluntarily.
    pub exit_code: u64,
    /// Diagnostic notes the domain emitted.
    pub notes: u64,
}

impl Domain {
    /// A free table slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: DomainState::Empty,
            name: [0; 32],
            name_len: 0,
            space: None,
            thread: None,
            entry: 0,
            segments: 0,
            image_pages: 0,
            stack_mapped: false,
            fault: None,
            exit_reason: None,
            exit_code: 0,
            notes: 0,
        }
    }

    /// The domain's name as text.
    #[must_use]
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("?")
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

/// An execution context.
pub struct Thread {
    /// Lifecycle state.
    pub state: ThreadState,
    /// What the thread is for.
    pub kind: ThreadKind,
    /// Owning domain index; meaningless for the idle thread.
    pub domain: usize,
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
            kstack_slot: usize::MAX,
            kstack_top: 0,
            saved_rsp: 0,
            cr3: 0,
            fpu: FpuState::zeroed(),
            cpu_ns: 0,
            dispatched_ns: 0,
            quantum_ticks: 0,
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
    /// Occupancy of the kernel stack slots.
    pub kstack_used: [bool; MAX_KSTACKS],
    /// Index of the running thread.
    pub current: usize,
    /// Round-robin cursor over the thread table.
    pub cursor: usize,
    /// Timer ticks since the timer was armed.
    pub ticks: u64,
    /// Involuntary switches away from a user thread.
    pub preemptions: u64,
    /// Frames reclaimed from loader and module memory after bootstrap.
    pub reclaimed_frames: usize,
    /// User faults contained.
    pub user_faults: u64,
    /// Modules the loader offered that validation refused.
    pub modules_rejected: u32,
    /// Detailed preemption records already emitted, before coalescing.
    pub preempt_records: u32,
}

impl Machine {
    const fn new() -> Self {
        Self {
            memory: None,
            kernel_space: None,
            domains: [const { Domain::empty() }; MAX_DOMAINS],
            threads: [const { Thread::empty() }; MAX_THREADS],
            kstack_used: [false; MAX_KSTACKS],
            current: IDLE_THREAD,
            cursor: 0,
            ticks: 0,
            preemptions: 0,
            reclaimed_frames: 0,
            user_faults: 0,
            modules_rejected: 0,
            preempt_records: 0,
        }
    }

    /// Frame allocator, which exists from the memory bootstrap onward.
    pub fn allocator(&mut self) -> &mut FrameAllocator {
        self.memory
            .as_mut()
            .expect("frame allocator established during bootstrap")
    }
}

/// The single lock protecting every kernel object in K1.
pub static MACHINE: SpinLock<Machine> = SpinLock::new(Machine::new());
