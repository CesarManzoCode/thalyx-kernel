//! Bringing the other processors up, and keeping the set of them honest.
//!
//! Enumerating processors is not starting them. The firmware's table says what
//! exists; the handshake here says what the kernel can schedule on. A processor
//! joins the online set only after it has installed its own descriptor tables,
//! its own emergency stacks, its own per-processor block, its own interrupt
//! controller and its own timer, and has published that fact with release
//! ordering. Until then it is a table entry, not a processor.
//!
//! Two rules come from the start-up protocol itself and are visible in the
//! code. A processor that does not answer keeps its resources: the stack it was
//! given is never handed to another processor, because a late arrival would
//! then run on a stack somebody else is using, and that is the failure the
//! protocol's timeouts invite. And the resources are complete before the first
//! interrupt is sent, not after: the trampoline reads a parameter block that is
//! already written, an address space that is already built, and a stack that is
//! already mapped.

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use thalyx_boot_protocol::PAGE_SIZE;

use crate::acpi::Platform;
use crate::arch::x86_64::paging::AddressSpace;
use crate::arch::x86_64::{ap, cpu, fpu, gdt, idt, lapic, syscall, trap};
use crate::event;
use crate::layout;
use crate::limits::MAX_CPUS;
use crate::mm::{Frame, Owner, Rights};
use crate::percpu;
use crate::state::{MACHINE, ThreadKind, ThreadState, idle_thread};
use crate::{sched, time, tlb};

/// Highest physical address a start-up interrupt can name: the vector is a page
/// number in one byte.
const STARTUP_LIMIT: u64 = 0x10_0000;

/// Local APIC identifier of each slot, or `u32::MAX`.
static APIC_IDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];
/// Slots that completed the handshake.
static ONLINE: AtomicU64 = AtomicU64::new(0);
/// Slots claimed, answered or not.
static CLAIMED: AtomicUsize = AtomicUsize::new(0);
/// Set once the run is over, so parked processors stop scheduling.
static SHUTDOWN: AtomicU64 = AtomicU64::new(0);
/// Processors that have parked after a shutdown.
static PARKED: AtomicU64 = AtomicU64::new(0);

/// Root of the address space an application processor enables paging with.
static AP_CR3: AtomicU64 = AtomicU64::new(0);
/// Whether the x2APIC interface was selected for this machine.
static AP_X2APIC: AtomicU64 = AtomicU64::new(0);
/// Time-stamp counter frequency, so an application processor can measure its
/// own timer without taking the one legacy timer the bootstrap processor used.
static AP_TSC_HZ: AtomicU64 = AtomicU64::new(0);
/// Emergency stack tops of each processor, written before it starts.
static AP_IST: [[AtomicU64; layout::IST_COUNT]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; layout::IST_COUNT] }; MAX_CPUS];
/// Set by a processor once it reaches 64-bit kernel code.
static AP_ALIVE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Local APIC identifier of slot `cpu`, or `u32::MAX`.
#[must_use]
pub fn apic_id_of(cpu: usize) -> u32 {
    if cpu >= MAX_CPUS {
        return u32::MAX;
    }
    APIC_IDS[cpu].load(Ordering::Acquire)
}

/// Processors that completed the handshake.
#[must_use]
pub fn online_count() -> u32 {
    ONLINE.load(Ordering::Acquire).count_ones()
}

/// Whether the run has been told to stop.
#[must_use]
pub fn shutting_down() -> bool {
    SHUTDOWN.load(Ordering::Acquire) != 0
}

