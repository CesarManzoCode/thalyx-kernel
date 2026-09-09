//! Thalyx-Kernel UEFI loader.
//!
//! The loader is a separate binary for a separate target, with its own calling
//! convention and its own dependency on firmware services. The kernel inherits
//! none of that: everything crossing between them is the versioned structure in
//! `thalyx-boot-protocol`, and the loader's last act is a jump with one
//! argument.
//!
//! What the loader does, in order: read and validate the kernel image and the
//! initial package; place them in reserved memory; build bootstrap page tables
//! and a stack; reserve the hand-off block; leave boot services; classify the
//! firmware's final memory map into the categories the kernel's allocator
//! distinguishes; and transfer control.
//!
//! What it does not do: verify a signature. Development images are trusted
//! explicitly, and this is not called verified boot anywhere, because there is
//! no key, no policy and no rollback anchor behind it.

#![no_std]
#![no_main]

extern crate alloc;

mod elf;
mod package;
mod paging;
mod serial;

use core::fmt::Write;

use thalyx_boot_protocol::{
    BOOTINFO_MAGIC, BOOTINFO_VERSION_MAJOR, BOOTINFO_VERSION_MINOR, BootInfo, BootModule,
    HHDM_BASE, KERNEL_IMAGE_BASE, KERNEL_PATH, MAX_MEMORY_REGIONS, MAX_MODULES, MemoryRegion,
    PACKAGE_PATH, PAGE_SIZE, memory_type, region_kind,
};
use uefi::boot::{self, AllocateType};
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::{CString16, Status};

/// Pages reserved for the kernel's initial stack.
const BOOT_STACK_PAGES: u64 = 16;
/// Upper bound on the direct map. Beyond this the loader refuses rather than
/// building a hierarchy whose size it did not plan for.
const MAX_DIRECT_MAP_GIB: u64 = 64;

static mut SEQUENCE: u64 = 0;

fn next_sequence() -> u64 {
    // SAFETY: the loader is single-threaded and firmware does not re-enter it.
    unsafe {
        let value = SEQUENCE;
        SEQUENCE = value + 1;
        value
    }
}

/// Number of diagnostic records emitted so far, handed to the kernel so it
/// continues one sequence.
fn emitted() -> u64 {
    // SAFETY: as `next_sequence`.
    unsafe { SEQUENCE }
}

fn emit(name: &str, fields: core::fmt::Arguments<'_>) {
    let sequence = next_sequence();
    let mut port = serial::Port;
    let _ = write!(port, "THLX1 loader {sequence} - {name} {fields}\n");
}

macro_rules! event {
    ($name:expr) => { crate::emit($name, ::core::format_args!("")) };
    ($name:expr, $($fields:tt)*) => { crate::emit($name, ::core::format_args!($($fields)*)) };
}

/// Everything reserved before boot services are left.
struct Prepared {
    tables_root: u64,
    tables_phys: u64,
    tables_pages: u64,
    entry: u64,
    kernel_phys: u64,
    kernel_pages: u64,
    stack_phys: u64,
    stack_pages: u64,
    handoff_phys: u64,
    handoff_pages: u64,
    regions_phys: u64,
    modules_phys: u64,
    module_count: u32,
    hhdm_pages: u64,
    acpi_rsdp: u64,
}

#[uefi::entry]
fn main() -> Status {
    // SAFETY: the loader owns the port from here on. Firmware may also write to
    // it; the record prefix keeps the two distinguishable.
    unsafe { serial::init() };

    let revision = uefi::system::uefi_revision();
    event!(
        "loader.entry",
        "firmware_revision=0x{:x} uefi={}.{} image_base=0x{:x}",
        uefi::system::firmware_revision(),
        revision.major(),
        revision.minor(),
        KERNEL_IMAGE_BASE
    );

    match prepare() {
        Ok(prepared) => {
            // SAFETY: `prepare` reserved every structure the transfer refers to
            // with a lifetime that outlives `ExitBootServices`, and validated
            // that the page tables map the kernel image, the direct map and the
            // identity range the transfer executes in.
            unsafe { transfer(prepared) }
        }
        Err(reason) => {
            event!("loader.failed", "reason={reason}");
            Status::LOAD_ERROR
        }
    }
}

fn read_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    let name = CString16::try_from(path).map_err(|_| "path_not_representable")?;
    let filesystem =
        boot::get_image_file_system(boot::image_handle()).map_err(|_| "no_image_file_system")?;
    let mut filesystem = uefi::fs::FileSystem::new(filesystem);
    filesystem
        .read(uefi::fs::Path::new(&name))
        .map_err(|_| "file_read_failed")
}

