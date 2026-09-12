//! Threads: execution contexts, and who may touch which of their fields.
//!
//! Until this revision every thread lived inside the machine structure, under
//! the machine lock, and every wake, every block and every tick took that
//! lock. Threads now live in their own table, and the fields of a thread are
//! divided by **who owns them**, which is the only division that lets four
//! processors switch threads at once without writing each other's cache
//! lines:
//!
//! * **Atomic fields** -- state, the processor standing on the context, the
//!   entry counter, the stop request -- are read anywhere and written under
//!   the rule each one states.
//! * **The wait record** is what a waker and a sleeper have to agree on, and
//!   they agree on it through the thread's own small lock: a wake either finds
//!   the wait it was aimed at and consumes it, or finds it gone and does
//!   nothing. A timeout is a wake that consumes the same record.
//! * **The scheduling record** -- stack pointer, floating-point area,
//!   dispatch reservation, charge timestamps -- is written only by the
//!   processor the thread is standing on or the one holding it in its run
//!   queue. Two processors never own a thread at once: the state machine and
//!   [`ThreadCell::on_cpu`] are what make that true.
//! * **The control record** -- domain, identity, scope, kernel stack, address
//!   space root -- is written under the machine lock, and only while the
//!   thread is not schedulable (`Empty`, `Held`) or after it has been reaped.
//!   It is read by the scheduler without the lock, which is sound because
//!   nothing rewrites it while the thread can run.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};

use crate::arch::x86_64::fpu::FpuState;
use crate::limits::{MAX_CPUS, MAX_THREADS};
use crate::obj::ScopeId;
use crate::sync::SpinLock;

/// Lifecycle of a thread.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ThreadState {
    /// Free table slot.
    Empty = 0,
    /// Allocated to a domain that is still being built: it has a kernel stack
    /// and an initial context, and it is not schedulable until the domain is
    /// activated. Distinct from `Empty` because a slot that looked free while it
    /// was held was handed out again: adding a thread to a domain under
    /// construction reused that domain's own initial thread, overwrote its
    /// context, and left `_start` never run. Found by K5's engine, the first
    /// native domain built with a second thread.
    Held = 1,
    /// Eligible to be dispatched: in some processor's run queue.
    Ready = 2,
    /// Currently on a processor.
    Running = 3,
    /// Waiting inside a kernel entry for a reply, a message or a signal.
    Blocked = 4,
    /// Stopped; its stack may still be in use until the switch away completes.
    Dead = 5,
}

impl ThreadState {
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => ThreadState::Held,
            2 => ThreadState::Ready,
            3 => ThreadState::Running,
            4 => ThreadState::Blocked,
            5 => ThreadState::Dead,
            _ => ThreadState::Empty,
        }
    }
}

/// What a thread is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ThreadKind {
    /// The bootstrap context, which becomes the idle thread. Never enters ring
    /// 3 and belongs to no domain.
    Idle = 0,
    /// A user thread of a domain.
    User = 1,
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
    /// Waiting for a control log to hold a receipt.
    Log(u16, u32),
}

/// What a waker and a sleeper agree on, under the thread's lock.
#[derive(Clone, Copy, Debug)]
pub struct WaitRecord {
    /// What the thread is waiting for.
    pub wait: Wait,
    /// Monotonic deadline of that wait, or zero for none.
    pub deadline_ns: u64,
    /// Status a woken thread returns from its kernel entry.
    pub wake_status: i64,
    /// Auxiliary value a woken thread returns.
    pub wake_aux: u64,
}

impl WaitRecord {
    const fn empty() -> Self {
        Self {
            wait: Wait::None,
            deadline_ns: 0,
            wake_status: 0,
            wake_aux: 0,
        }
    }
}