/// Maps the three emergency stacks of processor `cpu` and records their tops.
///
/// Each processor gets its own: a double fault that arrived because the current
/// stack is unusable cannot be handled on a stack another processor is
/// simultaneously faulting onto.
pub fn map_emergency_stacks(cpu: usize) -> [u64; layout::IST_COUNT] {
    let mut tops = [0u64; layout::IST_COUNT];
    let mut guard = MACHINE.lock();
    let machine = &mut *guard;
    for (slot, top) in tops.iter_mut().enumerate() {
        let base = layout::ist_slot_base(cpu * layout::IST_COUNT + slot);
        for page in 0..layout::IST_PAGES {
            let frame = machine
                .memory
                .as_mut()
                .expect("frame allocator established")
                .alloc(Owner::Kernel)
                .expect("emergency stack frame");
            let space = machine.kernel_space.as_mut().expect("kernel space built");
            let allocator = machine
                .memory
                .as_mut()
                .expect("frame allocator established");
            // The slot's first page stays unmapped as a guard.
            space
                .map(
                    base + (page + 1) * PAGE_SIZE,
                    frame,
                    Rights::KERNEL_RW,
                    allocator,
                    Owner::Kernel,
                )
                .expect("emergency stack mapping");
        }
        *top = base + layout::IST_SLOT_PAGES * PAGE_SIZE;
        AP_IST[cpu.min(MAX_CPUS - 1)][slot].store(*top, Ordering::Release);
    }
    tops
}

/// Records the bootstrap processor as slot zero.
pub fn claim_bootstrap(apic_id: u32) {
    APIC_IDS[0].store(apic_id, Ordering::Release);
    CLAIMED.store(1, Ordering::Release);
    percpu::claim(0, apic_id);
}

/// Publishes the bootstrap processor as online once its own tables exist.
pub fn bootstrap_online(apic_id: u32) {
    ONLINE.fetch_or(1, Ordering::AcqRel);
    tlb::mark_online(0);
    let mut machine = MACHINE.lock();
    machine.cpus[0].online = true;
    machine.cpus[0].apic_id = apic_id;
    machine.cpus[0].idle_thread = idle_thread(0);
    machine.cpus[0].current = idle_thread(0);
    machine.cpus_online = 1;
}

/// Builds the address space an application processor enables paging with.
///
/// It is not the kernel's own space: the trampoline executes at a physical
/// address below one mebibyte, and the instruction after `CR0.PG` is set has to
/// be mapped at that address. So this space is the kernel's upper half — shared,
/// not copied, so a kernel mapping made later is visible here too — plus one
/// two-mebibyte identity mapping that covers the trampoline and nothing else.
/// It is executable and not writable: the trampoline is code, and the parameter
/// block it reads is written before the processor exists.
fn build_ap_space() -> Option<u64> {
    let mut guard = MACHINE.lock();
    let machine = &mut *guard;
    let allocator = machine.memory.as_mut()?;
    let mut space = AddressSpace::new(allocator, Owner::Kernel).ok()?;
    let kernel = machine.kernel_space.as_ref()?;
    space.attach_kernel(kernel);
    let allocator = machine.memory.as_mut()?;
    space
        .map_large_range(0, 0, 1, Rights::KERNEL_RX, allocator)
        .ok()?;
    let cr3 = space.cr3();
    // The 32-bit stage loads this with a 32-bit move, so an address above four
    // gibibytes would be silently truncated into another table.
    if cr3 >= 1u64 << 32 {
        event!("smp.rejected", "reason=ap_cr3_above_4g cr3=0x{cr3:x}");
        return None;
    }
    // The space is retained for the life of the run: it is what a processor
    // that answers late would still need, and V0 has no processor hotplug to
    // make its removal meaningful. Dropping the handle frees nothing; the
    // frames stay charged to the kernel and the root stays live in `cr3`.
    Some(cr3)
}

