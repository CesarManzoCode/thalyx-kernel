//! Loader-to-kernel hand-off contract.
//!
//! This crate is the whole interface between `boot/uefi` and `kernel`. The
//! loader owns firmware services and the `efiapi` calling convention; the
//! kernel owns nothing of either. Everything crossing the boundary is a
//! `#[repr(C)]`, little-endian, explicitly padded structure with a magic value,
//! a version pair and self-describing lengths, and every pointer it carries
//! refers to memory the loader reserved with a lifetime that outlives
//! `ExitBootServices`.
//!
//! Contract: `vault/architecture/hardware.md`, section "Secuencia de
//! bootstrap".

#![no_std]

/// `THLXBOOT` in little-endian byte order.
pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"THLXBOOT");

/// Major hand-off version. A kernel that does not recognise it must refuse to
/// run rather than guess at the layout.
pub const BOOTINFO_VERSION_MAJOR: u16 = 0;
/// Minor hand-off version. Additive fields only, appended at the end.
pub const BOOTINFO_VERSION_MINOR: u16 = 1;

/// Virtual base of the direct map of physical memory installed by the loader
/// and re-established by the kernel in its own tables. PML4 slot 256.
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Virtual base of the kernel's dynamic region (kernel stacks and their guard
/// pages). PML4 slot 257.
pub const KERNEL_DYNAMIC_BASE: u64 = 0xFFFF_8080_0000_0000;

/// Virtual base the kernel image is linked at. PML4 slot 511, chosen so the
/// `kernel` code model of `x86_64-unknown-none` is valid.
pub const KERNEL_IMAGE_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// First virtual address a user domain may map. Page zero stays unmapped so a
/// null dereference faults.
pub const USER_MIN_ADDR: u64 = 0x1000;

/// One past the last virtual address a user domain may map. The lower canonical
/// half ends here.
pub const USER_MAX_ADDR: u64 = 0x0000_8000_0000_0000;

/// Page size the whole V0 platform profile uses for user and kernel mappings.
pub const PAGE_SIZE: u64 = 4096;

/// Largest number of physical memory regions the loader will hand over. The
/// buffer is reserved before `ExitBootServices`; a firmware map larger than
/// this is reported as a hard loader failure rather than silently truncated.
pub const MAX_MEMORY_REGIONS: usize = 512;

/// Largest number of boot modules the loader will hand over.
pub const MAX_MODULES: usize = 16;

/// Physical memory region as classified by the loader.
///
/// The classification is the loader's, not the firmware's: it collapses the
/// UEFI memory types into the categories the kernel's frame allocator and
/// reclamation actually distinguish.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    /// Page-aligned physical base address.
    pub base: u64,
    /// Length in 4 KiB pages.
    pub pages: u64,
    /// One of [`region_kind`].
    pub kind: u32,
    /// Reserved, zero in this version.
    pub flags: u32,
}

/// Region classifications used by [`MemoryRegion::kind`].
pub mod region_kind {
    /// Free RAM the kernel may allocate from immediately.
    pub const USABLE: u32 = 1;
    /// RAM holding loader structures: bootstrap page tables, the boot stack,
    /// the hand-off structures themselves. Reclaimable only after the kernel
    /// has installed its own page tables and copied what it needs.
    pub const BOOT_RECLAIMABLE: u32 = 2;
    /// RAM holding the loaded kernel image.
    pub const KERNEL_IMAGE: u32 = 3;
    /// RAM holding boot modules.
    pub const MODULES: u32 = 4;
    /// Firmware code or data that stays live after `ExitBootServices`.
    pub const FIRMWARE_RUNTIME: u32 = 5;
    /// ACPI tables, reclaimable only after they have been parsed.
    pub const ACPI_RECLAIMABLE: u32 = 6;
    /// ACPI non-volatile storage, never reclaimable.
    pub const ACPI_NVS: u32 = 7;
    /// Firmware-reserved memory.
    pub const RESERVED: u32 = 8;
    /// Memory the firmware reported as defective.
    pub const UNUSABLE: u32 = 9;
    /// Memory-mapped I/O or port space, never RAM.
    pub const MMIO: u32 = 10;
}

