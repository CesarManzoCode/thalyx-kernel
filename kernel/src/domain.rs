//! Domain construction, activation, termination and reclamation.
//!
//! The activation gate is the mechanism behind "an incomplete domain never
//! executes user code": a domain leaves `Building` exactly once, and only after
//! its address space, every loadable segment, a stack with a guard page below
//! it, a kernel stack and a thread context all exist. Any failure on the way
//! there tears the partial domain down instead of leaving it visible.
//!
//! Reclamation is split from termination. Marking a domain dead is immediate
//! and happens in the faulting thread's own context; walking its page tables
//! and returning its frames happens later, from a context that is neither
//! standing on its kernel stack nor running in its address space. That split is
//! the reason a user fault does not have to unwind anything to be contained.

use thalyx_boot_protocol::PAGE_SIZE;

use crate::arch::x86_64::context;
use crate::arch::x86_64::cpu;
use crate::arch::x86_64::fpu;
use crate::arch::x86_64::paging::{AddressSpace, MapError};
use crate::arch::x86_64::trap::TrapFrame;
use crate::elf::{self, Reject};
use crate::layout;
use crate::mm::{Owner, Rights};
use crate::obj::ScopeId;
use crate::scope::{self, Resource};
use crate::state::{
    Domain, DomainState, ExitReason, FaultRecord, MACHINE, MAX_DOMAINS, MAX_KSTACKS, MAX_THREADS,
    Machine, ThreadKind, ThreadState,
};
use crate::thread;
use crate::{event, trace};

/// Why a domain could not be created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CreateError {
    /// The domain table is full.
    DomainTableFull,
    /// The thread table is full.
    ThreadTableFull,
    /// No kernel stack slot is free.
    KernelStackExhausted,
    /// The module image did not validate.
    Image(Reject),
    /// A mapping was refused.
    Map(MapError),
    /// A frame or table could not be allocated.
    OutOfMemory,
    /// A reservation did not fit in the owning scope or one of its ancestors.
    LimitExhausted,
    /// The owning scope is not open.
    ScopeClosed,
}

impl CreateError {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CreateError::DomainTableFull => "domain_table_full",
            CreateError::ThreadTableFull => "thread_table_full",
            CreateError::KernelStackExhausted => "kernel_stack_exhausted",
            CreateError::Image(reject) => reject.name(),
            CreateError::Map(error) => error.name(),
            CreateError::OutOfMemory => "out_of_memory",
            CreateError::LimitExhausted => "limit_exhausted",
            CreateError::ScopeClosed => "scope_closed",
        }
    }
}

impl From<MapError> for CreateError {
    fn from(error: MapError) -> Self {
        match error {
            MapError::OutOfMemory => CreateError::OutOfMemory,
            other => CreateError::Map(other),
        }
    }
}

/// Why activation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivationRefusal {
    /// The domain is not under construction.
    NotBuilding,
    /// No address space.
    NoAddressSpace,
    /// No loadable segment was mapped.
    NoSegments,
    /// The initial stack is not mapped.
    NoStack,
    /// No thread context exists.
    NoThread,
    /// The entry point is not mapped executable in the domain.
    EntryNotExecutable,
    /// No supervisor channel is installed to report a fault on.
    NoFaultChannel,
    /// The scope that pays for the domain is not open.
    ScopeClosed,
}

impl ActivationRefusal {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ActivationRefusal::NotBuilding => "not_building",
            ActivationRefusal::NoAddressSpace => "no_address_space",
            ActivationRefusal::NoSegments => "no_segments",
            ActivationRefusal::NoStack => "no_stack",
            ActivationRefusal::NoThread => "no_thread",
            ActivationRefusal::EntryNotExecutable => "entry_not_executable",
            ActivationRefusal::NoFaultChannel => "no_fault_channel",
            ActivationRefusal::ScopeClosed => "scope_closed",
        }
    }
}