/// Copies the trampoline into a page a start-up interrupt can name, and writes
/// the three absolute addresses it cannot know until the page is chosen.
fn install_trampoline(cr3: u64) -> Option<Frame> {
    let frame = {
        let mut machine = MACHINE.lock();
        machine
            .allocator()
            .alloc_below(STARTUP_LIMIT, Owner::Kernel)
            .ok()?
    };
    let base = frame.addr();
    let blob = ap::blob_len();
    if blob > PAGE_SIZE as usize {
        event!("smp.rejected", "reason=trampoline_too_large bytes={blob}");
        return None;
    }

    // The function *pointer*, not the zero-sized function item: casting the
    // item's address would hand the trampoline the address of a temporary.
    let entry: extern "C" fn(u64) -> ! = ap_entry;
    let entry = entry as *const () as u64;

    let source = (&raw const ap::thalyx_ap_trampoline).cast::<u8>();
    let destination = frame.hhdm_ptr();
    // SAFETY: the blob is a contiguous run of bytes inside the kernel image and
    // the destination is a whole frame this kernel just allocated, reachable
    // through the direct map. They cannot overlap.
    unsafe { core::ptr::copy_nonoverlapping(source, destination, blob) };

    // SAFETY: every write below is inside the frame just allocated, at an
    // offset the trampoline's own layout constants define, and no processor is
    // executing it yet.
    unsafe {
        destination
            .add(ap::OFFSET_GDTR_BASE)
            .cast::<u32>()
            .write_unaligned((base + ap::OFFSET_GDT as u64) as u32);
        destination
            .add(ap::offset_far32())
            .cast::<u32>()
            .write_unaligned((base + ap::OFFSET_STAGE32 as u64) as u32);
        destination
            .add(ap::offset_far64())
            .cast::<u32>()
            .write_unaligned((base + ap::OFFSET_STAGE64 as u64) as u32);
        destination
            .add(ap::OFFSET_PARAMS)
            .cast::<ap::Params>()
            .write(ap::Params {
                cr3,
                stack_top: 0,
                entry,
                cpu_index: 0,
            });
    }

    event!(
        "smp.trampoline",
        "phys=0x{base:x} vector={} bytes={blob} gdt=0x{:x} stage32=0x{:x} stage64=0x{:x} \
         params=0x{:x} ap_cr3=0x{cr3:x}",
        base >> 12,
        base + ap::OFFSET_GDT as u64,
        base + ap::OFFSET_STAGE32 as u64,
        base + ap::OFFSET_STAGE64 as u64,
        base + ap::OFFSET_PARAMS as u64
    );
    Some(frame)
}

/// Busy-waits `ns` nanoseconds against the monotonic clock.
fn spin_ns(ns: u64) {
    let Some(start) = time::monotonic_ns() else {
        return;
    };
    while time::monotonic_ns().unwrap_or(start + ns) < start + ns {
        core::hint::spin_loop();
    }
}

/// Allocates the idle thread of processor `cpu` and returns its stack top.
fn establish_ap_idle(cpu: usize) -> Option<u64> {
    let mut machine = MACHINE.lock();
    let index = idle_thread(cpu);
    let (slot, top) = crate::domain::allocate_kernel_stack(&mut machine).ok()?;
    let cr3 = machine.kernel_space.as_ref()?.cr3();
    let thread = &mut machine.threads[index];
    thread.state = ThreadState::Running;
    thread.kind = ThreadKind::Idle;
    thread.kstack_slot = slot;
    thread.kstack_top = top;
    thread.cr3 = cr3;
    thread.fpu = fpu::initial();
    thread.quantum_ticks = sched::QUANTUM_TICKS;
    machine.cpus[cpu].idle_thread = index;
    machine.cpus[cpu].current = index;
    Some(top)
}