/// A module of the initial boot package, already copied into reserved memory.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootModule {
    /// NUL-padded ASCII name from the boot package directory.
    pub name: [u8; 32],
    /// Page-aligned physical base of the module image.
    pub phys_base: u64,
    /// Exact byte length of the module image.
    pub length: u64,
    /// One of [`module_kind`].
    pub kind: u32,
    /// Module flags from the boot package, see [`module_flags`].
    pub flags: u32,
}

/// Module classifications used by [`BootModule::kind`].
pub mod module_kind {
    /// A statically linked ET_EXEC ELF64 image for a user domain.
    ///
    /// In a K1 package the kernel instantiates every such module itself. In a
    /// K2 package it hands each one to the supervisor as a sealed memory
    /// object, and the supervisor decides what becomes a domain.
    pub const USER_ELF: u32 = 1;
    /// The image of the first supervisor.
    ///
    /// Exactly one module may carry this kind. Its presence is what selects the
    /// K2 boot path: the kernel builds this domain, gives it an explicit
    /// capability manifest, and creates nothing else.
    pub const SUPERVISOR: u32 = 2;
}

/// Flags carried from the boot package directory into [`BootModule::flags`].
pub mod module_flags {
    /// The image is expected to be rejected by validation. The kernel does not
    /// treat this as permission to relax any check; it only marks the module in
    /// the diagnostic plane so a rejection is distinguishable from a build
    /// accident.
    pub const EXPECT_REJECT: u32 = 1 << 0;
    /// On the supervisor module: once that supervisor is built, the kernel
    /// writes summary records to the diagnostic plane but withholds trace
    /// records -- one per operation or object -- and states how many it
    /// withheld. Under hardware virtualization a trace record costs about as
    /// much as a thousand of the operations it describes, so a package that
    /// measures asks for this. It changes nothing the kernel does, only what
    /// it says about it; the control-receipt plane is not affected.
    pub const TRACE_OFF: u32 = 1 << 1;
}

/// Root hand-off structure. The loader passes its **physical** address in RDI;
/// the kernel reaches it through [`HHDM_BASE`], which the loader has already
/// mapped.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    /// Must equal [`BOOTINFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOTINFO_VERSION_MAJOR`].
    pub version_major: u16,
    /// Minor version; the kernel accepts any minor it is at least as new as.
    pub version_minor: u16,
    /// Size of this structure in bytes, as built by the loader.
    pub header_len: u32,

    /// Virtual base of the direct map installed by the loader. Must equal
    /// [`HHDM_BASE`]; carried explicitly so a mismatch is a validation failure
    /// rather than a silent divergence between two constants.
    pub hhdm_base: u64,
    /// Number of 4 KiB pages of physical address space covered by the direct
    /// map, starting at physical zero.
    pub hhdm_pages: u64,

    /// Physical address of a [`MemoryRegion`] array.
    pub memory_map_phys: u64,
    /// Number of entries in that array.
    pub memory_map_entries: u32,
    /// Size of one entry, for forward compatibility.
    pub memory_map_entry_size: u32,

    /// Physical address of a [`BootModule`] array.
    pub modules_phys: u64,
    /// Number of entries in that array.
    pub modules_count: u32,
    /// Size of one entry, for forward compatibility.
    pub modules_entry_size: u32,

    /// Physical address of the ACPI 2.0+ RSDP, or zero if the firmware exposed
    /// none. K1 records it and does not parse it.
    pub acpi_rsdp_phys: u64,

    /// Physical base of the loaded kernel image.
    pub kernel_phys_base: u64,
    /// Virtual base the kernel image was mapped at.
    pub kernel_virt_base: u64,
    /// Size of the loaded kernel image in 4 KiB pages.
    pub kernel_image_pages: u64,

    /// Physical base of the bootstrap page tables.
    pub bootstrap_tables_phys: u64,
    /// Size of the bootstrap page tables in 4 KiB pages.
    pub bootstrap_tables_pages: u64,

    /// Physical base of the stack the kernel entry point runs on.
    pub boot_stack_phys: u64,
    /// Size of that stack in 4 KiB pages, guard page included.
    pub boot_stack_pages: u64,

    /// Physical base of the reserved block that holds this structure, the
    /// memory map array and the module array.
    pub handoff_phys: u64,
    /// Size of that block in 4 KiB pages.
    pub handoff_pages: u64,

    /// Number of diagnostic events the loader already emitted, so the kernel
    /// continues one sequence instead of restarting it.
    pub loader_event_count: u64,

    /// Firmware-derived value identifying this boot. It is **not** a unique
    /// identifier and carries no entropy claim: see
    /// `vault/architecture/hardware.md`, "Reloj y entropía".
    pub boot_epoch: u64,

    /// I/O port base of the 16550-compatible UART the loader used for its
    /// diagnostic output, or zero if it found none.
    pub serial_port: u16,
    /// Reserved, zero.
    pub reserved1: [u16; 3],
}