pub(crate) fn allocate_kernel_stack(machine: &mut Machine) -> Result<(usize, u64), CreateError> {
    let slot = machine
        .kstack_used
        .iter()
        .position(|used| !used)
        .ok_or(CreateError::KernelStackExhausted)?;
    let base = layout::kstack_slot_base(slot);

    let mut mapped = 0u64;
    let mut failure = None;
    for page in 0..layout::KSTACK_PAGES {
        // The slot's first page is left unmapped as a guard: a kernel stack
        // overflow faults on it instead of writing into the previous slot.
        let vaddr = base + (page + 1) * PAGE_SIZE;
        let frame = match machine.allocator().alloc(Owner::Kernel) {
            Ok(frame) => frame,
            Err(_) => {
                failure = Some(CreateError::OutOfMemory);
                break;
            }
        };
        let (space, allocator) = split_space(machine);
        if let Err(error) = space.map(vaddr, frame, Rights::KERNEL_RW, allocator, Owner::Kernel) {
            failure = Some(error.into());
            break;
        }
        mapped += 1;
    }

    if let Some(error) = failure {
        for page in 0..mapped {
            let vaddr = base + (page + 1) * PAGE_SIZE;
            let (space, allocator) = split_space(machine);
            if let Some(frame) = space.unmap(vaddr) {
                // The range was never handed to a thread, but the kernel half
                // is shared, so the entry could have been walked on another
                // processor. Quarantine rather than immediate reuse.
                allocator.retire(frame, Owner::Kernel);
                // SAFETY: the entry was just removed from the address space
                // this processor is running in.
                unsafe { cpu::invlpg(vaddr) };
            }
        }
        return Err(error);
    }

    machine.kstack_used[slot] = true;
    Ok((slot, base + layout::KSTACK_SLOT_PAGES * PAGE_SIZE))
}

fn release_kernel_stack(machine: &mut Machine, slot: usize) {
    if slot >= MAX_KSTACKS || !machine.kstack_used[slot] {
        return;
    }
    // One generation for the whole stack: every page of it has its entry
    // cleared here, under one lock.
    let stamp = crate::tlb::retire_stamp();
    let base = layout::kstack_slot_base(slot);
    for page in 0..layout::KSTACK_PAGES {
        let vaddr = base + (page + 1) * PAGE_SIZE;
        let (space, allocator) = split_space(machine);
        if let Some(frame) = space.unmap(vaddr) {
            // The mapping is gone from this processor's tables, but another
            // processor may still hold the translation: the kernel half is
            // shared, so this address means the same thing everywhere. The
            // frame therefore goes to quarantine and comes back only once every
            // processor has invalidated past this point.
            allocator.retire_at(frame, Owner::Kernel, stamp);
            // SAFETY: the entry was just removed from the address space this
            // processor is running in; invalidating it is what makes the
            // removal take effect here.
            unsafe { cpu::invlpg(vaddr) };
        }
    }
    machine.kstack_used[slot] = false;
}

/// Splits the borrow of the kernel address space from the borrow of the
/// allocator, which every mapping call needs at once.
fn split_space(
    machine: &mut Machine,
) -> (&mut AddressSpace, &mut crate::mm::frame::FrameAllocator) {
    let space = machine
        .kernel_space
        .as_mut()
        .expect("kernel address space established");
    let allocator = machine
        .memory
        .as_mut()
        .expect("frame allocator established");
    (space, allocator)
}

