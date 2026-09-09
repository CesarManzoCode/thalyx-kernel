//! Per-processor state reachable without a lock.
//!
//! Two different things are per-CPU and they live in two different places. The
//! scheduler's view of a processor — which thread it runs, where its cursor is,
//! what it has charged — is kernel metadata and lives in `Machine`, under the
//! machine lock, like every other object. What lives *here* is the small block
//! the entry paths need **before** they can take a lock or even own a stack:
//! the kernel stack the `syscall` instruction switches to, the scratch slot
//! that holds the interrupted user stack pointer for the two instructions of
//! that switch, and the processor's own identity.
//!
//! It is reached through `GS`. The convention is the usual one and is stated
//! here because getting it wrong is silent: while the processor is at CPL 0,
//! `IA32_GS_BASE` holds this block and `IA32_KERNEL_GS_BASE` holds what user
//! mode had; `swapgs` exchanges them on every user→kernel entry and on every
//! kernel→user return, and on no other path. An entry from CPL 0 does not
//! swap, because the base is already the kernel's.
//!
//! That leaves one window with no correct answer: a non-maskable interrupt or a
//! machine check arriving at the first instruction of the `syscall` entry, when
//! the processor is at CPL 0 with the *user's* `GS` still loaded. Nothing here
//! is used on that path. Those three vectors are diverted in `crate::trap`
//! before any per-CPU access, and the identity they report comes from
//! [`index_by_apic_id`], which reads the processor's own interrupt controller
//! instead of a base register that may not be ours.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::arch::x86_64::cpu;
use crate::limits::MAX_CPUS;

/// The block one processor reaches through `GS`.
///
/// `repr(C)` and the field order are load-bearing: the entry stubs address
/// `kernel_rsp` and `user_rsp` by literal offset, and the assertions below are
/// what keep the two definitions from drifting apart.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct PerCpu {
    /// Address of this block, so `gs:[0]` yields a usable pointer.
    pub self_ptr: u64,
    /// Top of the kernel stack the `syscall` entry switches to. Kept equal to
    /// the task state segment's ring 0 pointer by the context switch.
    pub kernel_rsp: u64,
    /// Scratch slot holding the interrupted user stack pointer across the two
    /// instructions of the `syscall` stack switch.
    pub user_rsp: u64,
    /// Dense index of this processor in the kernel's own tables.
    pub cpu_index: u64,
    /// Local APIC identifier, as the firmware reported it.
    pub apic_id: u64,
}

/// Byte offset of [`PerCpu::kernel_rsp`], as the entry stubs address it.
pub const OFFSET_KERNEL_RSP: usize = 8;
/// Byte offset of [`PerCpu::user_rsp`], as the entry stubs address it.
pub const OFFSET_USER_RSP: usize = 16;
/// Byte offset of [`PerCpu::cpu_index`].
pub const OFFSET_CPU_INDEX: usize = 24;

const _: () = assert!(core::mem::offset_of!(PerCpu, kernel_rsp) == OFFSET_KERNEL_RSP);
const _: () = assert!(core::mem::offset_of!(PerCpu, user_rsp) == OFFSET_USER_RSP);
const _: () = assert!(core::mem::offset_of!(PerCpu, cpu_index) == OFFSET_CPU_INDEX);

impl PerCpu {
    const fn empty() -> Self {
        Self {
            self_ptr: 0,
            kernel_rsp: 0,
            user_rsp: 0,
            cpu_index: 0,
            apic_id: 0,
        }
    }
}

// One block per processor slot. Each block is written by its own processor
// during bring-up and, before that, by the bootstrap processor which is the
// only context that exists at the time. Afterwards a block is written only by
// the processor it belongs to.
static mut BLOCKS: [PerCpu; MAX_CPUS] = [PerCpu::empty(); MAX_CPUS];

/// Local APIC identifier of each slot, published as the slot is claimed.
static APIC_IDS: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(u32::MAX) }; MAX_CPUS];
/// Slots claimed so far.
static CLAIMED: AtomicUsize = AtomicUsize::new(0);

