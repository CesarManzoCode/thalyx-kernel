//! The code an application processor executes before the kernel exists for it.
//!
//! A start-up interrupt puts a processor at `CS:IP = vector:0000` in real mode.
//! Nothing the kernel is compiled as can run there: it is 64-bit code at a
//! virtual address that no page table is loaded to translate. So the first
//! instructions a processor executes are these, copied into a page below one
//! mebibyte, and they do exactly three things — enter protected mode, enable
//! paging into an address space that maps both this page and the kernel, and
//! jump to 64-bit code at the kernel's own address.
//!
//! The blob is laid out at fixed offsets inside its page so the 16-bit stage
//! can name its data with literal displacements. Three fields carry absolute
//! linear addresses that are only known once the page is chosen, and the
//! bootstrap processor writes them into the copy before it sends anything.
//! Nothing here is self-modifying at run time: the writes happen before the
//! target processor exists.

use core::arch::global_asm;

/// Offset of the 32-bit stage inside the blob.
pub const OFFSET_STAGE32: usize = 0x0C0;
/// Offset of the 64-bit stage inside the blob.
pub const OFFSET_STAGE64: usize = 0x140;
/// Offset of the descriptor table.
pub const OFFSET_GDT: usize = 0x180;
/// Offset of the descriptor table pointer; its base field follows the limit.
pub const OFFSET_GDTR: usize = 0x1C0;
/// Offset of the base field inside that pointer.
pub const OFFSET_GDTR_BASE: usize = OFFSET_GDTR + 2;
/// Offset of the parameter block.
pub const OFFSET_PARAMS: usize = 0x200;

/// Parameter block the bootstrap processor fills in before starting a
/// processor. Offsets are matched by the assembly below.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Params {
    /// Root of the address space the processor enables paging with. Must be
    /// below four gibibytes: the 32-bit stage loads it with a 32-bit move.
    pub cr3: u64,
    /// Top of the kernel stack the 64-bit stage installs.
    pub stack_top: u64,
    /// Address of the 64-bit Rust entry point.
    pub entry: u64,
    /// Dense index of the processor, passed to that entry point.
    pub cpu_index: u64,
}

const _: () = assert!(core::mem::size_of::<Params>() == 32);

unsafe extern "C" {
    /// First byte of the blob.
    pub static thalyx_ap_trampoline: u8;
    /// One past the last byte of the blob.
    pub static thalyx_ap_trampoline_end: u8;
    /// The 32-bit absolute target of the real-mode far jump.
    pub static thalyx_ap_patch_far32: u8;
    /// The 32-bit absolute target of the protected-mode far jump.
    pub static thalyx_ap_patch_far64: u8;
}

/// Byte length of the blob.
#[must_use]
pub fn blob_len() -> usize {
    let start = (&raw const thalyx_ap_trampoline) as usize;
    let end = (&raw const thalyx_ap_trampoline_end) as usize;
    end - start
}

/// Offset of a patch site inside the blob.
fn patch_offset(site: *const u8) -> usize {
    (site as usize) - ((&raw const thalyx_ap_trampoline) as usize)
}

/// Offset of the real-mode far jump's target field.
#[must_use]
pub fn offset_far32() -> usize {
    patch_offset(&raw const thalyx_ap_patch_far32)
}

/// Offset of the protected-mode far jump's target field.
#[must_use]
pub fn offset_far64() -> usize {
    patch_offset(&raw const thalyx_ap_patch_far64)
}

global_asm!(
    ".section .text",
    ".balign 16",
    ".global thalyx_ap_trampoline",
    ".global thalyx_ap_trampoline_end",
    ".global thalyx_ap_patch_far32",
    ".global thalyx_ap_patch_far64",
    "thalyx_ap_trampoline:",
    // ---- 16-bit stage -----------------------------------------------------
    // Entered at CS = the start-up vector, IP = 0. `DS` is set from `CS` so the
    // literal displacements below address this page, and `EBX` is loaded with
    // the page's linear base, which the later stages use to reach the
    // parameter block once segmentation is flat.
    ".code16",
    "cli",
    "cld",
    "mov ax, cs",
    "mov ds, ax",
    "movzx ebx, ax",
    "shl ebx, 4",
    "lgdt [0x1c0]",
    "mov eax, cr0",
    "or al, 1",
    "mov cr0, eax",
    ".byte 0x66, 0xea",
    "thalyx_ap_patch_far32:",
    ".long 0",
    ".word 0x08",
    // ---- 32-bit stage -----------------------------------------------------
    ".space 0x0c0 - (. - thalyx_ap_trampoline)",
    ".code32",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    "mov fs, ax",
    "mov gs, ax",
    "mov eax, cr4",
    "or eax, 0x20", // CR4.PAE
    "mov cr4, eax",
    "mov eax, [ebx + 0x200]", // Params::cr3
    "mov cr3, eax",
    "mov ecx, 0xc0000080", // IA32_EFER
    "rdmsr",
    "or eax, 0x900", // LME | NXE
    "wrmsr",
    "mov eax, cr0",
    "or eax, 0x80010001", // PG | WP | PE
    "mov cr0, eax",
    ".byte 0xea",
    "thalyx_ap_patch_far64:",
    ".long 0",
    ".word 0x18",
    // ---- 64-bit stage -----------------------------------------------------
    ".space 0x140 - (. - thalyx_ap_trampoline)",
    ".code64",
    "mov ebx, ebx",           // clear any upper half the mode change left
    "mov rsp, [rbx + 0x208]", // Params::stack_top
    "mov rdi, [rbx + 0x218]", // Params::cpu_index
    "mov rax, [rbx + 0x210]", // Params::entry
    "jmp rax",
    // ---- descriptor table -------------------------------------------------
    ".space 0x180 - (. - thalyx_ap_trampoline)",
    ".quad 0",
    ".quad 0x00cf9a000000ffff", // 0x08: 32-bit code
    ".quad 0x00cf92000000ffff", // 0x10: 32-bit data
    ".quad 0x00af9a000000ffff", // 0x18: 64-bit code
    ".space 0x1c0 - (. - thalyx_ap_trampoline)",
    ".word 31", // limit of the four descriptors above
    ".long 0",  // base, written before the processor starts
    ".space 0x220 - (. - thalyx_ap_trampoline)",
    "thalyx_ap_trampoline_end:",
    ".code64",
);
