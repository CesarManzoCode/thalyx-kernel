//! Global descriptor table and task state segment.
//!
//! The selector layout is not free: `SYSCALL`/`SYSRET` derive the ring 3
//! selectors from `IA32_STAR`, so user data must sit immediately above kernel
//! data and user code immediately above user data. K1 returns to user through
//! `iretq` rather than `sysret`, but `syscall` entry reads the same MSR, so the
//! layout is fixed by the instruction either way.

use core::arch::asm;

use crate::layout;

/// Kernel code selector.
pub const KERNEL_CODE: u16 = 0x08;
/// Kernel data selector.
pub const KERNEL_DATA: u16 = 0x10;
/// User data selector, requested privilege level 3.
pub const USER_DATA: u16 = 0x18 | 3;
/// User code selector, requested privilege level 3.
pub const USER_CODE: u16 = 0x20 | 3;
/// Task state segment selector.
pub const TSS_SELECTOR: u16 = 0x28;

/// `IA32_STAR` bits 47:32 select the kernel selectors used by `syscall`.
pub const STAR_SYSCALL_BASE: u64 = KERNEL_CODE as u64;
/// `IA32_STAR` bits 63:48; `sysret` derives user CS from base + 16 and user SS
/// from base + 8, which is why this is the kernel data selector.
pub const STAR_SYSRET_BASE: u64 = KERNEL_DATA as u64;

const _: () = assert!(USER_DATA as u64 == STAR_SYSRET_BASE + 8 + 3);
const _: () = assert!(USER_CODE as u64 == STAR_SYSRET_BASE + 16 + 3);

/// 64-bit task state segment. `iomap_base` points past the end of the segment,
/// so no I/O permission bitmap exists and every port access from ring 3 faults.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Tss {
    reserved0: u32,
    /// Stack pointers for privilege levels 0..2.
    pub rsp: [u64; 3],
    reserved1: u64,
    /// Interrupt stack table entries 1..7, indexed from zero here.
    pub ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

const _: () = assert!(core::mem::size_of::<Tss>() == 104);

impl Tss {
    const fn new() -> Self {
        Self {
            reserved0: 0,
            rsp: [0; 3],
            reserved1: 0,
            ist: [0; 7],
            reserved2: 0,
            reserved3: 0,
            iomap_base: core::mem::size_of::<Tss>() as u16,
        }
    }
}

#[repr(C, align(16))]
struct Gdt {
    entries: [u64; 7],
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

// K1 is uniprocessor: one GDT and one TSS. Both are mutated only during
// bootstrap and, for `rsp[0]`, from the context switch with interrupts masked.
static mut GDT: Gdt = Gdt { entries: [0; 7] };
static mut TSS: Tss = Tss::new();

/// Sets the ring 0 stack pointer the CPU loads on a privilege transition into
/// the kernel.
///
/// # Safety
///
/// `top` must be the top of a mapped, 16-byte-aligned kernel stack that belongs
/// to the thread about to run. Interrupts must be masked, because a trap
/// between this write and the switch would land on the wrong stack.
pub unsafe fn set_kernel_stack(top: u64) {
    // SAFETY: uniprocessor, interrupts masked by the caller; no other reference
    // to the TSS exists at this point.
    unsafe { (*(&raw mut TSS)).rsp[0] = top };
}

/// Address of the emergency stack registered for interrupt stack table slot
/// `index` (zero-based, so slot 0 is IST1).
#[must_use]
pub fn ist_stack(index: usize) -> u64 {
    // SAFETY: as `kernel_stack`.
    unsafe { (*(&raw const TSS)).ist[index] }
}

/// Builds the descriptor table, installs it, reloads every segment register and
/// loads the task register.
///
/// # Safety
///
/// Must run once, on the bootstrap path, with interrupts masked and with the
/// emergency stacks already mapped. Reloading CS through a far return requires
/// that the code following the call remains mapped at the same address, which
/// holds because the kernel image mapping does not change here.
pub unsafe fn install(ist_tops: [u64; layout::IST_COUNT]) {
    // 64-bit code and data descriptors. The base and limit fields are ignored
    // in long mode; the bits that matter are present, descriptor type, DPL,
    // long mode and, for data, writable.
    const KERNEL_CODE_DESC: u64 = 0x00AF_9A00_0000_FFFF;
    const KERNEL_DATA_DESC: u64 = 0x00CF_9200_0000_FFFF;
    const USER_DATA_DESC: u64 = 0x00CF_F200_0000_FFFF;
    const USER_CODE_DESC: u64 = 0x00AF_FA00_0000_FFFF;

    // SAFETY: bootstrap path, uniprocessor, interrupts masked.
    unsafe {
        let tss = &raw mut TSS;
        for (slot, top) in ist_tops.iter().enumerate() {
            (*tss).ist[slot] = *top;
        }

        let tss_base = tss as u64;
        let tss_limit = (core::mem::size_of::<Tss>() - 1) as u64;
        let tss_low = (tss_limit & 0xFFFF)
            | ((tss_base & 0x00FF_FFFF) << 16)
            | (0x9u64 << 40)            // available 64-bit TSS
            | (1u64 << 47)              // present
            | (((tss_limit >> 16) & 0xF) << 48)
            | (((tss_base >> 24) & 0xFF) << 56);
        let tss_high = tss_base >> 32;

        let gdt = &raw mut GDT;
        (*gdt).entries = [
            0,
            KERNEL_CODE_DESC,
            KERNEL_DATA_DESC,
            USER_DATA_DESC,
            USER_CODE_DESC,
            tss_low,
            tss_high,
        ];

        let pointer = DescriptorPointer {
            limit: (core::mem::size_of::<Gdt>() - 1) as u16,
            base: gdt as u64,
        };
        asm!("lgdt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));

        // Reload CS with a far return: there is no `mov cs`.
        asm!(
            "push {selector}",
            "lea {scratch}, [rip + 2f]",
            "push {scratch}",
            "retfq",
            "2:",
            selector = in(reg) u64::from(KERNEL_CODE),
            scratch = lateout(reg) _,
            options(preserves_flags),
        );

        asm!(
            "mov ds, {sel:x}",
            "mov es, {sel:x}",
            "mov fs, {sel:x}",
            "mov gs, {sel:x}",
            "mov ss, {sel:x}",
            sel = in(reg) KERNEL_DATA,
            options(nostack, preserves_flags),
        );

        asm!("ltr {:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}