/// Creates a domain from a validated image and leaves it in `Building`.
///
/// `image` is the module's bytes, reachable through the direct map. Everything
/// the domain will hold is reserved in `owner_scope` and every ancestor before
/// a frame is taken: a domain that does not fit inside its scope is refused,
/// not built and then charged.
pub fn create_in(
    machine: &mut Machine,
    name: &str,
    image: &[u8],
    owner_scope: ScopeId,
    managed: bool,
) -> Result<usize, CreateError> {
    let parsed = elf::parse_user_image(image).map_err(CreateError::Image)?;

    if scope::table()[owner_scope as usize].state() != scope::State::Open {
        return Err(CreateError::ScopeClosed);
    }
    // A domain arrives with a thread, and that thread will charge this scope.
    // Refusing here rather than after the build keeps a scope at its
    // parallelism limit from being given a half-built domain to reject.
    if !scope::has_parallelism(owner_scope) {
        return Err(CreateError::LimitExhausted);
    }
    let index = match machine
        .domains
        .iter()
        .position(|domain| domain.state == DomainState::Empty)
    {
        Some(index) => index,
        None => {
            // No empty slot: the oldest dead one nothing names any more, if
            // there is one, gives up its accounting for the newcomer.
            let dead = (0..MAX_DOMAINS)
                .find(|&candidate| collect_dead(machine, candidate))
                .ok_or(CreateError::DomainTableFull)?;
            dead
        }
    };
    let thread_index = thread::find_empty().ok_or(CreateError::ThreadTableFull)?;
    let id = index as u16;
    let owner = Owner::Domain(id);

    // The reservation is deliberately an upper bound taken up front: image
    // pages, the initial stack, the kernel stack of the first thread and the
    // page tables the mappings will need. The surplus is returned once the
    // build has finished and the real charge is known, so the scope is never
    // credited with pages the domain actually holds.
    let reserved_pages =
        parsed.pages + layout::USER_STACK_PAGES + layout::KSTACK_PAGES + TABLE_RESERVE_PAGES;
    if !scope::reserve(owner_scope, Resource::MemoryPages, reserved_pages) {
        return Err(CreateError::LimitExhausted);
    }
    // One object for the domain, one for its first thread.
    if !scope::reserve(owner_scope, Resource::Metadata, 2) {
        scope::release(owner_scope, Resource::MemoryPages, reserved_pages);
        return Err(CreateError::LimitExhausted);
    }
    let Some(identity) = machine.next_id() else {
        scope::release(owner_scope, Resource::MemoryPages, reserved_pages);
        scope::release(owner_scope, Resource::Metadata, 2);
        return Err(CreateError::LimitExhausted);
    };

    // Reserve the domain slot before anything observable is built, so a
    // concurrent creation cannot take the same slot and a failure cannot leave
    // a half-built domain reachable.
    let generation = machine.domains[index].generation.saturating_add(1);
    machine.domains[index] = Domain::empty();
    machine.domains[index].state = DomainState::Building;
    machine.domains[index].generation = generation;
    machine.domains[index].id = identity;
    machine.domains[index].owner_scope = owner_scope;
    machine.domains[index].managed = managed;
    machine.domains[index].reserved_pages = reserved_pages;
    machine.domains[index].reserved_metadata = 2;
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(32);
    machine.domains[index].name[..name_len].copy_from_slice(&name_bytes[..name_len]);
    machine.domains[index].name_len = name_len;

    match build(machine, index, thread_index, owner, image, &parsed) {
        Ok(()) => {
            scope::table()[owner_scope as usize]
                .threads
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            crate::tlb::forget_space(index);
            // The build is over, so the real charge is known. Returning the
            // surplus keeps the scope's accounting equal to what the domain
            // actually holds rather than to what it might have needed.
            let actual = machine.allocator().charged(owner) as u64 + layout::KSTACK_PAGES;
            let surplus = reserved_pages.saturating_sub(actual);
            scope::release(owner_scope, Resource::MemoryPages, surplus);
            machine.domains[index].reserved_pages = reserved_pages - surplus;
            Ok(index)
        }
        Err(error) => {
            demolish(machine, index, owner);
            scope::release(owner_scope, Resource::MemoryPages, reserved_pages);
            scope::release(owner_scope, Resource::Metadata, 2);
            machine.domains[index] = Domain::empty();
            machine.domains[index].generation = generation;
            Err(error)
        }
    }
}

/// Pages reserved up front for the page tables a domain's mappings will need.
///
/// Four levels for a handful of separated ranges. The surplus is returned after
/// the build, so an over-estimate costs nothing beyond a momentarily larger
/// reservation, while an under-estimate would let a build fail after taking
/// frames the scope had not agreed to.
pub const TABLE_RESERVE_PAGES: u64 = 12;

/// Creates a domain, taking the machine lock.
pub fn create(name: &str, image: &[u8], owner_scope: ScopeId) -> Result<usize, CreateError> {
    let mut machine = MACHINE.lock();
    create_in(&mut machine, name, image, owner_scope, false)
}