fn allocate(kind: u32, pages: u64) -> Result<u64, &'static str> {
    let pointer = boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType(kind),
        usize::try_from(pages).map_err(|_| "allocation_too_large")?,
    )
    .map_err(|_| "allocation_failed")?;
    let address = pointer.as_ptr() as u64;
    // SAFETY: the firmware just reserved these pages for us, they are identity
    // mapped while boot services are active, and nothing else refers to them.
    unsafe { core::ptr::write_bytes(address as *mut u8, 0, (pages * PAGE_SIZE) as usize) };
    Ok(address)
}

fn prepare() -> Result<Prepared, &'static str> {
    let kernel_image = read_file(KERNEL_PATH)?;
    let package_image = read_file(PACKAGE_PATH)?;
    event!(
        "loader.images_read",
        "kernel_path={KERNEL_PATH} kernel_bytes={} package_path={PACKAGE_PATH} package_bytes={}",
        kernel_image.len(),
        package_image.len()
    );

    let kernel = elf::parse(&kernel_image).map_err(elf::Reject::name)?;
    event!(
        "loader.kernel_validated",
        "entry=0x{:x} base=0x{:x} end=0x{:x} segments={} pages={}",
        kernel.entry,
        kernel.base,
        kernel.end,
        kernel.segment_count,
        kernel.pages()
    );

    let directory = package::parse(&package_image).map_err(package::Reject::name)?;
    event!(
        "loader.package_validated",
        "entries={} bytes={}",
        directory.count,
        package_image.len()
    );

    // Place the kernel image as one contiguous physical block so the kernel can
    // recompute the physical address of any of its own virtual addresses with a
    // single delta, which is what it needs to rebuild its own mappings.
    let kernel_pages = kernel.pages();
    let kernel_phys = allocate(memory_type::KERNEL_IMAGE, kernel_pages)?;
    for segment in &kernel.segments[..kernel.segment_count] {
        let destination = kernel_phys + (segment.vaddr - kernel.base);
        // SAFETY: the destination lies inside the block just reserved, whose
        // size is `kernel.end - kernel.base`; the source range was validated
        // against the image length. Firmware memory and the new block do not
        // overlap because the block was freshly allocated.
        unsafe {
            core::ptr::copy_nonoverlapping(
                kernel_image.as_ptr().add(segment.file_offset),
                destination as *mut u8,
                segment.file_len,
            );
        }
        event!(
            "loader.kernel_segment",
            "vaddr=0x{:x} phys=0x{destination:x} file_bytes={} mem_bytes={} r={} w={} x={}",
            segment.vaddr,
            segment.file_len,
            segment.mem_len,
            u8::from(segment.read),
            u8::from(segment.write),
            u8::from(segment.execute)
        );
    }

    // Modules: one reserved block each, so the kernel can hand a domain builder
    // an exact byte range and reclaim the block afterwards.
    let mut module_bases = [0u64; MAX_MODULES];
    let mut module_pages = [0u64; MAX_MODULES];
    for (index, entry) in directory.entries[..directory.count].iter().enumerate() {
        let pages = (entry.length as u64).div_ceil(PAGE_SIZE);
        let base = allocate(memory_type::MODULES, pages)?;
        // SAFETY: the destination block was just reserved with room for
        // `entry.length` bytes and the source range was validated against the
        // package length.
        unsafe {
            core::ptr::copy_nonoverlapping(
                package_image.as_ptr().add(entry.offset),
                base as *mut u8,
                entry.length,
            );
        }
        module_bases[index] = base;
        module_pages[index] = pages;
    }

    let handoff_pages = handoff_layout().total_pages;
    let handoff_phys = allocate(memory_type::BOOT_RECLAIMABLE, handoff_pages)?;
    let stack_phys = allocate(memory_type::BOOT_RECLAIMABLE, BOOT_STACK_PAGES)?;

    // Write the module directory into the hand-off block now, while the
    // addresses are still known and boot services are still available.
    let layout = handoff_layout();
    for (index, entry) in directory.entries[..directory.count].iter().enumerate() {
        let module = BootModule {
            name: entry.name,
            phys_base: module_bases[index],
            length: entry.length as u64,
            kind: entry.kind,
            flags: entry.flags,
        };
        // SAFETY: the array lies inside the reserved hand-off block, `index` is
        // below `MAX_MODULES` because the package validator bounded the count,
        // and the destination is 8-byte aligned because the block is page
        // aligned and the element size is a multiple of eight.
        unsafe {
            ((handoff_phys + layout.modules_offset) as *mut BootModule)
                .add(index)
                .write(module);
        }
        let name_len = entry.name.iter().position(|byte| *byte == 0).unwrap_or(32);
        let name = core::str::from_utf8(&entry.name[..name_len]).unwrap_or("?");
        event!(
            "loader.module",
            "index={index} name={name} phys=0x{:x} bytes={} pages={} kind={} flags=0x{:x}",
            module_bases[index],
            entry.length,
            module_pages[index],
            entry.kind,
            entry.flags
        );
    }

    // The extent of the direct map is decided from the firmware's map, rounded
    // to whole gibibytes so the tables need only 2 MiB entries under one page
    // directory per gibibyte.
    let map = boot::memory_map(MemoryType::LOADER_DATA).map_err(|_| "memory_map_failed")?;
    let mut highest_ram = 0u64;
    let mut highest_any = 0u64;
    for descriptor in map.entries() {
        let end = descriptor.phys_start + descriptor.page_count * PAGE_SIZE;
        highest_any = highest_any.max(end);
        // The direct map covers RAM, identified by an allow list rather than by
        // excluding the device types: firmware also reports address-space
        // windows as reserved, and a window at the top of a 40-bit address
        // space would size the map for memory that does not exist.
        if is_ram(descriptor.ty) {
            highest_ram = highest_ram.max(end);
        }
    }
    let entries_seen = map.entries().len();
    drop(map);

    const GIB: u64 = 1024 * 1024 * 1024;
    let gibibytes = highest_ram.div_ceil(GIB).max(1);
    event!(
        "loader.memory_extent",
        "highest_ram=0x{highest_ram:x} highest_any=0x{highest_any:x} direct_map_gib={gibibytes} \
         map_entries={entries_seen}"
    );
    if gibibytes > MAX_DIRECT_MAP_GIB {
        return Err("physical_memory_beyond_direct_map_bound");
    }
    let map_limit = gibibytes * GIB;
    let hhdm_pages = map_limit / PAGE_SIZE;

    if entries_seen + 32 > MAX_MEMORY_REGIONS {
        return Err("memory_map_too_large");
    }

    // One page directory per gibibyte for each of the two large-page ranges,
    // one page-directory-pointer table for each of the three ranges, one page
    // directory and the page tables for the kernel image, the PML4, and a small
    // margin. Sized from the map rather than fixed, so a larger machine does
    // not silently run the pool dry.
    let kernel_page_tables = kernel_pages.div_ceil(512) + 1;
    let table_pool_pages = (1 + 3 + 2 * gibibytes + 1 + kernel_page_tables + 4) as usize;
    let tables_phys = allocate(memory_type::BOOT_RECLAIMABLE, table_pool_pages as u64)?;

    // SAFETY: the pool is a reserved, identity-mapped block of exactly
    // `table_pool_pages` pages that nothing else uses.
    let pool = unsafe { paging::Pool::new(tables_phys, table_pool_pages) };
    let mut tables = paging::Tables::new(pool).map_err(|_| "page_table_pool_exhausted")?;

    // The identity map stays executable: the instructions between loading CR3
    // and jumping into the kernel run at their physical addresses. It exists
    // only for that window, and the kernel's own tables replace it.
    tables
        .map_large_range(0, 0, map_limit / paging::LARGE_PAGE_SIZE, paging::WRITABLE)
        .map_err(|_| "identity_map_failed")?;
    tables
        .map_large_range(
            HHDM_BASE,
            0,
            map_limit / paging::LARGE_PAGE_SIZE,
            paging::WRITABLE | paging::NO_EXECUTE,
        )
        .map_err(|_| "direct_map_failed")?;

    for segment in &kernel.segments[..kernel.segment_count] {
        let phys = kernel_phys + (segment.vaddr - kernel.base);
        let mut flags = paging::WRITABLE;
        if !segment.execute {
            flags |= paging::NO_EXECUTE;
        }
        if !segment.write {
            flags &= !paging::WRITABLE;
        }
        tables
            .map_range(segment.vaddr, phys, segment.pages(), flags)
            .map_err(|_| "kernel_map_failed")?;
    }

    let acpi_rsdp = uefi::system::with_config_table(|entries| {
        entries
            .iter()
            .find(|entry| entry.guid == uefi::table::cfg::ConfigTableEntry::ACPI2_GUID)
            .map_or(0, |entry| entry.address as u64)
    });

    event!(
        "loader.tables_built",
        "cr3=0x{:x} identity_pages={hhdm_pages} direct_map_base=0x{HHDM_BASE:x} \
         table_pages_used={} table_pages_reserved={} kernel_phys=0x{kernel_phys:x} \
         kernel_pages={kernel_pages} stack_phys=0x{stack_phys:x} stack_pages={BOOT_STACK_PAGES} \
         acpi_rsdp=0x{acpi_rsdp:x}",
        tables.root(),
        tables.pool().used(),
        tables.pool().pages()
    );

    // NX is set in the tables above, so the bit has to be enabled before CR3 is
    // loaded or every one of those entries becomes a reserved-bit fault.
    // SAFETY: `IA32_EFER` exists on every CPU that reached long mode, which is
    // every CPU that can run this binary, and bit 11 is its no-execute enable.
    unsafe { write_efer(read_efer() | (1 << 11)) };

    Ok(Prepared {
        tables_root: tables.root(),
        tables_phys,
        tables_pages: table_pool_pages as u64,
        entry: kernel.entry,
        kernel_phys,
        kernel_pages,
        stack_phys,
        stack_pages: BOOT_STACK_PAGES,
        handoff_phys,
        handoff_pages,
        regions_phys: handoff_phys + layout.regions_offset,
        modules_phys: handoff_phys + layout.modules_offset,
        module_count: directory.count as u32,
        hhdm_pages,
        acpi_rsdp,
    })
}

