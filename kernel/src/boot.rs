//! Bootstrap.
//!
//! The order below is not incidental. Each step establishes something the next
//! one depends on, and the sequence is the one the platform contract states:
//! validate the hand-off, refuse a machine that cannot support the profile,
//! take ownership of memory, install the kernel's own tables, install the
//! descriptor tables, enable protection, establish a clock, then and only then
//! build the first domain and enter ring 3.

use thalyx_boot_protocol::{
    BOOTINFO_MAGIC, BOOTINFO_VERSION_MAJOR, BootInfo, BootModule, HHDM_BASE, KERNEL_IMAGE_BASE,
    MAX_MEMORY_REGIONS, MAX_MODULES, MemoryRegion, PAGE_SIZE, module_flags, module_kind,
    region_kind,
};

use crate::arch::x86_64::serial::{COM1, Uart};
use crate::arch::x86_64::{cpu, fpu, gdt, idt, lapic, paging::AddressSpace, pic, syscall, trap};
use crate::event;
use crate::harness;
use crate::layout;
use crate::mm::frame::FrameAllocator;
use crate::mm::{Frame, Owner, Rights};
use crate::state::{IDLE_THREAD, MACHINE, ThreadKind, ThreadState};
use crate::{domain, sched, time};

unsafe extern "C" {
    static __thalyx_text_start: u8;
    static __thalyx_text_end: u8;
    static __thalyx_rodata_start: u8;
    static __thalyx_rodata_end: u8;
    static __thalyx_data_start: u8;
    static __thalyx_data_end: u8;
}

const EMPTY_REGION: MemoryRegion = MemoryRegion {
    base: 0,
    pages: 0,
    kind: 0,
    flags: 0,
};
const EMPTY_MODULE: BootModule = BootModule {
    name: [0; 32],
    phys_base: 0,
    length: 0,
    kind: 0,
    flags: 0,
};

// The hand-off block is reclaimed once the run is under way, so the map and the
// module directory are copied into the kernel image first. These are written
// once, during bootstrap, before any other context exists.
static mut REGIONS: [MemoryRegion; MAX_MEMORY_REGIONS] = [EMPTY_REGION; MAX_MEMORY_REGIONS];
static mut REGION_COUNT: usize = 0;
static mut MODULES: [BootModule; MAX_MODULES] = [EMPTY_MODULE; MAX_MODULES];
static mut MODULE_COUNT: usize = 0;

fn regions() -> &'static [MemoryRegion] {
    // SAFETY: written once during bootstrap before any other context exists,
    // read-only afterwards on a uniprocessor machine.
    unsafe {
        core::slice::from_raw_parts((&raw const REGIONS).cast::<MemoryRegion>(), REGION_COUNT)
    }
}

fn modules() -> &'static [BootModule] {
    // SAFETY: as `regions`.
    unsafe { core::slice::from_raw_parts((&raw const MODULES).cast::<BootModule>(), MODULE_COUNT) }
}

