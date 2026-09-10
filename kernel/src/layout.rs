//! Fixed virtual layout of the kernel's dynamic region and of a user domain.
//!
//! The addresses are constants rather than the result of an allocator because
//! K1 has a fixed, small set of kernel objects and because a constant layout is
//! checkable by reading it. The kernel dynamic region lives entirely under one
//! PML4 slot, which is what lets every address space share it.

use thalyx_boot_protocol::{KERNEL_DYNAMIC_BASE, PAGE_SIZE, USER_MAX_ADDR};

/// Device mappings. Uncacheable, never RAM.
pub const DEVICE_AREA: u64 = KERNEL_DYNAMIC_BASE;
/// Local APIC register page.
pub const LAPIC_VADDR: u64 = DEVICE_AREA;

/// The firmware's configuration-space window, mapped with one large entry.
/// Two mebibytes cover the first two buses, which is where every device this
/// revision assigns lives.
pub const ECAM_VADDR: u64 = KERNEL_DYNAMIC_BASE + (16 << 20);
/// Buses that window covers.
pub const ECAM_BUSES: u8 = 2;

/// Device registers the kernel maps for itself: the parts of a device's
/// configuration a driver is not allowed to reach directly.
pub const MMIO_AREA: u64 = KERNEL_DYNAMIC_BASE + (32 << 20);
/// Pages of that window.
pub const MMIO_PAGES: u64 = 64;

/// Emergency stacks referenced by the TSS interrupt stack table.
///
/// One set per processor: the slot index is `cpu * IST_COUNT + entry`, so a
/// double fault on one processor never lands on a stack another processor is
/// faulting onto.
pub const IST_AREA: u64 = KERNEL_DYNAMIC_BASE + (1 << 20);
/// Pages per emergency stack.
pub const IST_PAGES: u64 = 2;
/// Pages per emergency stack slot, including the guard page below it.
pub const IST_SLOT_PAGES: u64 = IST_PAGES + 1;
/// Emergency stacks: double fault, non-maskable interrupt, machine check.
pub const IST_COUNT: usize = 3;

/// Per-thread kernel stacks.
pub const KSTACK_AREA: u64 = KERNEL_DYNAMIC_BASE + (2 << 20);
/// Pages per kernel stack.
pub const KSTACK_PAGES: u64 = 4;
/// Pages per kernel stack slot, including the guard page below it.
pub const KSTACK_SLOT_PAGES: u64 = KSTACK_PAGES + 1;

/// Top of a user domain's initial stack. The stack grows down from here and a
/// guard page sits below its lowest mapped page.
pub const USER_STACK_TOP: u64 = USER_MAX_ADDR - PAGE_SIZE;
/// Pages of user stack mapped at domain creation.
pub const USER_STACK_PAGES: u64 = 8;

/// Base of the kernel stack slot `index`, guard page included.
#[must_use]
pub const fn kstack_slot_base(index: usize) -> u64 {
    KSTACK_AREA + (index as u64) * KSTACK_SLOT_PAGES * PAGE_SIZE
}

/// Base of the emergency stack slot `index`, guard page included. The index
/// spans every processor's set, not one processor's three.
#[must_use]
pub const fn ist_slot_base(index: usize) -> u64 {
    IST_AREA + (index as u64) * IST_SLOT_PAGES * PAGE_SIZE
}
