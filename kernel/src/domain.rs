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
use crate::event;
use crate::layout;
use crate::mm::{Owner, Rights};
use crate::state::{
    Domain, DomainState, ExitReason, FaultRecord, IDLE_THREAD, MACHINE, MAX_DOMAINS, MAX_KSTACKS,
    MAX_THREADS, Machine, ThreadKind, ThreadState,
};

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
        }
    }
}

fn allocate_kernel_stack(machine: &mut Machine) -> Result<(usize, u64), CreateError> {
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
                // SAFETY: the mapping was just removed and no other reference
                // to the frame exists; the range was never handed to a thread.
                unsafe {
                    allocator.release(frame, Owner::Kernel);
                    cpu::invlpg(vaddr);
                }
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
    let base = layout::kstack_slot_base(slot);
    for page in 0..layout::KSTACK_PAGES {
        let vaddr = base + (page + 1) * PAGE_SIZE;
        let (space, allocator) = split_space(machine);
        if let Some(frame) = space.unmap(vaddr) {
            // SAFETY: the thread that used this stack is dead and the CPU is no
            // longer executing on it, which the caller established by switching
            // away first. K1 has no DMA and no other core, so the removed
            // translation cannot be in use anywhere.
            unsafe {
                allocator.release(frame, Owner::Kernel);
                cpu::invlpg(vaddr);
            }
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

/// Creates a domain from a validated boot module and leaves it in `Building`.
///
/// `image` is the module's bytes, reachable through the direct map.
pub fn create(name: &str, image: &[u8]) -> Result<usize, CreateError> {
    let parsed = elf::parse_user_image(image).map_err(CreateError::Image)?;

    let mut machine = MACHINE.lock();
    let index = machine
        .domains
        .iter()
        .position(|domain| domain.state == DomainState::Empty)
        .ok_or(CreateError::DomainTableFull)?;
    let thread_index = machine
        .threads
        .iter()
        .enumerate()
        .position(|(slot, thread)| slot != IDLE_THREAD && thread.state == ThreadState::Empty)
        .ok_or(CreateError::ThreadTableFull)?;
    let id = index as u16;
    let owner = Owner::Domain(id);

    // Reserve the domain slot before anything observable is built, so a
    // concurrent creation cannot take the same slot and a failure cannot leave
    // a half-built domain reachable.
    machine.domains[index].state = DomainState::Building;
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(32);
    machine.domains[index].name[..name_len].copy_from_slice(&name_bytes[..name_len]);
    machine.domains[index].name_len = name_len;

    match build(&mut machine, index, thread_index, owner, image, &parsed) {
        Ok(()) => Ok(index),
        Err(error) => {
            demolish(&mut machine, index, owner);
            machine.domains[index] = Domain::empty();
            Err(error)
        }
    }
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
    let saved_rsp =
        unsafe { context::prepare_user_thread(kstack_top, parsed.entry, layout::USER_STACK_TOP) };

    let cr3 = machine.domains[index]
        .space
        .as_ref()
        .expect("space installed above")
        .cr3();
    let thread = &mut machine.threads[thread_index];
    thread.state = ThreadState::Empty; // stays unschedulable until activation
    thread.kind = ThreadKind::User;
    thread.domain = index;
    thread.kstack_slot = kstack_slot;
    thread.kstack_top = kstack_top;
    thread.saved_rsp = saved_rsp;
    thread.cr3 = cr3;
    thread.fpu = fpu::initial();
    thread.cpu_ns = 0;
    thread.dispatched_ns = 0;
    thread.quantum_ticks = 0;
    thread.preemptions = 0;
    thread.syscalls = 0;
    thread.ring3_confirmed = false;
    machine.domains[index].thread = Some(thread_index);

    Ok(())
}

fn demolish(machine: &mut Machine, index: usize, owner: Owner) {
    if let Some(thread_index) = machine.domains[index].thread.take() {
        let slot = machine.threads[thread_index].kstack_slot;
        release_kernel_stack(machine, slot);
        machine.threads[thread_index].state = ThreadState::Empty;
        machine.threads[thread_index].kstack_slot = usize::MAX;
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
    let Some(thread_index) = machine.domains[index].thread else {
        return Err(ActivationRefusal::NoThread);
    };
    if thread_index >= MAX_THREADS || machine.threads[thread_index].kstack_top == 0 {
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

    machine.domains[index].state = DomainState::Runnable;
    machine.threads[thread_index].state = ThreadState::Ready;
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

    let (domain_index, thread_index, notes, cpu_ns, preemptions, syscalls) = {
        let mut machine = MACHINE.lock();
        let thread_index = machine.current;
        let domain_index = machine.threads[thread_index].domain;
        machine.user_faults += 1;
        machine.threads[thread_index].state = ThreadState::Dead;
        machine.domains[domain_index].state = DomainState::Faulted;
        machine.domains[domain_index].fault = Some(record);
        machine.domains[domain_index].exit_reason = Some(ExitReason::UserFault);
        (
            domain_index,
            thread_index,
            machine.domains[domain_index].notes,
            machine.threads[thread_index].cpu_ns,
            machine.threads[thread_index].preemptions,
            machine.threads[thread_index].syscalls,
        )
    };

    let name = domain_name(domain_index);
    event!(
        "user.fault",
        "domain={domain_index} name={name} thread={thread_index} vector={} error=0x{:x} \
         cr2=0x{:x} rip=0x{:x} rsp=0x{:x} cs=0x{:x} cpl={} class=user_fault \
         action=terminate_domain kernel=survives",
        record.vector,
        record.error_code,
        record.cr2,
        record.rip,
        record.rsp,
        record.cs,
        (record.cs & 3) as u8
    );
    event!(
        "domain.terminated",
        "domain={domain_index} name={name} thread={thread_index} reason=user_fault \
         notes={notes} cpu_ns={cpu_ns} preemptions={preemptions} syscalls={syscalls}"
    );

    crate::sched::switch_away_from_dead()
}

/// Stops the running domain at its own request, and does not return.
pub fn terminate_voluntarily(code: u64) -> ! {
    let (domain_index, thread_index, notes, cpu_ns, preemptions, syscalls) = {
        let mut machine = MACHINE.lock();
        let thread_index = machine.current;
        let domain_index = machine.threads[thread_index].domain;
        machine.threads[thread_index].state = ThreadState::Dead;
        machine.domains[domain_index].state = DomainState::Stopping;
        machine.domains[domain_index].exit_reason = Some(ExitReason::Voluntary);
        machine.domains[domain_index].exit_code = code;
        (
            domain_index,
            thread_index,
            machine.domains[domain_index].notes,
            machine.threads[thread_index].cpu_ns,
            machine.threads[thread_index].preemptions,
            machine.threads[thread_index].syscalls,
        )
    };

    let name = domain_name(domain_index);
    event!(
        "domain.terminated",
        "domain={domain_index} name={name} thread={thread_index} reason=voluntary \
         code=0x{code:x} notes={notes} cpu_ns={cpu_ns} preemptions={preemptions} \
         syscalls={syscalls}"
    );

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

/// Reclaims every domain that has stopped and is not the current address space.
///
/// Called from the idle thread, which runs on its own kernel stack in the
/// kernel's own address space, so nothing being freed can still be in use.
pub fn reap_dead() {
    loop {
        let mut machine = MACHINE.lock();
        let active_cr3 = cpu::read_cr3();
        let candidate = machine.domains.iter().position(|domain| {
            matches!(domain.state, DomainState::Faulted | DomainState::Stopping)
                && domain
                    .space
                    .as_ref()
                    .is_some_and(|space| space.cr3() != active_cr3)
        });
        let Some(index) = candidate else { return };

        let owner = Owner::Domain(index as u16);
        let charged_before = machine.allocator().charged(owner);

        if let Some(thread_index) = machine.domains[index].thread.take() {
            let slot = machine.threads[thread_index].kstack_slot;
            release_kernel_stack(&mut machine, slot);
            machine.threads[thread_index].state = ThreadState::Empty;
            machine.threads[thread_index].kstack_slot = usize::MAX;
            machine.threads[thread_index].kstack_top = 0;
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
        let free_frames = machine.allocator().free_frames();
        machine.domains[index].state = DomainState::Dead;
        let reason = machine.domains[index]
            .exit_reason
            .map_or("unknown", ExitReason::name);
        let data = reclaimed.data_frames;
        let tables = reclaimed.table_frames;
        drop(machine);
        let name = domain_name(index);

        event!(
            "mm.reclaimed",
            "domain={index} name={name} reason={reason} data_frames={data} \
             table_frames={tables} charged_before={charged_before} charged_after={charged_after} \
             free_frames={free_frames}"
        );
    }
}