/// Entry point. Called by the loader with the physical address of the hand-off
/// structure in RDI, on a stack the loader mapped through the direct map.
///
/// # Safety
///
/// Only the loader may call this, exactly once, having established the state
/// described in `boot/protocol`: a direct map at [`HHDM_BASE`], the kernel image
/// mapped at its link addresses, a valid stack, and interrupts masked.
pub unsafe fn start(bootinfo_phys: u64) -> ! {
    let uart = Uart::new(COM1);
    // SAFETY: the loader has already finished with the port and handed it over.
    unsafe { uart.init() };

    // SAFETY: the loader placed the structure in reserved memory covered by the
    // direct map, which the caller guarantees is installed. `BootInfo` is a
    // plain `repr(C)` value with no invalid bit patterns, so the read is
    // well-defined even if the contents turn out to be wrong; the validation
    // below decides whether they can be used.
    let info: BootInfo = unsafe { core::ptr::read((HHDM_BASE + bootinfo_phys) as *const BootInfo) };

    crate::diag::init(uart, info.loader_event_count);

    event!(
        "kernel.entry",
        "bootinfo_phys=0x{bootinfo_phys:x} magic_ok={} version={}.{} \
         image_phys=0x{:x} image_virt=0x{:x}",
        u8::from(info.magic == BOOTINFO_MAGIC),
        info.version_major,
        info.version_minor,
        info.kernel_phys_base,
        info.kernel_virt_base
    );

    if let Err(reason) = validate(&info) {
        event!("boot.rejected", "reason={reason}");
        harness::finish(harness::STATUS_PANIC);
    }

    let usable_pages: u64 = 0;
    let _ = usable_pages;

    // SAFETY: the hand-off arrays live in reserved memory covered by the direct
    // map, their element sizes were validated above, and the entry counts are
    // within the fixed capacity of the destinations.
    unsafe {
        let source = (HHDM_BASE + info.memory_map_phys) as *const MemoryRegion;
        let count = info.memory_map_entries as usize;
        core::ptr::copy_nonoverlapping(source, (&raw mut REGIONS).cast::<MemoryRegion>(), count);
        REGION_COUNT = count;

        let source = (HHDM_BASE + info.modules_phys) as *const BootModule;
        let count = info.modules_count as usize;
        core::ptr::copy_nonoverlapping(source, (&raw mut MODULES).cast::<BootModule>(), count);
        MODULE_COUNT = count;
    }

    report_memory_map();

    let features = cpu::Features::detect();
    event!(
        "cpu.features",
        "long_mode={} nx={} syscall={} fxsr={} sse={} sse2={} apic={} x2apic={} pge={} pat={} \
         page1gb={} smep={} smap={} invariant_tsc={}",
        u8::from(features.long_mode),
        u8::from(features.nx),
        u8::from(features.syscall),
        u8::from(features.fxsr),
        u8::from(features.sse),
        u8::from(features.sse2),
        u8::from(features.apic),
        u8::from(features.x2apic),
        u8::from(features.pge),
        u8::from(features.pat),
        u8::from(features.page1gb),
        u8::from(features.smep),
        u8::from(features.smap),
        u8::from(features.invariant_tsc)
    );

    let missing = features.missing_mandatory();
    if !missing.is_empty() {
        event!("cpu.profile", "name=v0 status=rejected missing={missing}");
        harness::finish(harness::STATUS_PANIC);
    }

    // SAFETY: the feature gate above confirmed NX and the MSR interface. CR0.WP
    // must be set before any supervisor write could bypass a read-only mapping,
    // and the kernel installs read-only mappings of its own image below.
    unsafe {
        cpu::wrmsr(cpu::MSR_EFER, cpu::rdmsr(cpu::MSR_EFER) | cpu::EFER_NXE);
        cpu::write_cr0(cpu::read_cr0() | cpu::CR0_WP);
    }

    // SAFETY: bootstrap, interrupts masked, no interrupt source enabled yet.
    let lapic_phys = unsafe { lapic::enable_xapic() };

    // SAFETY: `regions` is the loader's classification and the direct map covers
    // every usable frame it names, which `validate` checked against
    // `hhdm_pages`. Nothing else owns that memory yet.
    let allocator = unsafe { FrameAllocator::new(regions(), info.hhdm_pages as usize) };
    let Some(allocator) = allocator else {
        event!("boot.rejected", "reason=no_region_for_frame_bitmap");
        harness::finish(harness::STATUS_PANIC);
    };
    event!(
        "mm.frames",
        "tracked={} usable={} free={} bitmap_owner=kernel",
        allocator.frames(),
        allocator.usable(),
        allocator.free_frames()
    );

    {
        let mut machine = MACHINE.lock();
        machine.memory = Some(allocator);
    }

    build_kernel_space(&info, lapic_phys);

    // SAFETY: the kernel's own tables are installed and every mapping the
    // kernel executes from, and its stack, exist in them.
    unsafe {
        let machine = MACHINE.lock();
        let space = machine
            .kernel_space
            .as_ref()
            .expect("kernel space built above");
        let cr3 = space.cr3();
        drop(machine);
        cpu::write_cr3(cr3);
        event!("mm.paging_installed", "cr3=0x{cr3:x} owner=kernel");
    }

    let ist_tops = map_emergency_stacks();

    // SAFETY: bootstrap path, interrupts masked, emergency stacks mapped.
    unsafe {
        gdt::install(ist_tops);
        idt::install();
    }
    event!(
        "cpu.gdt_installed",
        "kernel_cs=0x{:x} kernel_ds=0x{:x} user_cs=0x{:x} user_ds=0x{:x} tss=0x{:x}",
        gdt::KERNEL_CODE,
        gdt::KERNEL_DATA,
        gdt::USER_CODE,
        gdt::USER_DATA,
        gdt::TSS_SELECTOR
    );
    event!(
        "cpu.tss_installed",
        "ist1=0x{:x} ist2=0x{:x} ist3=0x{:x} double_fault_vector={} nmi_vector={} \
         machine_check_vector={}",
        gdt::ist_stack(0),
        gdt::ist_stack(1),
        gdt::ist_stack(2),
        idt::VECTOR_DOUBLE_FAULT,
        idt::VECTOR_NMI,
        idt::VECTOR_MACHINE_CHECK
    );
    event!(
        "cpu.idt_installed",
        "entries=256 gate=interrupt dpl=0 timer_vector=0x{:x} spurious_vector=0x{:x}",
        trap::TIMER_VECTOR,
        trap::SPURIOUS_VECTOR
    );

    // Supervisor execution and access prevention are enabled only now: before
    // the kernel's own tables were installed, a firmware mapping that marked
    // kernel memory user-accessible would have faulted immediately.
    let mut cr4 = cpu::read_cr4();
    if features.smep {
        cr4 |= cpu::CR4_SMEP;
    }
    if features.smap {
        cr4 |= cpu::CR4_SMAP;
    }
    // SAFETY: the bits are enabled only when CPUID advertised them, and the
    // kernel neither executes from nor reads user pages.
    unsafe { cpu::write_cr4(cr4) };
    event!(
        "cpu.profile",
        "name=v0-qemu status=accepted paging=4level nx=on wp=on smep={} smap={} \
         kernel_simd=off user_fp=x87+sse2 fp_switch=eager",
        if features.smep { "on" } else { "absent" },
        if features.smap { "on" } else { "absent" }
    );

    // SAFETY: the feature gate confirmed FXSR and SSE2.
    unsafe { fpu::init() };
    event!(
        "cpu.fpu_initialized",
        "cr0=0x{:x} cr4=0x{:x} mxcsr=0x{:x} area_bytes=512 policy=eager",
        cpu::read_cr0(),
        cpu::read_cr4(),
        fpu::DEFAULT_MXCSR
    );

    // SAFETY: the GDT is installed and the feature gate confirmed SYSCALL.
    unsafe { syscall::init() };
    event!(
        "cpu.syscall_installed",
        "lstar=0x{:x} star=0x{:x} fmask=0x{:x} return_path=iretq",
        syscall::lstar(),
        syscall::star_value(),
        syscall::SYSCALL_FLAG_MASK
    );

    // SAFETY: bootstrap, interrupts masked, no legacy interrupt is wanted.
    unsafe { pic::remap_and_mask() };

    start_timer(lapic_phys);
    establish_idle_thread(&info);
    let created = create_initial_domains();

    reclaim_boot_memory(&info);

    if created == 0 {
        event!("k1.terminal", "reason=no_domain_created");
        harness::finish(harness::STATUS_PANIC);
    }

    sched::run_until_idle();
    domain::reap_dead();

    summarize();
    harness::finish(harness::STATUS_COMPLETE)
}