/// The part of a thread the scheduler owns.
///
/// Written only by the processor standing on the thread or holding it in its
/// run queue; see the module documentation.
pub struct Sched {
    /// Stack pointer of the thread's stopped context.
    pub saved_rsp: u64,
    /// Eagerly saved x87/SSE state.
    pub fpu: FpuState,
    /// Nanoseconds of CPU charged to the thread.
    pub cpu_ns: u64,
    /// Monotonic time at which the thread was last dispatched or charged.
    pub dispatched_ns: u64,
    /// Monotonic time the current dispatch reservation runs out.
    pub reservation_end_ns: u64,
    /// Execution reserved for the current dispatch.
    pub dispatch_reserved_ns: u64,
    /// Execution already charged against that reservation.
    pub dispatch_charged_ns: u64,
    /// Window that reservation belongs to.
    pub dispatch_window: u64,
    /// Scope the reservation was taken in. Held separately from the
    /// effective scope because a thread may be rebound while it runs, and the
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
    /// Times the timer ended the thread's dispatch: its reservation was taken
    /// back and it ran on only if the scheduler granted it another.
    ///
    /// Not the same number as a processor's `preemptions`, which counts the
    /// times the processor was taken away and given to somebody else. On a
    /// processor with nothing else to run the two differ, and the difference
    /// is the point: a thread whose turn the timer ends and who wins the next
    /// turn was still scheduled.
    pub preemptions: u64,
    /// Detailed preemption records already emitted for this thread.
    pub preempt_records: u32,
}

impl Sched {
    const fn empty() -> Self {
        Self {
            saved_rsp: 0,
            fpu: FpuState::zeroed(),
            cpu_ns: 0,
            dispatched_ns: 0,
            reservation_end_ns: 0,
            dispatch_reserved_ns: 0,
            dispatch_charged_ns: 0,
            dispatch_window: 0,
            dispatch_scope: 0,
            dispatch_recovery: false,
            dispatched: false,
            last_cpu: usize::MAX,
            migrations: 0,
            preemptions: 0,
            preempt_records: 0,
        }
    }
}

/// The part of a thread the control plane owns.
pub struct Control {
    /// Owning domain index; meaningless for the idle thread.
    pub domain: usize,
    /// Generation of the owning domain when the thread was created.
    pub domain_generation: u32,
    /// Diagnostic identity.
    pub id: u64,
    /// Scope that pays for the thread's own domain.
    pub owner_scope: ScopeId,
    /// Kernel stack slot index.
    pub kstack_slot: usize,
    /// Top of the thread's kernel stack.
    pub kstack_top: u64,
    /// CR3 value for the thread's address space.
    pub cr3: u64,
}

impl Control {
    const fn empty() -> Self {
        Self {
            domain: usize::MAX,
            domain_generation: 0,
            id: 0,
            owner_scope: 0,
            kstack_slot: usize::MAX,
            kstack_top: 0,
            cr3: 0,
        }
    }
}

/// An execution context.
pub struct ThreadCell {
    /// Lifecycle state. Transitions follow the rules in `crate::sched`.
    state: AtomicU8,
    /// What the thread is for.
    kind: AtomicU8,
    /// One plus the index of the processor standing on this context: the one
    /// running it, or the one still switching away from it. Zero when no
    /// processor is. A thread whose context is still being written must not
    /// be resumed elsewhere, and this is what says whether it is.
    pub on_cpu: AtomicU8,
    /// Whether the thread was told to stop while it ran. Read on every return
    /// to ring 3 without a lock; set by a termination under the machine lock.
    pub stop: AtomicBool,
    /// Whether a frame from this thread has been observed at privilege level 3.
    pub ring3_confirmed: AtomicBool,
    /// Kernel entries the thread performed through `syscall`.
    pub syscalls: AtomicU64,
    /// Scope the thread's current work is charged to. Equal to the owner scope
    /// except while the thread is bound to another scope's invocation.
    /// Written by the thread itself while it runs.
    pub effective_scope: AtomicU16,
    /// Whether the current work is charged to the closure reserve.
    pub recovery: AtomicBool,
    /// Scope currently holding a parallelism slot for this thread, or
    /// [`crate::scope::NO_SCOPE`]. Taken with a swap, so the thread and a
    /// termination racing to return it cannot both return it.
    pub parallelism_scope: AtomicU16,
    /// Invocation whose origin the thread adopted: index and generation packed,
    /// or `u64::MAX` for none.
    pub bound_invocation: AtomicU64,
    /// The thread's own FS base, written to the processor on every dispatch.
    ///
    /// Register state of the thread, set only through `THREAD_POINTER_SET` and
    /// never read by the kernel for anything else: nothing in ring 0 addresses
    /// memory through FS. Zero for every thread that never set one.
    pub fs_base: AtomicU64,
    /// Deadline of the current wait, or zero. A copy of the wait record's
    /// deadline kept outside the lock, so a tick can find expired waits
    /// without taking every thread's lock.
    pub deadline_ns: AtomicU64,
    /// The wait record.
    pub wait: SpinLock<WaitRecord>,
    sched: UnsafeCell<Sched>,
    control: UnsafeCell<Control>,
}

