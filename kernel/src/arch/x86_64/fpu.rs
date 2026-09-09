//! x87 and SSE state.
//!
//! K1 saves and restores the whole legacy area eagerly on every context switch,
//! as the platform contract requires. Lazy switching through `CR0.TS` would be
//! cheaper and is exactly the mechanism that has historically leaked register
//! contents between protection domains, so it is not used and `CR0.TS` stays
//! clear. Extended state beyond SSE is not enabled: `XSAVE` areas are not saved
//! here, so enabling AVX would silently drop or leak the wide registers.

use core::arch::asm;

use super::cpu;

/// Legacy `FXSAVE` area.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct FpuState {
    bytes: [u8; 512],
}

const _: () = assert!(core::mem::size_of::<FpuState>() == 512);
const _: () = assert!(core::mem::align_of::<FpuState>() == 16);

impl FpuState {
    /// A zeroed area. Not a legal restore image; use [`initial`] instead.
    #[must_use]
    pub const fn zeroed() -> Self {
        Self { bytes: [0; 512] }
    }
}

static mut TEMPLATE: FpuState = FpuState::zeroed();

/// Default MXCSR: every SIMD exception masked, no flush-to-zero, round to
/// nearest. Reserved bits clear, so `fxrstor` of this image cannot raise #GP.
pub const DEFAULT_MXCSR: u32 = 0x1F80;

/// Enables FP and SSE and captures the template new threads start from.
///
/// # Safety
///
/// Must run once during bootstrap, after the CPU feature gate has confirmed
/// FXSR and SSE2. Enabling `CR4.OSFXSR` without those is undefined.
pub unsafe fn init() {
    // SAFETY: bootstrap path with the feature gate already passed.
    unsafe {
        let mut cr0 = cpu::read_cr0();
        cr0 &= !(cpu::CR0_EM | cpu::CR0_TS);
        cr0 |= cpu::CR0_MP | cpu::CR0_NE;
        cpu::write_cr0(cr0);

        let cr4 = cpu::read_cr4() | cpu::CR4_OSFXSR | cpu::CR4_OSXMMEXCPT;
        cpu::write_cr4(cr4);

        let mxcsr = DEFAULT_MXCSR;
        asm!("fninit", "ldmxcsr [{}]", in(reg) &mxcsr, options(readonly, nostack));
        let template = &raw mut TEMPLATE;
        asm!("fxsave [{}]", in(reg) template, options(nostack));
    }
}

/// A legal starting image: the state captured right after `fninit`, identical
/// for every thread and carrying no bytes from any previous owner.
#[must_use]
pub fn initial() -> FpuState {
    // SAFETY: `TEMPLATE` is written once during bootstrap and read-only
    // afterwards on a uniprocessor machine.
    unsafe { *(&raw const TEMPLATE) }
}

/// Saves the current FP state into `state`.
///
/// # Safety
///
/// `state` must be a live, 16-byte-aligned 512-byte area owned by the caller.
pub unsafe fn save(state: *mut FpuState) {
    // SAFETY: the caller guarantees the area.
    unsafe { asm!("fxsave [{}]", in(reg) state, options(nostack)) }
}

/// Restores FP state from `state`.
///
/// # Safety
///
/// `state` must hold an image produced by `fxsave` on this machine, either the
/// bootstrap template or a previous [`save`]. An arbitrary buffer can carry a
/// reserved MXCSR bit and raise #GP inside the context switch.
pub unsafe fn restore(state: *const FpuState) {
    // SAFETY: the caller guarantees the image.
    unsafe { asm!("fxrstor [{}]", in(reg) state, options(readonly, nostack)) }
}