fn validate(info: &BootInfo) -> Result<(), &'static str> {
    if info.magic != BOOTINFO_MAGIC {
        return Err("bad_magic");
    }
    if info.version_major != BOOTINFO_VERSION_MAJOR {
        return Err("incompatible_version");
    }
    if (info.header_len as usize) < core::mem::size_of::<BootInfo>() {
        return Err("short_header");
    }
    if info.hhdm_base != HHDM_BASE {
        return Err("direct_map_base_mismatch");
    }
    if info.hhdm_pages == 0 {
        return Err("empty_direct_map");
    }
    if info.memory_map_entry_size as usize != core::mem::size_of::<MemoryRegion>() {
        return Err("memory_map_entry_size");
    }
    if info.memory_map_entries == 0 || info.memory_map_entries as usize > MAX_MEMORY_REGIONS {
        return Err("memory_map_entry_count");
    }
    if info.modules_entry_size as usize != core::mem::size_of::<BootModule>() {
        return Err("module_entry_size");
    }
    if info.modules_count as usize > MAX_MODULES {
        return Err("module_count");
    }
    if info.kernel_virt_base != KERNEL_IMAGE_BASE {
        return Err("kernel_base_mismatch");
    }
    if info.boot_stack_pages == 0 {
        return Err("no_boot_stack");
    }
    Ok(())
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        region_kind::USABLE => "usable",
        region_kind::BOOT_RECLAIMABLE => "boot_reclaimable",
        region_kind::KERNEL_IMAGE => "kernel_image",
        region_kind::MODULES => "modules",
        region_kind::FIRMWARE_RUNTIME => "firmware_runtime",
        region_kind::ACPI_RECLAIMABLE => "acpi_reclaimable",
        region_kind::ACPI_NVS => "acpi_nvs",
        region_kind::RESERVED => "reserved",
        region_kind::UNUSABLE => "unusable",
        region_kind::MMIO => "mmio",
        _ => "unknown",
    }
}