// SAFETY: every field is either atomic, behind a lock, or behind an
// `UnsafeCell` whose access rules the module documentation states and the
// accessors below restate.
unsafe impl Sync for ThreadCell {}

impl ThreadCell {
    const fn empty() -> Self {
        Self {
            state: AtomicU8::new(0),
            kind: AtomicU8::new(0),
            on_cpu: AtomicU8::new(0),
            stop: AtomicBool::new(false),
            ring3_confirmed: AtomicBool::new(false),
            syscalls: AtomicU64::new(0),
            effective_scope: AtomicU16::new(0),
            recovery: AtomicBool::new(false),
            parallelism_scope: AtomicU16::new(crate::scope::NO_SCOPE),
            bound_invocation: AtomicU64::new(u64::MAX),
            fs_base: AtomicU64::new(0),
            deadline_ns: AtomicU64::new(0),
            wait: SpinLock::of_class(WaitRecord::empty(), crate::sync::LockClass::Wait),
            sched: UnsafeCell::new(Sched::empty()),
            control: UnsafeCell::new(Control::empty()),
        }
    }

    /// Lifecycle state.
    #[inline]
    #[must_use]
    pub fn state(&self) -> ThreadState {
        ThreadState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Publishes a state.
    #[inline]
    pub fn set_state(&self, state: ThreadState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// What the thread is for.
    #[inline]
    #[must_use]
    pub fn kind(&self) -> ThreadKind {
        if self.kind.load(Ordering::Relaxed) == 1 {
            ThreadKind::User
        } else {
            ThreadKind::Idle
        }
    }

    /// Sets what the thread is for. Under the machine lock, at creation.
    pub fn set_kind(&self, kind: ThreadKind) {
        self.kind.store(kind as u8, Ordering::Relaxed);
    }

    /// Whether the thread is a user thread.
    #[inline]
    #[must_use]
    pub fn is_user(&self) -> bool {
        self.kind() == ThreadKind::User
    }

    /// The scheduler's record.
    ///
    /// # Safety
    ///
    /// The caller must be the processor standing on this thread, or hold the
    /// run-queue lock of the queue holding it, or hold the machine lock while
    /// the thread is not schedulable.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn sched(&self) -> &mut Sched {
        // SAFETY: the caller upholds the ownership rule above.
        unsafe { &mut *self.sched.get() }
    }

    /// The control record, read-only. Sound to read from the scheduler because
    /// the record does not change while the thread can be scheduled.
    #[inline]
    #[must_use]
    pub fn control(&self) -> &Control {
        // SAFETY: writes happen only under the machine lock while the thread
        // is `Empty` or `Held` or reaped, states in which no other reader
        // exists; every other access is a read.
        unsafe { &*self.control.get() }
    }

    /// The control record, writable. Under the machine lock, while the thread
    /// is `Empty`, `Held` or reaped.
    ///
    /// # Safety
    ///
    /// See above.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn control_mut(&self) -> &mut Control {
        // SAFETY: the caller upholds the rule above.
        unsafe { &mut *self.control.get() }
    }

    /// Owning domain.
    #[inline]
    #[must_use]
    pub fn domain(&self) -> usize {
        self.control().domain
    }