struct HandoffLayout {
    regions_offset: u64,
    modules_offset: u64,
    total_pages: u64,
}

fn handoff_layout() -> HandoffLayout {
    let info = core::mem::size_of::<BootInfo>() as u64;
    let regions = (MAX_MEMORY_REGIONS * core::mem::size_of::<MemoryRegion>()) as u64;
    let modules = (MAX_MODULES * core::mem::size_of::<BootModule>()) as u64;
    let regions_offset = info.div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let modules_offset = (regions_offset + regions).div_ceil(PAGE_SIZE) * PAGE_SIZE;
    let total = modules_offset + modules;
    HandoffLayout {
        regions_offset,
        modules_offset,
        total_pages: total.div_ceil(PAGE_SIZE),
    }
}

fn read_efer() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `IA32_EFER` exists in long mode.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0080u32, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_efer(value: u64) {
    // SAFETY: the caller sets only bits that are valid in long mode.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC000_0080u32,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// True for memory types that name real RAM the kernel could hold pages in.
fn is_ram(kind: MemoryType) -> bool {
    match kind {
        MemoryType::CONVENTIONAL
        | MemoryType::BOOT_SERVICES_CODE
        | MemoryType::BOOT_SERVICES_DATA
        | MemoryType::LOADER_CODE
        | MemoryType::LOADER_DATA
        | MemoryType::RUNTIME_SERVICES_CODE
        | MemoryType::RUNTIME_SERVICES_DATA
        | MemoryType::ACPI_RECLAIM
        | MemoryType::ACPI_NON_VOLATILE => true,
        other => matches!(
            other.0,
            memory_type::KERNEL_IMAGE | memory_type::MODULES | memory_type::BOOT_RECLAIMABLE
        ),
    }
}

fn classify(kind: MemoryType) -> u32 {
    match kind {
        MemoryType::CONVENTIONAL
        | MemoryType::BOOT_SERVICES_CODE
        | MemoryType::BOOT_SERVICES_DATA => region_kind::USABLE,
        MemoryType::LOADER_CODE | MemoryType::LOADER_DATA => region_kind::BOOT_RECLAIMABLE,
        MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
            region_kind::FIRMWARE_RUNTIME
        }
        MemoryType::ACPI_RECLAIM => region_kind::ACPI_RECLAIMABLE,
        MemoryType::ACPI_NON_VOLATILE => region_kind::ACPI_NVS,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => region_kind::MMIO,
        MemoryType::UNUSABLE => region_kind::UNUSABLE,
        other => match other.0 {
            memory_type::KERNEL_IMAGE => region_kind::KERNEL_IMAGE,
            memory_type::MODULES => region_kind::MODULES,
            memory_type::BOOT_RECLAIMABLE => region_kind::BOOT_RECLAIMABLE,
            _ => region_kind::RESERVED,
        },
    }
}