fn build(
    machine: &mut Machine,
    index: usize,
    thread_index: usize,
    owner: Owner,
    image: &[u8],
    parsed: &elf::Image,
) -> Result<(), CreateError> {
    let mut space = {
        let allocator = machine
            .memory
            .as_mut()
            .expect("frame allocator established");
        AddressSpace::new(allocator, owner)?
    };
    {
        let kernel = machine
            .kernel_space
            .as_ref()
            .expect("kernel address space established");
        space.attach_kernel(kernel);
    }
    machine.domains[index].space = Some(space);

    for segment in &parsed.segments[..parsed.segment_count] {
        let rights = Rights::user(segment.read, segment.write, segment.execute);
        for page in 0..segment.pages() {
            let frame = machine
                .allocator()
                .alloc(owner)
                .map_err(|_| CreateError::OutOfMemory)?;
            let offset = (page * PAGE_SIZE) as usize;
            let remaining = segment.file_len.saturating_sub(offset);
            let copy_len = remaining.min(PAGE_SIZE as usize);
            if copy_len > 0 {
                let source =
                    &image[segment.file_offset + offset..segment.file_offset + offset + copy_len];
                // SAFETY: `frame` was just allocated, is zeroed, is mapped
                // through the direct map and is not reachable from anywhere
                // else. `source` is inside the module image, whose bounds the
                // ELF reader already checked. The ranges cannot overlap: the
                // frame is free memory and the image is loader memory.
                unsafe {
                    core::ptr::copy_nonoverlapping(source.as_ptr(), frame.hhdm_ptr(), copy_len);
                }
            }
            let space = machine.domains[index]
                .space
                .as_mut()
                .expect("space installed above");
            let allocator = machine
                .memory
                .as_mut()
                .expect("frame allocator established");
            space.map(
                segment.vaddr + page * PAGE_SIZE,
                frame,
                rights,
                allocator,
                owner,
            )?;
        }
        machine.domains[index].segments += 1;
    }

    // Initial stack: mapped pages below `USER_STACK_TOP`, with the page below
    // the lowest one left unmapped as a guard.
    let stack_low = layout::USER_STACK_TOP - layout::USER_STACK_PAGES * PAGE_SIZE;
    for page in 0..layout::USER_STACK_PAGES {
        let frame = machine
            .allocator()
            .alloc(owner)
            .map_err(|_| CreateError::OutOfMemory)?;
        let space = machine.domains[index]
            .space
            .as_mut()
            .expect("space installed above");
        let allocator = machine
            .memory
            .as_mut()
            .expect("frame allocator established");
        space.map(
            stack_low + page * PAGE_SIZE,
            frame,
            Rights::user(true, true, false),
            allocator,
            owner,
        )?;
    }
    machine.domains[index].stack_mapped = true;
    machine.domains[index].entry = parsed.entry;
    machine.domains[index].image_pages = parsed.pages;

    let (kstack_slot, kstack_top) = allocate_kernel_stack(machine)?;
    // SAFETY: the stack was just mapped, is 16-byte aligned by construction
    // (the slot base and the page size both are) and belongs to no other
    // thread.
    let saved_rsp = unsafe {
        context::prepare_user_thread(kstack_top, parsed.entry, layout::USER_STACK_TOP, 0)
    };

    let cr3 = machine.domains[index]
        .space
        .as_ref()
        .expect("space installed above")
        .cr3();
    let thread_id = machine.next_id().unwrap_or(0);
    let owner_scope = machine.domains[index].owner_scope;
    let domain_generation = machine.domains[index].generation;
    initialise_thread(
        thread_index,
        index,
        domain_generation,
        thread_id,
        owner_scope,
        kstack_slot,
        kstack_top,
        saved_rsp,
        cr3,
    );
    machine.domains[index].threads[0] = Some(thread_index);

    // The thread charges its owner scope from here, so it holds one of that
    // scope's parallelism slots. The limit bounds how many threads may charge
    // one scope at all -- a domain's own and any worker borrowed into it -- and
    // it is only a limit if every one of them is counted.
    scope::take_parallelism(owner_scope);
    thread::get(thread_index).set_parallelism_scope(Some(owner_scope));

    Ok(())
}