    /// Scope the current work is charged to.
    #[inline]
    #[must_use]
    pub fn effective_scope(&self) -> ScopeId {
        self.effective_scope.load(Ordering::Relaxed)
    }

    /// Whether the current work is charged to the closure reserve.
    #[inline]
    #[must_use]
    pub fn is_recovery(&self) -> bool {
        self.recovery.load(Ordering::Relaxed)
    }

    /// The invocation the thread is bound to, if any.
    #[must_use]
    pub fn bound(&self) -> Option<(u16, u32)> {
        let packed = self.bound_invocation.load(Ordering::Relaxed);
        if packed == u64::MAX {
            None
        } else {
            Some(((packed >> 32) as u16, packed as u32))
        }
    }

    /// Binds the thread to an invocation, or unbinds it with `None`.
    pub fn set_bound(&self, bound: Option<(u16, u32)>) {
        let packed = bound.map_or(u64::MAX, |(index, generation)| {
            (u64::from(index) << 32) | u64::from(generation)
        });
        self.bound_invocation.store(packed, Ordering::Relaxed);
    }

    /// Takes the parallelism slot the thread holds, if any.
    #[must_use]
    pub fn take_parallelism_scope(&self) -> Option<ScopeId> {
        let scope = self
            .parallelism_scope
            .swap(crate::scope::NO_SCOPE, Ordering::AcqRel);
        if scope == crate::scope::NO_SCOPE {
            None
        } else {
            Some(scope)
        }
    }

    /// Records the scope holding a parallelism slot for this thread.
    pub fn set_parallelism_scope(&self, scope: Option<ScopeId>) {
        self.parallelism_scope
            .store(scope.unwrap_or(crate::scope::NO_SCOPE), Ordering::Release);
    }

    /// Resets a slot for a new thread. Under the machine lock, while the slot
    /// is `Empty`.
    pub fn reset(&self) {
        self.set_state(ThreadState::Empty);
        self.kind.store(0, Ordering::Relaxed);
        self.on_cpu.store(0, Ordering::Relaxed);
        self.stop.store(false, Ordering::Relaxed);
        self.ring3_confirmed.store(false, Ordering::Relaxed);
        self.syscalls.store(0, Ordering::Relaxed);
        self.effective_scope.store(0, Ordering::Relaxed);
        self.recovery.store(false, Ordering::Relaxed);
        self.parallelism_scope
            .store(crate::scope::NO_SCOPE, Ordering::Relaxed);
        self.bound_invocation.store(u64::MAX, Ordering::Relaxed);
        self.fs_base.store(0, Ordering::Relaxed);
        self.deadline_ns.store(0, Ordering::Relaxed);
        *self.wait.lock() = WaitRecord::empty();
        // SAFETY: the slot is empty and under the machine lock; no processor
        // stands on it and no run queue holds it.
        unsafe {
            *self.sched() = Sched::empty();
            *self.control_mut() = Control::empty();
        }
    }
}

/// The thread table.
static THREADS: [ThreadCell; MAX_THREADS] = [const { ThreadCell::empty() }; MAX_THREADS];

/// The thread `index`.
#[inline]
#[must_use]
pub fn get(index: usize) -> &'static ThreadCell {
    &THREADS[index.min(MAX_THREADS - 1)]
}

/// Every thread, with its index.
pub fn iter() -> impl Iterator<Item = (usize, &'static ThreadCell)> {
    THREADS.iter().enumerate()
}

/// Thread-table index of the idle thread of processor `cpu`.
///
/// The first [`MAX_CPUS`] slots are reserved for them. A processor's idle
/// thread is the context it runs when nothing else is eligible **on that
/// processor**, so there is one per processor and no domain owns any of them.
#[must_use]
pub const fn idle_thread(cpu: usize) -> usize {
    cpu
}

/// Whether `index` is a user thread slot rather than an idle one.
#[must_use]
pub const fn is_user_slot(index: usize) -> bool {
    index >= MAX_CPUS
}

