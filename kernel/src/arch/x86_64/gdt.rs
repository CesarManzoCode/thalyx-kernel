//! Global descriptor table and task state segment, one of each per processor.
//!
//! The descriptors themselves are identical on every processor, but the table
//! cannot be shared: its sixth and seventh entries describe *this* processor's
//! task state segment, and the ring 0 stack pointer inside that segment is
//! rewritten by every context switch. One table updated by every processor for
//! all of them is the race the platform contract names.
//!
//! The selector layout is not free: `SYSCALL`/`SYSRET` derive the ring 3
//! selectors from `IA32_STAR`, so user data must sit immediately above kernel
//! data and user code immediately above user data. K1 returns to user through
//! `iretq` rather than `sysret`, but `syscall` entry reads the same MSR, so the
//! layout is fixed by the instruction either way.

use core::arch::asm;

use crate::layout;
use crate::limits::MAX_CPUS;
use crate::percpu;

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
/// `IA32_STAR` bits 63:48; `sysretq` derives user CS from base + 16 and user
/// SS from base + 8, which is why this is the kernel data selector -- **with
/// its requested privilege level already set to three**.
///
/// The privilege bits belong in this field and not in the processor's
/// arithmetic. Intel's `SYSRET` forces the returned selectors to RPL 3; AMD's
/// takes them from this field as it stands, and on a processor that does the
/// latter a base with RPL 0 puts ring-3 code on `SS = 0x18`. Nothing faults
/// there and nothing looks wrong: the fault arrives at the *next* interrupt
/// from that thread, whose `iretq` finds a frame whose stack selector does not
/// agree with its code selector, in the kernel, on another processor's stack.
/// Setting it here makes both implementations produce the same two selectors.
pub const STAR_SYSRET_BASE: u64 = (KERNEL_DATA as u64) | 3;

const _: () = assert!(USER_DATA as u64 == STAR_SYSRET_BASE + 8);
const _: () = assert!(USER_CODE as u64 == STAR_SYSRET_BASE + 16);

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
#[derive(Clone, Copy)]
struct Gdt {
    entries: [u64; 7],
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

// One table and one segment per processor. A slot is written by the bootstrap
// processor before the processor it belongs to exists, and afterwards only by
// that processor itself, with interrupts masked.
static mut GDTS: [Gdt; MAX_CPUS] = [Gdt { entries: [0; 7] }; MAX_CPUS];
static mut TSSES: [Tss; MAX_CPUS] = [Tss::new(); MAX_CPUS];

fn tss_ptr(cpu: usize) -> *mut Tss {
    // SAFETY: the array is a static of fixed length and the index is bounded.
    unsafe { (&raw mut TSSES).cast::<Tss>().add(cpu.min(MAX_CPUS - 1)) }
}

/// Sets the ring 0 stack pointer this processor loads on a privilege
/// transition into the kernel.
///
/// # Safety
///
/// `top` must be the top of a mapped, 16-byte-aligned kernel stack that belongs
/// to the thread about to run on this processor. Interrupts must be masked,
/// because a trap between this write and the switch would land on the wrong
/// stack.
pub unsafe fn set_kernel_stack(top: u64) {
    let cpu = percpu::index();
    // SAFETY: each processor writes only its own segment, with interrupts
    // masked by the caller, so no other context can observe a half-written
    // pointer for this processor.
    unsafe { (*tss_ptr(cpu)).rsp[0] = top };
}

/// Address of the emergency stack registered for interrupt stack table slot
/// `index` (zero-based, so slot 0 is IST1) on processor `cpu`.
#[must_use]
pub fn ist_stack(cpu: usize, index: usize) -> u64 {
    // SAFETY: reads a field of a static written during that processor's
    // bring-up and never again.
    unsafe { (*tss_ptr(cpu)).ist[index] }
}

/// Builds this processor's descriptor table, installs it, reloads every segment
/// register and loads the task register.
///
/// # Safety
///
/// Must run once per processor, with interrupts masked and with that
/// processor's emergency stacks already mapped. Reloading CS through a far
/// return requires that the code following the call remains mapped at the same
/// address, which holds because the kernel image mapping does not change here.
pub unsafe fn install(cpu: usize, ist_tops: [u64; layout::IST_COUNT]) {
    // 64-bit code and data descriptors. The base and limit fields are ignored
    // in long mode; the bits that matter are present, descriptor type, DPL,
    // long mode and, for data, writable.
    const KERNEL_CODE_DESC: u64 = 0x00AF_9A00_0000_FFFF;
    const KERNEL_DATA_DESC: u64 = 0x00CF_9200_0000_FFFF;
    const USER_DATA_DESC: u64 = 0x00CF_F200_0000_FFFF;
    const USER_CODE_DESC: u64 = 0x00AF_FA00_0000_FFFF;

    // SAFETY: this processor's own slot, with interrupts masked; no other
    // context touches it.
    unsafe {
        let tss = tss_ptr(cpu);
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

        let gdt = (&raw mut GDTS).cast::<Gdt>().add(cpu.min(MAX_CPUS - 1));
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
