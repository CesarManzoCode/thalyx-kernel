//! Interrupt descriptor table.
//!
//! Every gate is an interrupt gate at descriptor privilege level 0: they clear
//! IF on entry, which is what makes every kernel path in K1 non-preemptible,
//! and ring 3 cannot raise any vector with a software interrupt. Three vectors
//! run on emergency stacks because they can arrive when the current stack is
//! the reason for the fault.

use core::arch::asm;

use super::gdt;
use super::trap;

/// Double fault. Runs on interrupt stack table slot 1.
pub const VECTOR_DOUBLE_FAULT: usize = 8;
/// Non-maskable interrupt. Runs on interrupt stack table slot 2.
pub const VECTOR_NMI: usize = 2;
/// Machine check. Runs on interrupt stack table slot 3.
pub const VECTOR_MACHINE_CHECK: usize = 18;

#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

const _: () = assert!(core::mem::size_of::<Entry>() == 16);

impl Entry {
    const fn empty() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            flags: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn interrupt_gate(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE,
            ist,
            // Present, DPL 0, 64-bit interrupt gate.
            flags: 0x8E,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

static mut IDT: [Entry; 256] = [Entry::empty(); 256];

/// Fills the descriptor table and loads it on the calling processor.
///
/// # Safety
///
/// Must run once, on the bootstrap processor, after the GDT is installed and
/// after the emergency stacks are mapped and registered in the TSS.
pub unsafe fn install() {
    // SAFETY: the bootstrap processor is the only context in the machine when
    // this runs, so no other reference to the table exists.
    unsafe {
        let idt = &raw mut IDT;
        for vector in 0..256usize {
            let ist = match vector {
                VECTOR_DOUBLE_FAULT => 1,
                VECTOR_NMI => 2,
                VECTOR_MACHINE_CHECK => 3,
                _ => 0,
            };
            (*idt)[vector] = Entry::interrupt_gate(trap::stub_address(vector), ist);
        }
        load();
    }
}

/// Loads the already-built table on the calling processor.
///
/// The table itself is shared. Sharing it is correct and duplicating it would
/// not be an improvement: it is written once, before any processor but the
/// bootstrap one exists, and never again. What must *not* be shared is the
/// interrupt stack table it names, and that lives in each processor's own task
/// state segment.
///
/// # Safety
///
/// Must run once per processor, with interrupts masked, after that processor's
/// emergency stacks are registered in its own task state segment.
pub unsafe fn load() {
    // SAFETY: the table is a static built before any other processor started,
    // and is only read from here on.
    unsafe {
        let idt = &raw const IDT;
        let pointer = DescriptorPointer {
            limit: (core::mem::size_of::<[Entry; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
    }
}