const _: () = assert!(core::mem::size_of::<MemoryRegion>() == 24);
const _: () = assert!(core::mem::size_of::<BootModule>() == 56);
const _: () = assert!(core::mem::size_of::<BootInfo>() == 168);
const _: () = assert!(core::mem::align_of::<BootInfo>() == 8);

/// Boot package container format, `THLXPKG0`.
///
/// The package is a flat directory of named byte ranges. It is deliberately not
/// an archive format with compression, ownership or permissions: the loader has
/// to validate it entirely before `ExitBootServices` with no allocator of its
/// own, and every field it needs is a bounded integer.
pub mod package {
    /// `THLXPKG0` in little-endian byte order.
    pub const MAGIC: u64 = u64::from_le_bytes(*b"THLXPKG0");
    /// Major package version.
    pub const VERSION_MAJOR: u16 = 0;
    /// Minor package version.
    pub const VERSION_MINOR: u16 = 1;

    /// Package header. Immediately followed by `entry_count` [`Entry`] records.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Header {
        /// Must equal [`MAGIC`].
        pub magic: u64,
        /// Must equal [`VERSION_MAJOR`].
        pub version_major: u16,
        /// Package minor version.
        pub version_minor: u16,
        /// Size of this header in bytes.
        pub header_len: u32,
        /// Number of directory entries.
        pub entry_count: u32,
        /// Size of one directory entry in bytes.
        pub entry_size: u32,
        /// Total size of the package file in bytes.
        pub total_len: u64,
        /// Reserved, zero.
        pub reserved: u64,
    }

    /// One directory entry.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Entry {
        /// NUL-padded ASCII name.
        pub name: [u8; 32],
        /// Byte offset of the payload from the start of the package file.
        pub offset: u64,
        /// Payload length in bytes.
        pub length: u64,
        /// Module kind, see [`super::module_kind`].
        pub kind: u32,
        /// Module flags, see [`super::module_flags`].
        pub flags: u32,
        /// Reserved, zero.
        pub reserved: u64,
    }

    const _: () = assert!(core::mem::size_of::<Header>() == 40);
    const _: () = assert!(core::mem::size_of::<Entry>() == 64);
}

/// UEFI memory types the loader allocates with, so the classification the
/// kernel receives comes from the firmware's own map rather than from a
/// side table the two sides could disagree about.
///
/// Values at or above `0x8000_0000` are reserved by the UEFI specification for
/// the operating system loader, which is exactly this use.
pub mod memory_type {
    /// Pages holding the loaded kernel image.
    pub const KERNEL_IMAGE: u32 = 0x8000_0001;
    /// Pages holding boot module images.
    pub const MODULES: u32 = 0x8000_0002;
    /// Pages holding loader structures: bootstrap page tables, the boot stack
    /// and the hand-off block.
    pub const BOOT_RECLAIMABLE: u32 = 0x8000_0003;
}

/// Path of the kernel image on the EFI system partition.
pub const KERNEL_PATH: &str = "\\thalyx\\kernel.elf";
/// Path of the initial boot package on the EFI system partition.
pub const PACKAGE_PATH: &str = "\\thalyx\\boot.tbp";