/// Writes a fresh thread into an empty slot, `Held` until activation.
///
/// A slot is reused across domains. Everything the previous occupant left --
/// a thread pointer that would be an address in someone else's space, a
/// counter, a wait -- is reset before the new identity is written.
#[allow(clippy::too_many_arguments)]
fn initialise_thread(
    thread_index: usize,
    domain: usize,
    domain_generation: u32,
    thread_id: u64,
    owner_scope: ScopeId,
    kstack_slot: usize,
    kstack_top: u64,
    saved_rsp: u64,
    cr3: u64,
) {
    let cell = thread::get(thread_index);
    cell.reset();
    cell.set_kind(ThreadKind::User);
    cell.effective_scope
        .store(owner_scope, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: the slot is empty and under the machine lock; no processor
    // stands on it and no run queue holds it.
    unsafe {
        let control = cell.control_mut();
        control.domain = domain;
        control.domain_generation = domain_generation;
        control.id = thread_id;
        control.owner_scope = owner_scope;
        control.kstack_slot = kstack_slot;
        control.kstack_top = kstack_top;
        control.cr3 = cr3;
        let sched = cell.sched();
        sched.saved_rsp = saved_rsp;
        sched.fpu = fpu::initial();
    }
    // Allocated, unschedulable until activation.
    cell.set_state(ThreadState::Held);
}

fn demolish(machine: &mut Machine, index: usize, owner: Owner) {
    for slot in 0..crate::state::MAX_THREADS_PER_DOMAIN {
        let Some(thread_index) = machine.domains[index].threads[slot].take() else {
            continue;
        };
        let cell = thread::get(thread_index);
        let kstack = cell.control().kstack_slot;
        release_kernel_stack(machine, kstack);
        // Never scheduled: the slot goes back untouched by any processor.
        cell.reset();
    }
    if let Some(mut space) = machine.domains[index].space.take() {
        let allocator = machine
            .memory
            .as_mut()
            .expect("frame allocator established");
        let _ = space.destroy_user_half(allocator, owner);
        // SAFETY: the space was never activated on any CPU and its user half
        // has just been emptied.
        unsafe { space.release_root(allocator, owner) };
    }
}

/// Moves a domain from `Building` to `Runnable`.
///
/// This is the single point at which a domain becomes able to execute. Every
/// requirement is checked here rather than assumed from the construction path,
/// so a future construction path that forgets one is refused rather than
/// trusted.
pub fn activate(index: usize) -> Result<(), ActivationRefusal> {
    let mut machine = MACHINE.lock();
    activate_in(&mut machine, index)
}

/// Moves a domain from `Building` to `Runnable`, with the lock already held.
pub fn activate_in(machine: &mut Machine, index: usize) -> Result<(), ActivationRefusal> {
    if index >= MAX_DOMAINS || machine.domains[index].state != DomainState::Building {
        return Err(ActivationRefusal::NotBuilding);
    }
    if machine.domains[index].space.is_none() {
        return Err(ActivationRefusal::NoAddressSpace);
    }
    if machine.domains[index].segments == 0 {
        return Err(ActivationRefusal::NoSegments);
    }
    if !machine.domains[index].stack_mapped {
        return Err(ActivationRefusal::NoStack);
    }
    let Some(thread_index) = machine.domains[index].first_thread() else {
        return Err(ActivationRefusal::NoThread);
    };
    if thread_index >= MAX_THREADS || thread::get(thread_index).control().kstack_top == 0 {
        return Err(ActivationRefusal::NoThread);
    }

    let entry = machine.domains[index].entry;
    let executable = machine.domains[index]
        .space
        .as_ref()
        .and_then(|space| space.translate(entry))
        .is_some_and(|(_, flags)| flags & (1 << 63) == 0 && flags & (1 << 2) != 0);
    if !executable {
        return Err(ActivationRefusal::EntryNotExecutable);
    }

    if machine.domains[index].managed && machine.domains[index].fault_endpoint.is_none() {
        return Err(ActivationRefusal::NoFaultChannel);
    }
    let owner = machine.domains[index].owner_scope;
    if scope::table()[owner as usize].state() != scope::State::Open {
        return Err(ActivationRefusal::ScopeClosed);
    }

    machine.domains[index].state = DomainState::Runnable;
    for slot in 0..crate::state::MAX_THREADS_PER_DOMAIN {
        if let Some(thread) = machine.domains[index].threads[slot] {
            // The activator keeps running; the new thread goes to an idle
            // processor, or waits its turn on a busy one.
            crate::sched::start_thread(thread);
        }
    }
    let _ = thread_index;
    Ok(())
}

/// Stops the running domain because it faulted, and does not return.
pub fn terminate_on_fault(frame: &TrapFrame, cr2: u64) -> ! {
    let record = FaultRecord {
        vector: frame.vector,
        error_code: frame.error_code,
        cr2,
        rip: frame.rip,
        rsp: frame.rsp,
        cs: frame.cs,
    };

    let (domain_index, thread_index) = {
        let thread_index = crate::sched::current_thread();
        let cell = thread::get(thread_index);
        let domain_index = cell.domain();
        let mut machine = MACHINE.lock();
        // A thread that was stopped by authority while it was running, and
        // whose withdrawn mappings caught up with it before the kick did. It
        // has not done anything its program should be charged with; it leaves,
        // and the fault is not counted against a domain already terminated.
        if cell.stop.load(core::sync::atomic::Ordering::Acquire) {
            let name = machine.domains[domain_index].name_str();
            event!(
                "thread.stopped_late",
                "cpu={} domain={domain_index} name={name} thread={thread_index} rip=0x{:x} \
                 vector=0x{:x} cr2=0x{:x} class=stopped_by_authority action=leave \
                 kernel=survives",
                crate::percpu::index(),
                record.rip,
                record.vector,
                record.cr2
            );
            drop(machine);
            crate::sched::switch_away_from_dead();
        }
        machine.user_faults += 1;
        machine.domains[domain_index].fault = Some(record);
        machine.domains[domain_index].faults += 1;

        let name = machine.domains[domain_index].name_str();
        // SAFETY: the faulting thread is this processor's current thread.
        let cpu_ns = unsafe { cell.sched().cpu_ns };
        event!(
            "user.fault",
            "domain={domain_index} name={name} thread={thread_index} vector={} error=0x{:x} \
             cr2=0x{:x} rip=0x{:x} rsp=0x{:x} cs=0x{:x} cpl={} class=user_fault \
             action=terminate_domain kernel=survives cpu_ns={cpu_ns}",
            record.vector,
            record.error_code,
            record.cr2,
            record.rip,
            record.rsp,
            record.cs,
            (record.cs & 3) as u8
        );

        // The supervisor is told before the domain is torn down, using the
        // record reserved when the channel was installed, so a full queue
        // cannot lose a fault report.
        crate::api::ipcops::deliver_fault(&mut machine, domain_index, thread_index, &record);
        terminate_in(&mut machine, domain_index, ExitReason::UserFault, 0);
        (domain_index, thread_index)
    };
    let _ = (domain_index, thread_index);

    crate::sched::switch_away_from_dead()
}

/// Stops the running domain at its own request, and does not return.
pub fn terminate_voluntarily(code: u64) -> ! {
    {
        let thread_index = crate::sched::current_thread();
        let domain_index = thread::get(thread_index).domain();
        let mut machine = MACHINE.lock();
        terminate_in(&mut machine, domain_index, ExitReason::Voluntary, code);
    }
    crate::sched::switch_away_from_dead()
}

/// Name of a domain, for diagnostic records.
#[must_use]
pub fn domain_name(index: usize) -> &'static str {
    // SAFETY: names are written once during creation and read-only afterwards;
    // K1 is uniprocessor with interrupts masked in kernel context. The returned
    // reference points into the `MACHINE` static, which lives forever.
    let machine = unsafe { &*MACHINE.as_mut_ptr() };
    machine.domains[index].name_str()
}