fn report_memory_map() {
    let mut usable = 0u64;
    let mut reclaimable = 0u64;
    let mut firmware = 0u64;
    for region in regions() {
        match region.kind {
            region_kind::USABLE => usable += region.pages,
            region_kind::BOOT_RECLAIMABLE | region_kind::MODULES => reclaimable += region.pages,
            region_kind::FIRMWARE_RUNTIME | region_kind::ACPI_NVS | region_kind::RESERVED => {
                firmware += region.pages;
            }
            _ => {}
        }
    }
    event!(
        "boot.validated",
        "regions={} modules={} usable_pages={usable} reclaimable_pages={reclaimable} \
         firmware_pages={firmware}",
        regions().len(),
        modules().len()
    );
    for region in regions().iter().take(48) {
        event!(
            "mm.region",
            "base=0x{:x} pages={} kind={}",
            region.base,
            region.pages,
            kind_name(region.kind)
        );
    }
}

fn map_kernel_range(space: &mut AddressSpace, start: u64, end: u64, rights: Rights, delta: u64) {
    let mut guard = MACHINE.lock();
    let allocator = guard.memory.as_mut().expect("frame allocator established");
    let mut vaddr = start;
    while vaddr < end {
        let frame = Frame::containing(vaddr.wrapping_add(delta));
        space
            .map(vaddr, frame, rights, allocator, Owner::Kernel)
            .expect("kernel image mapping");
        vaddr += PAGE_SIZE;
    }
}