/// A free user thread slot, if any. Under the machine lock.
#[must_use]
pub fn find_empty() -> Option<usize> {
    (MAX_CPUS..MAX_THREADS).find(|&index| THREADS[index].state() == ThreadState::Empty)
}

/// Wakes `index` if it is blocked on a wait that `matches`, leaving `status`
/// and `aux` for it to return.
///
/// Returns whether it was. The check and the consumption of the wait happen
/// under the thread's own lock, so two wakers -- a reply and a timeout, say
/// -- cannot both consume one wait, and a wake aimed at a wait the thread
/// has already left finds nothing.
pub fn wake_if(
    index: usize,
    matches: impl FnOnce(&WaitRecord) -> bool,
    status: i64,
    aux: u64,
    hint: crate::sched::WakeHint,
) -> bool {
    if !claim_wake(index, matches, status, aux) {
        return false;
    }
    crate::sched::wake(index, hint);
    true
}

/// Consumes a thread's wait if it matches, and leaves the thread to be woken
/// by the caller: [`wake_if`] with the wake itself deferred.
///
/// Between the claim and the wake the thread is `Blocked` with no wait to
/// match: another claim finds nothing, a deadline finds none, a cancellation
/// finds nothing to cancel. So the wake can be issued after the lock the
/// caller holds is dropped, through [`crate::sched::defer_wake`], which is
/// what keeps a queue lock, a spin on another processor's switch and an
/// inter-processor interrupt -- a microsecond and a half on this platform --
/// from happening under the control lock.
pub fn claim_wake(
    index: usize,
    matches: impl FnOnce(&WaitRecord) -> bool,
    status: i64,
    aux: u64,
) -> bool {
    let cell = get(index);
    if cell.state() != ThreadState::Blocked {
        return false;
    }
    let mut record = cell.wait.lock();
    if cell.state() != ThreadState::Blocked || !matches(&record) {
        return false;
    }
    record.wait = Wait::None;
    record.deadline_ns = 0;
    record.wake_status = status;
    record.wake_aux = aux;
    cell.deadline_ns.store(0, Ordering::Relaxed);
    true
}

/// [`claim_wake`], with the wake deferred to the next flush of this
/// processor's deferred wakes: the caller holds a lock the wake should not
/// happen under.
pub fn defer_wake_if(
    index: usize,
    matches: impl FnOnce(&WaitRecord) -> bool,
    status: i64,
    aux: u64,
    hint: crate::sched::WakeHint,
) -> bool {
    if !claim_wake(index, matches, status, aux) {
        return false;
    }
    crate::sched::defer_wake(index, hint);
    true
}

/// Registers what the running thread is about to wait for, and moves it to
/// `Blocked`. The caller then calls [`crate::sched::block_current`].
///
/// Publishing the wait and the state under the thread's lock is what makes the
/// classic race survivable: a wake that arrives between the caller's check of
/// its condition and this call finds the thread not yet blocked and does
/// nothing, so the caller must re-check its condition under the object's lock
/// before calling this -- which every caller does, by registering the wait
/// while still holding that lock.
pub fn prepare_wait(index: usize, wait: Wait, deadline_ns: u64) {
    let cell = get(index);
    let mut record = cell.wait.lock();
    record.wait = wait;
    record.deadline_ns = deadline_ns;
    record.wake_status = thalyx_abi::status::OK;
    record.wake_aux = 0;
    cell.deadline_ns.store(deadline_ns, Ordering::Relaxed);
    cell.set_state(ThreadState::Blocked);
    if deadline_ns != 0 {
        crate::sched::note_deadline(deadline_ns);
    }
}

/// Takes the status the last wake left, resetting it.
#[must_use]
pub fn take_wake_status(index: usize) -> (i64, u64) {
    let cell = get(index);
    let mut record = cell.wait.lock();
    let status = record.wake_status;
    let aux = record.wake_aux;
    record.wake_status = thalyx_abi::status::OK;
    record.wake_aux = 0;
    (status, aux)
}