/// Pointer to the block of `index`.
#[must_use]
pub fn block(index: usize) -> *mut PerCpu {
    // SAFETY: the array is a static of fixed length and the caller's index is
    // bounded below.
    unsafe {
        (&raw mut BLOCKS)
            .cast::<PerCpu>()
            .add(index.min(MAX_CPUS - 1))
    }
}

/// Records the identity of slot `index` before the processor it names runs.
///
/// Called by the bootstrap processor for every slot, including its own, while
/// it is still the only context in the machine.
pub fn claim(index: usize, apic_id: u32) {
    if index >= MAX_CPUS {
        return;
    }
    let block = block(index);
    // SAFETY: the bootstrap processor is the only context running when slots
    // are claimed, and each slot is claimed once.
    unsafe {
        (*block).self_ptr = block as u64;
        (*block).cpu_index = index as u64;
        (*block).apic_id = u64::from(apic_id);
        (*block).kernel_rsp = 0;
        (*block).user_rsp = 0;
    }
    APIC_IDS[index].store(apic_id, Ordering::Release);
    let claimed = CLAIMED.load(Ordering::Relaxed);
    if index + 1 > claimed {
        CLAIMED.store(index + 1, Ordering::Release);
    }
}

/// Loads this processor's block into `GS` and clears the user-side base.
///
/// # Safety
///
/// Must run once per processor, at CPL 0, with interrupts masked, before any
/// path that reads `GS` and before the first return to user mode. The slot must
/// already have been claimed.
pub unsafe fn install(index: usize) {
    let block = block(index) as u64;
    // SAFETY: both bases are ordinary MSRs on every CPU in long mode, and the
    // value is the address of a static this kernel owns. `IA32_KERNEL_GS_BASE`
    // is set to zero because user mode has no per-thread base in V0, so the
    // first `swapgs` on the way out leaves user mode with a zero base and this
    // block parked where the next entry's `swapgs` will find it.
    unsafe {
        cpu::wrmsr(cpu::MSR_GS_BASE, block);
        cpu::wrmsr(cpu::MSR_KERNEL_GS_BASE, 0);
    }
}

/// Index of the processor executing this code.
///
/// Valid only at CPL 0 on a processor that has run [`install`], which is every
/// kernel path except the three diverted vectors described in the module
/// documentation.
#[inline]
#[must_use]
pub fn index() -> usize {
    let value: u64;
    // SAFETY: reads one quadword of this processor's own block through `GS`,
    // whose base this kernel set. The block is `repr(C)` and the offset is
    // checked against the field above.
    unsafe {
        core::arch::asm!(
            "mov {}, gs:[{off}]",
            out(reg) value,
            off = const OFFSET_CPU_INDEX,
            options(nostack, preserves_flags),
        );
    }
    value as usize
}

/// Points this processor's `syscall` entry at a kernel stack.
///
/// # Safety
///
/// `top` must be the top of the mapped kernel stack of the thread about to run
/// on this processor, and interrupts must be masked.
pub unsafe fn set_kernel_rsp(top: u64) {
    // SAFETY: writes one quadword of this processor's own block through `GS`.
    // Only this processor and its own entry stubs read it.
    unsafe {
        core::arch::asm!(
            "mov gs:[{off}], {}",
            in(reg) top,
            off = const OFFSET_KERNEL_RSP,
            options(nostack, preserves_flags),
        );
    }
}

/// Slot holding `apic_id`, or `None`.
///
/// Used by the paths that must not trust `GS`: the non-maskable interrupt, the
/// machine check, the double fault and the panic that follows any of them.
#[must_use]
pub fn index_by_apic_id(apic_id: u32) -> Option<usize> {
    let claimed = CLAIMED.load(Ordering::Acquire).min(MAX_CPUS);
    (0..claimed).find(|&index| APIC_IDS[index].load(Ordering::Acquire) == apic_id)
}

/// Slots claimed so far.
#[must_use]
pub fn claimed() -> usize {
    CLAIMED.load(Ordering::Acquire).min(MAX_CPUS)
}