/// Whether nothing anywhere in the machine is still standing on `index`.
///
/// "Not the address space this processor is in" was a sufficient test with one
/// processor and is not one with several. Three things have to be true of every
/// online processor, and each of them is a way a reclamation could pull the
/// ground out from under a processor that is still running:
///
/// * it must not be running a thread of this domain, or the reclamation would
///   free a kernel stack a processor is executing on;
/// * it must not still be *leaving* one — the thread it switched away from and
///   has not yet published — because until that publication the outgoing
///   context is still being written on that stack;
/// * neither of those threads may name this address space, because a processor
///   loads the new root only after it has released the lock, so between the two
///   it is still executing with the old one in `CR3`.
///
/// The dying thread itself satisfies all three the moment its own switch
/// completes, which is exactly when the stack becomes free.
fn reapable(machine: &crate::state::Machine, index: usize) -> bool {
    let Some(space) = machine.domains[index].space.as_ref() else {
        return false;
    };
    !crate::sched::standing_on(index, machine.domains[index].generation, space.cr3())
}

/// Interrupts every other processor that is running a thread of `index`.
///
/// Called under the machine lock from a termination, so the set of processors
/// standing on the domain cannot change under it. The calling processor is
/// left out: if it is running one of the threads, it is the caller itself, and
/// the caller leaves on its own way out.
fn kick_running_threads(machine: &Machine, index: usize) {
    crate::sched::kick_domain(index, machine.domains[index].generation);
}

/// Frees the table slot of a domain that has stopped, been reaped, and is
/// named by no capability.
///
/// Everything else the domain held went before this: its threads and address
/// space at the reap, its capabilities and fault channel at termination, its
/// pages and metadata back to its scope. The slot keeps its generation, so a
/// handle that outlived the domain names nothing rather than the next
/// occupant. Returns whether the slot was freed.
///
/// Called when a creation finds no empty slot, not when the domain dies: a
/// dead domain's slot is also its accounting -- what it was charged, how it
/// ended -- and the run's per-domain summary reads it at the end. Until K6 a
/// dead domain kept its slot for the rest of the boot, and K6's closure
/// benchmark, which builds and stops a domain per sample, found the table of
/// sixteen full after sixteen.
pub fn collect_dead(machine: &mut Machine, index: usize) -> bool {
    let domain = &machine.domains[index];
    if domain.state != DomainState::Dead
        || domain.refs != 0
        || domain.space.is_some()
        || domain.thread_count() != 0
    {
        return false;
    }
    // Frames of the dead domain still in quarantine wait out their
    // invalidation on the kernel's account rather than the next occupant's.
    machine
        .allocator()
        .reassign_quarantine(Owner::Domain(index as u16), Owner::Kernel);
    let domain = &machine.domains[index];
    let (generation, id, scope) = (domain.generation, domain.id, domain.owner_scope);
    let name = domain.name_str();
    trace!(
        "domain.collected",
        "domain={index} name={name} id={id} scope={}",
        scope::table()[scope as usize].id()
    );
    crate::tlb::forget_space(index);
    machine.domains[index] = Domain::empty();
    machine.domains[index].generation = generation;
    true
}