/// Leaves boot services, records the final memory map and jumps to the kernel.
///
/// # Safety
///
/// `prepared` must describe reserved memory that survives `ExitBootServices`,
/// and its page tables must map the identity range this code executes in, the
/// direct map the kernel's stack pointer refers to, and the kernel image at its
/// link addresses.
unsafe fn transfer(prepared: Prepared) -> ! {
    // SAFETY: no protocol reference, pool allocation or boot-services structure
    // is live past this point: the file contents were dropped, the memory map
    // taken earlier was dropped, and everything the kernel receives lives in
    // pages reserved with allocator types that survive.
    let map = unsafe { boot::exit_boot_services(Some(MemoryType(memory_type::BOOT_RECLAIMABLE))) };

    // SAFETY: firmware disables its timer in `ExitBootServices`, but the
    // interrupt flag is the caller's to control from here on and no interrupt
    // descriptor table of ours exists yet.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    let regions = prepared.regions_phys as *mut MemoryRegion;
    let mut count = 0usize;
    for descriptor in map.entries() {
        if count == MAX_MEMORY_REGIONS {
            event!(
                "loader.failed",
                "reason=memory_map_overflow entries={count}"
            );
            halt();
        }
        let region = MemoryRegion {
            base: descriptor.phys_start,
            pages: descriptor.page_count,
            kind: classify(descriptor.ty),
            flags: 0,
        };
        // SAFETY: `regions` points at the reserved region array inside the
        // hand-off block, which has room for `MAX_MEMORY_REGIONS` entries, and
        // `count` is below that bound.
        unsafe { regions.add(count).write(region) };
        count += 1;
    }
    core::mem::forget(map);

    let info = BootInfo {
        magic: BOOTINFO_MAGIC,
        version_major: BOOTINFO_VERSION_MAJOR,
        version_minor: BOOTINFO_VERSION_MINOR,
        header_len: core::mem::size_of::<BootInfo>() as u32,
        hhdm_base: HHDM_BASE,
        hhdm_pages: prepared.hhdm_pages,
        memory_map_phys: prepared.regions_phys,
        memory_map_entries: count as u32,
        memory_map_entry_size: core::mem::size_of::<MemoryRegion>() as u32,
        modules_phys: prepared.modules_phys,
        modules_count: prepared.module_count,
        modules_entry_size: core::mem::size_of::<BootModule>() as u32,
        acpi_rsdp_phys: prepared.acpi_rsdp,
        kernel_phys_base: prepared.kernel_phys,
        kernel_virt_base: KERNEL_IMAGE_BASE,
        kernel_image_pages: prepared.kernel_pages,
        bootstrap_tables_phys: prepared.tables_phys,
        bootstrap_tables_pages: prepared.tables_pages,
        boot_stack_phys: prepared.stack_phys,
        boot_stack_pages: prepared.stack_pages,
        handoff_phys: prepared.handoff_phys,
        handoff_pages: prepared.handoff_pages,
        loader_event_count: emitted() + 2,
        boot_epoch: 0,
        serial_port: serial::COM1,
        reserved1: [0; 3],
    };
    // SAFETY: the hand-off block was reserved with room for the structure and
    // is page aligned, so the write is aligned and in bounds.
    unsafe { core::ptr::write(prepared.handoff_phys as *mut BootInfo, info) };

    event!(
        "loader.exit_boot_services",
        "status=ok memory_regions={count} handoff=0x{:x} entry=0x{:x}",
        prepared.handoff_phys,
        prepared.entry
    );
    event!(
        "loader.handoff",
        "cr3=0x{:x} rsp=0x{:x} rdi=0x{:x} kernel_entry=0x{:x} direct_map_base=0x{HHDM_BASE:x}",
        prepared.tables_root,
        HHDM_BASE + prepared.stack_phys + prepared.stack_pages * PAGE_SIZE,
        prepared.handoff_phys,
        prepared.entry
    );

    let stack_top = HHDM_BASE + prepared.stack_phys + prepared.stack_pages * PAGE_SIZE;
    // SAFETY: the caller guarantees the tables map this code's identity range,
    // the direct map that `stack_top` lies in, and the kernel image containing
    // `entry`. Every operand is already in a register before CR3 changes.
    unsafe {
        core::arch::asm!(
            "mov cr3, {cr3}",
            "mov rsp, {stack}",
            "xor rbp, rbp",
            "jmp {entry}",
            cr3 = in(reg) prepared.tables_root,
            stack = in(reg) stack_top,
            entry = in(reg) prepared.entry,
            in("rdi") prepared.handoff_phys,
            options(noreturn)
        );
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    match info.location() {
        Some(location) => emit(
            "loader.panic",
            format_args!(
                "file={} line={} message=\"{}\"",
                location.file(),
                location.line(),
                info.message()
            ),
        ),
        None => emit(
            "loader.panic",
            format_args!("file=? line=0 message=\"{}\"", info.message()),
        ),
    }
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: halting with interrupts masked is the terminal state.
        unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) }
    }
}