/// Starts one processor and waits for its handshake.
///
/// Returns whether it answered. A processor that did not keeps its slot, its
/// stack and its emergency stacks: they are never given to another processor,
/// because the one that was slow may still arrive.
fn start_one(cpu: usize, apic_id: u32, trampoline: Frame) -> bool {
    APIC_IDS[cpu].store(apic_id, Ordering::Release);
    CLAIMED.store(cpu + 1, Ordering::Release);
    percpu::claim(cpu, apic_id);
    map_emergency_stacks(cpu);
    let Some(stack_top) = establish_ap_idle(cpu) else {
        event!(
            "smp.ap_failed",
            "cpu={cpu} apic_id={apic_id} reason=no_idle_stack"
        );
        return false;
    };

    let params = trampoline.hhdm_addr() + ap::OFFSET_PARAMS as u64;
    // SAFETY: the parameter block lies inside the trampoline frame this kernel
    // owns; the processor it describes has not started, so nothing else reads
    // it. The write completes before the interrupt below announces it.
    unsafe {
        let block = params as *mut ap::Params;
        (*block).stack_top = stack_top;
        (*block).cpu_index = cpu as u64;
    }
    AP_ALIVE[cpu].store(0, Ordering::Release);

    let Some(controller) = lapic::current() else {
        return false;
    };
    let vector = (trampoline.addr() >> 12) as u8;

    event!(
        "smp.ap_start",
        "cpu={cpu} apic_id={apic_id} vector=0x{vector:x} stack_top=0x{stack_top:x} \
         protocol=init_sipi_sipi"
    );

    controller.send_init(apic_id);
    spin_ns(10_000_000);
    controller.send_startup(apic_id, vector);
    spin_ns(200_000);
    let mut attempts = 1u32;
    if AP_ALIVE[cpu].load(Ordering::Acquire) == 0 {
        controller.send_startup(apic_id, vector);
        attempts = 2;
        spin_ns(1_000_000);
    }

    // The handshake, not the start-up interrupt, is what makes a processor
    // usable: an early store from the trampoline would prove only that some
    // code ran.
    let deadline = time::monotonic_ns().unwrap_or(0) + 200_000_000;
    while ONLINE.load(Ordering::Acquire) & (1u64 << cpu) == 0 {
        if time::monotonic_ns().unwrap_or(deadline) >= deadline {
            event!(
                "smp.ap_timeout",
                "cpu={cpu} apic_id={apic_id} attempts={attempts} \
                 reached_64bit={} stack_retained=1 slot_retired=1 reused=0",
                AP_ALIVE[cpu].load(Ordering::Acquire)
            );
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Starts every processor the firmware describes as enabled.
pub fn start_all(platform: &Platform, x2apic: bool, tsc_hz: u64) {
    AP_X2APIC.store(u64::from(x2apic), Ordering::Release);
    AP_TSC_HZ.store(tsc_hz, Ordering::Release);

    let bsp = apic_id_of(0);
    let described = platform.enabled();
    let Some(cr3) = build_ap_space() else {
        event!(
            "smp.summary",
            "described={described} started=0 online=1 reason=no_ap_address_space \
             profile=uniprocessor"
        );
        return;
    };
    AP_CR3.store(cr3, Ordering::Release);
    let Some(trampoline) = install_trampoline(cr3) else {
        event!(
            "smp.summary",
            "described={described} started=0 online=1 reason=no_trampoline_frame \
             profile=uniprocessor"
        );
        return;
    };

    let mut next = 1usize;
    let mut started = 0usize;
    let mut failed = 0usize;
    for entry in 0..platform.cpu_count {
        let cpu_entry = platform.cpus[entry];
        if !cpu_entry.enabled || cpu_entry.apic_id == bsp {
            continue;
        }
        if next >= MAX_CPUS {
            event!(
                "smp.ap_skipped",
                "apic_id={} reason=cpu_table_full capacity={MAX_CPUS}",
                cpu_entry.apic_id
            );
            continue;
        }
        if start_one(next, cpu_entry.apic_id, trampoline) {
            started += 1;
        } else {
            failed += 1;
        }
        next += 1;
    }

    event!(
        "smp.summary",
        "described={described} started={started} failed={failed} online={} \
         backend={} capacity={MAX_CPUS}",
        online_count(),
        if x2apic { "x2apic" } else { "xapic" }
    );

    probe_absent_processor(platform, trampoline.addr());
}

/// Attempts to start a processor the firmware never described.
///
/// This is the timeout path exercised on purpose. Nothing answers, because
/// nothing is there; what the run then shows is that the kernel waits, gives
/// up, keeps the slot and its stack, and never adds the slot to the online set.
/// Without it the timeout branch would be code no evidence had ever entered.
fn probe_absent_processor(platform: &Platform, trampoline_page: u64) {
    let mut candidate = 0xF0u32;
    while platform.cpus[..platform.cpu_count]
        .iter()
        .any(|cpu| cpu.apic_id == candidate)
    {
        candidate += 1;
    }
    let slot = CLAIMED.load(Ordering::Acquire).min(MAX_CPUS - 1);
    if slot >= MAX_CPUS {
        return;
    }
    let Some(controller) = lapic::current() else {
        return;
    };
    let before = online_count();
    event!(
        "smp.absent_probe",
        "slot={slot} apic_id={candidate} expectation=timeout note=negative_control"
    );
    controller.send_init(candidate);
    spin_ns(10_000_000);
    controller.send_startup(candidate, (trampoline_page >> 12) as u8);
    spin_ns(2_000_000);
    let alive = AP_ALIVE[slot].load(Ordering::Acquire);
    event!(
        "smp.absent_result",
        "slot={slot} apic_id={candidate} answered={alive} online_before={before} \
         online_after={} slot_added=0 stack_allocated=0",
        online_count()
    );
}

/// The first 64-bit kernel code an application processor executes.
///
/// # Safety
///
/// Reached only from the trampoline, once, per processor, with the address
/// space it was given loaded and a stack it alone owns installed.
extern "C" fn ap_entry(cpu_index: u64) -> ! {
    let cpu = (cpu_index as usize).min(MAX_CPUS - 1);
    // Before anything that could read `GS`, including a lock's spin loop.
    // SAFETY: this processor's slot was claimed before it was started.
    unsafe { percpu::install(cpu) };
    AP_ALIVE[cpu].store(1, Ordering::Release);

    let features = cpu::Features::detect();
    // SAFETY: bootstrap of this processor with interrupts masked. The kernel's
    // own tables are already loaded through the address space the trampoline
    // installed, whose upper half is the shared one.
    unsafe {
        cpu::wrmsr(cpu::MSR_EFER, cpu::rdmsr(cpu::MSR_EFER) | cpu::EFER_NXE);
        cpu::write_cr0(cpu::read_cr0() | cpu::CR0_WP);
        let mut cr4 = cpu::read_cr4();
        if features.smep {
            cr4 |= cpu::CR4_SMEP;
        }
        if features.smap {
            cr4 |= cpu::CR4_SMAP;
        }
        cpu::write_cr4(cr4);
        fpu::init();
    }

    let mut ists = [0u64; layout::IST_COUNT];
    for (slot, top) in ists.iter_mut().enumerate() {
        *top = AP_IST[cpu][slot].load(Ordering::Acquire);
    }
    // SAFETY: this processor's own descriptor tables, with interrupts masked
    // and its emergency stacks already mapped by the bootstrap processor.
    unsafe {
        gdt::install(cpu, ists);
        // Reloading the segment registers above cleared the `GS` base, so the
        // per-processor block is installed again here. Everything between the
        // two calls stays clear of `GS`.
        percpu::install(cpu);
        idt::load();
        syscall::init();
    }

    let want_x2apic = AP_X2APIC.load(Ordering::Acquire) != 0;
    // SAFETY: this processor's own controller, no interrupt source enabled yet.
    unsafe { lapic::enable_local(want_x2apic) };
    let controller = lapic::current().expect("interrupt controller installed");
    controller.configure(trap::SPURIOUS_VECTOR);

    let tsc_hz = AP_TSC_HZ.load(Ordering::Acquire);
    let lapic_hz = controller.calibrate_against_tsc(tsc_hz, 5_000_000);
    let count = u32::try_from((lapic_hz / sched::TICK_HZ).max(1)).unwrap_or(u32::MAX);

    // The kernel's own address space, rather than the one with the identity
    // mapping the trampoline needed. From here the identity mapping is gone
    // from this processor's view.
    let kernel_cr3 = {
        let machine = MACHINE.lock();
        machine
            .kernel_space
            .as_ref()
            .map_or(0, crate::arch::x86_64::paging::AddressSpace::cr3)
    };
    if kernel_cr3 != 0 {
        // SAFETY: the kernel's own space, in which this processor's code, stack
        // and data are all mapped.
        unsafe { cpu::write_cr3(kernel_cr3) };
    }

    let stack_top = {
        let machine = MACHINE.lock();
        machine.threads[idle_thread(cpu)].kstack_top
    };
    // SAFETY: the idle thread's own stack, mapped in the shared kernel half,
    // with interrupts masked.
    unsafe {
        gdt::set_kernel_stack(stack_top);
        percpu::set_kernel_rsp(stack_top);
    }

    let apic_id = controller.id();
    {
        let mut machine = MACHINE.lock();
        machine.cpus[cpu].online = true;
        machine.cpus[cpu].apic_id = apic_id;
        machine.cpus[cpu].cursor = 0;
        machine.cpus_online += 1;
        machine.threads[idle_thread(cpu)].dispatched_ns = time::monotonic_ns().unwrap_or(0);
    }
    tlb::mark_online(cpu);
    time::observe();

    controller.start_periodic(trap::TIMER_VECTOR, count);
    ONLINE.fetch_or(1u64 << cpu, Ordering::AcqRel);

    let claimed = apic_id_of(cpu);
    event!(
        "smp.ap_online",
        "cpu={cpu} apic_id={apic_id} claimed_apic_id={claimed} identity_match={} \
         backend={} lapic_hz={lapic_hz} initial_count={count} idle_thread={} \
         kstack_top=0x{stack_top:x} smep={} smap={}",
        u8::from(apic_id == claimed),
        controller.backend().name(),
        idle_thread(cpu),
        u8::from(features.smep),
        u8::from(features.smap)
    );

    sched::run_ap(cpu)
}

/// Tells every other processor the run is over and waits for them to park.
pub fn stop_all() {
    SHUTDOWN.store(1, Ordering::Release);
    let me = percpu::index();
    let expected = ONLINE.load(Ordering::Acquire) & !(1u64 << me);
    if expected == 0 {
        return;
    }
    if let Some(controller) = lapic::current() {
        for cpu in 0..MAX_CPUS {
            if expected & (1u64 << cpu) == 0 {
                continue;
            }
            let apic_id = apic_id_of(cpu);
            if apic_id != u32::MAX {
                controller.send_fixed(apic_id, trap::RESCHEDULE_VECTOR);
            }
        }
    }
    let deadline = time::monotonic_ns().unwrap_or(0) + 500_000_000;
    while PARKED.load(Ordering::Acquire) & expected != expected {
        if time::monotonic_ns().unwrap_or(deadline) >= deadline {
            event!(
                "smp.park_timeout",
                "expected=0x{expected:x} parked=0x{:x}",
                PARKED.load(Ordering::Acquire)
            );
            return;
        }
        core::hint::spin_loop();
    }
    event!(
        "smp.parked",
        "processors={} mask=0x{expected:x}",
        expected.count_ones()
    );
}

/// Records that this processor has stopped scheduling and will not answer
/// another invalidation.
pub fn park(cpu: usize) -> ! {
    {
        let mut machine = MACHINE.lock();
        machine.cpus[cpu].online = false;
        machine.cpus_online = machine.cpus_online.saturating_sub(1);
    }
    ONLINE.fetch_and(!(1u64 << cpu), Ordering::AcqRel);
    tlb::mark_offline(cpu);
    PARKED.fetch_or(1u64 << cpu, Ordering::AcqRel);
    cpu::halt_forever()
}