fn build_kernel_space(info: &BootInfo, lapic_phys: u64) {
    let mut space = {
        let mut guard = MACHINE.lock();
        let allocator = guard.memory.as_mut().expect("frame allocator established");
        let mut space = AddressSpace::new(allocator, Owner::Kernel).expect("kernel PML4");
        space
            .create_kernel_slots(allocator)
            .expect("kernel PML4 slots");
        // The direct map uses 2 MiB entries: it maps memory the kernel already
        // owns, so there is nothing to split later and no per-page right to
        // express.
        let large_pages = info.hhdm_pages.div_ceil(512);
        space
            .map_large_range(HHDM_BASE, 0, large_pages, Rights::KERNEL_RW, allocator)
            .expect("direct map");
        space
            .map(
                layout::LAPIC_VADDR,
                Frame::containing(lapic_phys),
                Rights::KERNEL_DEVICE,
                allocator,
                Owner::Kernel,
            )
            .expect("local APIC mapping");
        space
    };

    let delta = info.kernel_phys_base.wrapping_sub(info.kernel_virt_base);
    let text_start = (&raw const __thalyx_text_start) as u64;
    let text_end = (&raw const __thalyx_text_end) as u64;
    let rodata_start = (&raw const __thalyx_rodata_start) as u64;
    let rodata_end = (&raw const __thalyx_rodata_end) as u64;
    let data_start = (&raw const __thalyx_data_start) as u64;
    let data_end = (&raw const __thalyx_data_end) as u64;

    map_kernel_range(&mut space, text_start, text_end, Rights::KERNEL_RX, delta);
    map_kernel_range(
        &mut space,
        rodata_start,
        rodata_end,
        Rights::KERNEL_RO,
        delta,
    );
    map_kernel_range(&mut space, data_start, data_end, Rights::KERNEL_RW, delta);

    event!(
        "mm.kernel_image_mapped",
        "text=0x{text_start:x}..0x{text_end:x}:rx rodata=0x{rodata_start:x}..0x{rodata_end:x}:r \
         data=0x{data_start:x}..0x{data_end:x}:rw phys_delta=0x{delta:x} \
         direct_map_pages={} lapic=0x{:x}",
        info.hhdm_pages,
        layout::LAPIC_VADDR
    );

    let mut machine = MACHINE.lock();
    machine.kernel_space = Some(space);
}

fn map_emergency_stacks() -> [u64; layout::IST_COUNT] {
    let mut tops = [0u64; layout::IST_COUNT];
    let mut guard = MACHINE.lock();
    let machine = &mut *guard;
    for (slot, top) in tops.iter_mut().enumerate() {
        let base = layout::ist_slot_base(slot);
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
    }
    tops
}

fn start_timer(lapic_phys: u64) {
    lapic::install(layout::LAPIC_VADDR);
    let Some(controller) = lapic::current() else {
        event!("boot.rejected", "reason=no_local_apic");
        harness::finish(harness::STATUS_PANIC);
    };
    controller.configure(trap::SPURIOUS_VECTOR);

    // A 10 ms window against the PIT: long enough that the counter resolution
    // does not dominate, short enough that the 16-bit PIT counter cannot wrap.
    let window_ticks = (crate::arch::x86_64::pit::FREQUENCY / 100) as u16;
    // SAFETY: the legacy controllers are masked, the PIT belongs to the kernel
    // and interrupts are masked for the whole window.
    let calibration = unsafe { controller.calibrate(window_ticks) };

    if calibration.lapic_hz == 0 || calibration.tsc_hz == 0 {
        event!("boot.rejected", "reason=clock_calibration_failed");
        harness::finish(harness::STATUS_PANIC);
    }

    let features = cpu::Features::detect();
    time::init(calibration.tsc_hz, features.invariant_tsc);
    event!(
        "time.source",
        "kind={} hz={} invariant_tsc={} reference=pit window_ns={} \
         cross_core_monotonic=not_exercised_uniprocessor",
        time::source().name(),
        time::hz(),
        u8::from(features.invariant_tsc),
        calibration.window_ns
    );

    let count = calibration.lapic_hz / sched::TICK_HZ;
    let count = u32::try_from(count.max(1)).unwrap_or(u32::MAX);
    controller.start_periodic(trap::TIMER_VECTOR, count);
    event!(
        "timer.armed",
        "vector=0x{:x} mode=periodic tick_hz={} lapic_hz={} initial_count={count} \
         quantum_ns={} quantum_ticks={} apic_id={} apic_version=0x{:x} lapic_phys=0x{lapic_phys:x}",
        trap::TIMER_VECTOR,
        sched::TICK_HZ,
        calibration.lapic_hz,
        sched::QUANTUM_NS,
        sched::QUANTUM_TICKS,
        controller.id(),
        controller.version()
    );
}