/// Set while at least one domain is `Faulted` or `Stopping`.
///
/// Every processor's idle loop asks whether there is anything to reclaim, and
/// asking used to mean taking the machine lock -- on four processors, in a
/// loop, competing with every entry that needs the control plane. The flag
/// answers the common case, "nothing died", without touching it. It is set
/// when a domain stops and cleared only by a sweep that found nothing left to
/// reclaim, so it is never clear while a reapable domain exists.
static DEAD_PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Records that a domain has stopped and will need reclaiming.
pub fn note_dead() {
    DEAD_PENDING.store(true, core::sync::atomic::Ordering::Release);
}

/// Reclaims every domain that has stopped and that no processor is still
/// standing on.
pub fn reap_dead() {
    if !DEAD_PENDING.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    loop {
        let mut machine = MACHINE.lock();
        let candidate = (0..MAX_DOMAINS).find(|&index| {
            matches!(
                machine.domains[index].state,
                DomainState::Faulted | DomainState::Stopping
            ) && reapable(&machine, index)
        });
        let Some(index) = candidate else {
            // Cleared only when no domain is waiting to be reclaimed at all,
            // not merely when none is ready: one that is still standing on a
            // processor must keep the flag set, or nobody would come back.
            if !(0..MAX_DOMAINS).any(|index| {
                matches!(
                    machine.domains[index].state,
                    DomainState::Faulted | DomainState::Stopping
                )
            }) {
                DEAD_PENDING.store(false, core::sync::atomic::Ordering::Release);
            }
            return;
        };

        let owner = Owner::Domain(index as u16);
        let charged_before = machine.allocator().charged(owner);
        // Counted before the slots are emptied. Read afterwards it is always
        // zero, and a domain that held two threads would return one of them to
        // its scope: the scope would then never see itself as empty, and a
        // drain waiting on it would wait forever.
        let threads = machine.domains[index].thread_count() as u32;

        for slot in 0..crate::state::MAX_THREADS_PER_DOMAIN {
            let Some(thread_index) = machine.domains[index].threads[slot].take() else {
                continue;
            };
            let cell = thread::get(thread_index);
            let kstack = cell.control().kstack_slot;
            release_kernel_stack(&mut machine, kstack);
            // Off every processor and out of every queue: `reapable` said so
            // under this lock, and a dead thread is never enqueued again.
            cell.reset();
        }

        let reclaimed = {
            let mut space = machine.domains[index].space.take().expect("checked above");
            let allocator = machine
                .memory
                .as_mut()
                .expect("frame allocator established");
            let counts = space.destroy_user_half(allocator, owner);
            // SAFETY: the space is not loaded in CR3 (checked above), its user
            // half is empty, and the only thread that could have run in it is
            // dead and no longer on its kernel stack.
            unsafe { space.release_root(allocator, owner) };
            counts
        };

        let charged_after = machine.allocator().charged(owner);
        let quarantined = machine.allocator().quarantined_for(owner);
        let free_frames = machine.allocator().free_frames();

        // The scope is credited back exactly what it was charged for this
        // domain, and the domain's slot moves on a generation so a reference
        // that outlived it names nothing.
        let scope_index = machine.domains[index].owner_scope;
        let pages = machine.domains[index].reserved_pages;
        let metadata = machine.domains[index].reserved_metadata;
        scope::release(scope_index, Resource::MemoryPages, pages);
        scope::release(scope_index, Resource::Metadata, metadata);
        scope::subtract32(
            &scope::table()[scope_index as usize].threads,
            threads.max(1),
        );
        machine.domains[index].reserved_pages = 0;
        machine.domains[index].reserved_metadata = 0;
        machine.domains[index].state = DomainState::Dead;
        let reason = machine.domains[index]
            .exit_reason
            .map_or("unknown", ExitReason::name);
        let data = reclaimed.data_frames;
        let tables = reclaimed.table_frames;
        drop(machine);
        let name = domain_name(index);

        trace!(
            "mm.reclaimed",
            "domain={index} name={name} reason={reason} data_frames={data} \
             table_frames={tables} charged_before={charged_before} charged_after={charged_after} \
             quarantined={quarantined} free_frames={free_frames} release=deferred"
        );
    }
}

/// Adds a thread to a domain under construction.
///
/// The entry point and the stack are checked against the domain's own mappings,
/// not taken on trust: a thread whose stack is not writable memory of its
/// domain would fault on its first push, and a supervisor that got the address
/// wrong should learn so here rather than through a fault report.
pub fn add_thread_in(
    machine: &mut Machine,
    index: usize,
    entry: u64,
    stack_top: u64,
    argument: u64,
) -> Result<usize, CreateError> {
    if machine.domains[index].state != DomainState::Building {
        return Err(CreateError::ScopeClosed);
    }
    let slot = machine.domains[index]
        .threads
        .iter()
        .position(|thread| thread.is_none())
        .ok_or(CreateError::ThreadTableFull)?;
    let thread_index = thread::find_empty().ok_or(CreateError::ThreadTableFull)?;

    let owner_scope = machine.domains[index].owner_scope;
    if !scope::has_parallelism(owner_scope) {
        return Err(CreateError::LimitExhausted);
    }
    if !scope::reserve(owner_scope, Resource::MemoryPages, layout::KSTACK_PAGES) {
        return Err(CreateError::LimitExhausted);
    }
    if !scope::reserve(owner_scope, Resource::Metadata, 1) {
        scope::release(owner_scope, Resource::MemoryPages, layout::KSTACK_PAGES);
        return Err(CreateError::LimitExhausted);
    }

    let (kstack_slot, kstack_top) = match allocate_kernel_stack(machine) {
        Ok(value) => value,
        Err(error) => {
            scope::release(owner_scope, Resource::MemoryPages, layout::KSTACK_PAGES);
            scope::release(owner_scope, Resource::Metadata, 1);
            return Err(error);
        }
    };

    // SAFETY: the stack was just mapped, is 16-byte aligned by construction and
    // belongs to no other thread.
    let saved_rsp = unsafe { context::prepare_user_thread(kstack_top, entry, stack_top, argument) };

    let cr3 = machine.domains[index]
        .space
        .as_ref()
        .expect("building domain has a space")
        .cr3();
    let thread_id = machine.next_id().unwrap_or(0);
    let generation = machine.domains[index].generation;
    initialise_thread(
        thread_index,
        index,
        generation,
        thread_id,
        owner_scope,
        kstack_slot,
        kstack_top,
        saved_rsp,
        cr3,
    );

    machine.domains[index].threads[slot] = Some(thread_index);
    machine.domains[index].reserved_pages += layout::KSTACK_PAGES;
    machine.domains[index].reserved_metadata += 1;
    scope::table()[owner_scope as usize]
        .threads
        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    scope::take_parallelism(owner_scope);
    thread::get(thread_index).set_parallelism_scope(Some(owner_scope));
    Ok(thread_index)
}

/// Stops every thread of a domain and releases what the kernel can release at
/// once.
///
/// What it does **not** do is discharge obligations a server already accepted.
/// The death of a client cancels messages nobody has received; a request that
/// was received is the receiver's obligation and stays counted until the
/// receiver resolves it. That asymmetry is the execution contract's, and it is
/// the difference between closing a client and pretending its work never
/// happened.
pub fn terminate_in(machine: &mut Machine, index: usize, reason: ExitReason, code: u64) -> bool {
    let state = machine.domains[index].state;
    if !matches!(state, DomainState::Building | DomainState::Runnable) {
        return false;
    }
    machine.domains[index].state = match reason {
        ExitReason::UserFault => DomainState::Faulted,
        _ => DomainState::Stopping,
    };
    machine.domains[index].exit_reason = Some(reason);
    machine.domains[index].exit_code = code;
    note_dead();

    for slot in 0..crate::state::MAX_THREADS_PER_DOMAIN {
        let Some(thread) = machine.domains[index].threads[slot] else {
            continue;
        };
        crate::sched::stop_thread(thread);
    }

    // A thread of this domain that is on another processor right now keeps
    // executing user instructions until something brings it into the kernel.
    // That something is sent here, so it is this and not the page fault the
    // withdrawn mappings below would otherwise produce: the processor takes the
    // interrupt, finds its current thread `Dead`, and leaves it.
    kick_running_threads(machine, index);

    // Direct access goes away with the mappings, before the frames are touched.
    for map_index in 0..machine.maps.len() {
        if machine.maps[map_index].used && machine.maps[map_index].domain as usize == index {
            crate::api::memops::withdraw_map(machine, map_index);
        }
    }

    crate::api::ipcops::on_domain_death(machine, index);

    // Capabilities the domain held stop keeping anything alive.
    for slot in 0..crate::limits::MAX_CAPS {
        crate::api::cap_release_slot(machine, index, slot);
    }

    let name = machine.domains[index].name_str();
    let scope = machine.domains[index].owner_scope;
    trace!(
        "domain.terminated",
        "domain={index} name={name} reason={} code=0x{code:x} scope={} threads={} \
         notes={} invocations={} refusals={}",
        reason.name(),
        scope::table()[scope as usize].id(),
        machine.domains[index].thread_count(),
        machine.domains[index].notes,
        machine.domains[index].invocations,
        machine.domains[index].refusals
    );
    true
}