fn establish_idle_thread(info: &BootInfo) {
    let stack_top = HHDM_BASE + info.boot_stack_phys + info.boot_stack_pages * PAGE_SIZE;
    let mut machine = MACHINE.lock();
    let cr3 = machine
        .kernel_space
        .as_ref()
        .expect("kernel space built")
        .cr3();
    let thread = &mut machine.threads[IDLE_THREAD];
    thread.state = ThreadState::Running;
    thread.kind = ThreadKind::Idle;
    thread.kstack_top = stack_top;
    thread.cr3 = cr3;
    thread.fpu = fpu::initial();
    thread.quantum_ticks = sched::QUANTUM_TICKS;
    thread.dispatched_ns = time::monotonic_ns().unwrap_or(0);
    machine.current = IDLE_THREAD;
    drop(machine);

    // SAFETY: bootstrap path with interrupts masked; the boot stack is mapped
    // through the direct map and is the stack this code is running on.
    unsafe {
        gdt::set_kernel_stack(stack_top);
        trap::set_syscall_stack(stack_top);
    }
    event!(
        "sched.idle_established",
        "thread={IDLE_THREAD} kstack_top=0x{stack_top:x} cr3=0x{cr3:x}"
    );
}

fn create_initial_domains() -> usize {
    let mut created = 0usize;
    for module in modules() {
        let name_len = module.name.iter().position(|byte| *byte == 0).unwrap_or(32);
        let name = core::str::from_utf8(&module.name[..name_len]).unwrap_or("?");
        let expect_reject = module.flags & module_flags::EXPECT_REJECT != 0;

        if module.kind != module_kind::USER_ELF {
            event!(
                "module.rejected",
                "name={name} reason=unsupported_kind kind={}",
                module.kind
            );
            MACHINE.lock().modules_rejected += 1;
            continue;
        }

        // SAFETY: the loader copied the module into reserved memory covered by
        // the direct map and recorded its exact length; the slice is read-only
        // and nothing else writes that memory.
        let image = unsafe {
            core::slice::from_raw_parts(
                (HHDM_BASE + module.phys_base) as *const u8,
                module.length as usize,
            )
        };

        match domain::create(name, image) {
            Ok(index) => {
                if expect_reject {
                    event!(
                        "module.accepted_unexpectedly",
                        "name={name} domain={index} note=module_marked_expect_reject"
                    );
                }
                report_domain(index, name);
                match domain::activate(index) {
                    Ok(()) => {
                        let (entry, segments, thread) = {
                            let machine = MACHINE.lock();
                            (
                                machine.domains[index].entry,
                                machine.domains[index].segments,
                                machine.domains[index].thread.unwrap_or(usize::MAX),
                            )
                        };
                        event!(
                            "domain.activated",
                            "domain={index} name={name} thread={thread} entry=0x{entry:x} \
                             segments={segments} state=runnable"
                        );
                        created += 1;
                    }
                    Err(refusal) => {
                        event!(
                            "domain.activation_refused",
                            "domain={index} name={name} reason={}",
                            refusal.name()
                        );
                    }
                }
            }
            Err(error) => {
                MACHINE.lock().modules_rejected += 1;
                event!(
                    "module.rejected",
                    "name={name} reason={} expected_reject={}",
                    error.name(),
                    u8::from(expect_reject)
                );
            }
        }
    }
    created
}

fn report_domain(index: usize, name: &str) {
    let machine = MACHINE.lock();
    let Some(space) = machine.domains[index].space.as_ref() else {
        return;
    };
    let entry = machine.domains[index].entry;
    let cr3 = space.cr3();
    let segments = machine.domains[index].segments;
    let image_pages = machine.domains[index].image_pages;
    let charged = {
        // The lock is already held; read through the same guard.
        drop(machine);
        let mut machine = MACHINE.lock();
        machine.allocator().charged(Owner::Domain(index as u16))
    };
    event!(
        "domain.created",
        "domain={index} name={name} entry=0x{entry:x} cr3=0x{cr3:x} segments={segments} \
         image_pages={image_pages} \
         stack_top=0x{:x} stack_pages={} guard_page=below charged_frames={charged} \
         state=building",
        layout::USER_STACK_TOP,
        layout::USER_STACK_PAGES
    );

    let machine = MACHINE.lock();
    let Some(space) = machine.domains[index].space.as_ref() else {
        return;
    };
    let stack_low = layout::USER_STACK_TOP - layout::USER_STACK_PAGES * PAGE_SIZE;
    let guard = stack_low - PAGE_SIZE;
    let guard_mapped = space.translate(guard).is_some();
    let null_mapped = space.translate(0).is_some();
    let entry_flags = space.translate(entry).map_or(0, |(_, flags)| flags);
    drop(machine);
    event!(
        "domain.protection",
        "domain={index} name={name} entry_flags=0x{entry_flags:x} entry_user={} entry_nx={} \
         guard_page=0x{guard:x} guard_mapped={} null_page_mapped={}",
        u8::from(entry_flags & (1 << 2) != 0),
        u8::from(entry_flags & (1 << 63) != 0),
        u8::from(guard_mapped),
        u8::from(null_mapped)
    );
}

fn reclaim_boot_memory(info: &BootInfo) {
    let stack_start = info.boot_stack_phys;
    let stack_end = info.boot_stack_phys + info.boot_stack_pages * PAGE_SIZE;

    let mut modules_frames = 0usize;
    let mut boot_frames = 0usize;
    let mut machine = MACHINE.lock();
    for region in regions() {
        let start = region.base;
        let end = region.base + region.pages * PAGE_SIZE;
        let overlaps_stack = start < stack_end && stack_start < end;
        match region.kind {
            region_kind::MODULES => {
                // SAFETY: every module's contents have been copied into frames
                // the domains own; nothing references this memory now.
                modules_frames += unsafe { machine.allocator().reclaim(start, region.pages) };
            }
            region_kind::BOOT_RECLAIMABLE if !overlaps_stack => {
                // SAFETY: the kernel installed its own page tables, so the
                // loader's tables are unreachable, and the hand-off structures
                // have been copied into the kernel image. The boot stack is
                // excluded because the idle thread is still standing on it.
                boot_frames += unsafe { machine.allocator().reclaim(start, region.pages) };
            }
            _ => {}
        }
    }
    machine.reclaimed_frames = modules_frames + boot_frames;
    let free = machine.allocator().free_frames();
    drop(machine);
    event!(
        "mm.boot_memory_reclaimed",
        "module_frames={modules_frames} loader_frames={boot_frames} \
         boot_stack_retained_pages={} free_frames={free}",
        info.boot_stack_pages
    );
}

fn summarize() {
    let machine = MACHINE.lock();
    let ticks = machine.ticks;
    let preemptions = machine.preemptions;
    let faults = machine.user_faults;
    let rejected = machine.modules_rejected;
    let records = machine.preempt_records;
    drop(machine);

    for index in 0..crate::state::MAX_DOMAINS {
        let machine = MACHINE.lock();
        let state = machine.domains[index].state;
        if state == crate::state::DomainState::Empty {
            continue;
        }
        let notes = machine.domains[index].notes;
        let reason = machine.domains[index]
            .exit_reason
            .map_or("none", crate::state::ExitReason::name);
        let charged = {
            drop(machine);
            let mut machine = MACHINE.lock();
            machine.allocator().charged(Owner::Domain(index as u16))
        };
        let name = domain::domain_name(index);
        event!(
            "k1.domain_summary",
            "domain={index} name={name} final_state={} exit_reason={reason} notes={notes} \
             charged_frames={charged}",
            state.name()
        );
    }

    let mut machine = MACHINE.lock();
    let free = machine.allocator().free_frames();
    let usable = machine.allocator().usable();
    let kernel_charged = machine.allocator().charged(Owner::Kernel);
    let reclaimed = machine.reclaimed_frames;
    drop(machine);

    event!(
        "k1.summary",
        "timer_ticks={ticks} preemptions={preemptions} preempt_records_emitted={records} \
         user_faults={faults} modules_rejected={rejected} kernel_charged_frames={kernel_charged} \
         free_frames={free} usable_frames_at_boot={usable} boot_frames_reclaimed={reclaimed} \
         plane=diagnostic coalesced={}",
        u8::from(preemptions > u64::from(records))
    );
    event!("k1.terminal", "reason=no_runnable_domain status=complete");
}
