//! Direct CPU access: ports, model-specific registers, control registers and
//! feature identification.
//!
//! Every function here is a thin wrapper over one instruction. The wrappers are
//! `unsafe` when the instruction can change machine state that Rust's type
//! system does not model, and none of them is re-exported behind a safe API
//! that could be used to break the state they configure.

use core::arch::asm;
use core::arch::x86_64::{__cpuid_count, _rdtsc};

/// `IA32_EFER`.
pub const MSR_EFER: u32 = 0xC000_0080;
/// `IA32_STAR`: SYSCALL/SYSRET segment selectors.
pub const MSR_STAR: u32 = 0xC000_0081;
/// `IA32_LSTAR`: 64-bit SYSCALL entry point.
pub const MSR_LSTAR: u32 = 0xC000_0082;
/// `IA32_FMASK`: RFLAGS bits cleared on SYSCALL.
pub const MSR_FMASK: u32 = 0xC000_0084;
/// `IA32_APIC_BASE`.
pub const MSR_APIC_BASE: u32 = 0x1B;

/// `IA32_EFER.SCE`, enables SYSCALL/SYSRET.
pub const EFER_SCE: u64 = 1 << 0;
/// `IA32_EFER.NXE`, enables the no-execute page bit.
pub const EFER_NXE: u64 = 1 << 11;

/// `CR0.MP`, monitor coprocessor.
pub const CR0_MP: u64 = 1 << 1;
/// `CR0.EM`, FPU emulation. Must be clear for SSE.
pub const CR0_EM: u64 = 1 << 2;
/// `CR0.TS`, task switched. Cleared: K1 saves FP state eagerly.
pub const CR0_TS: u64 = 1 << 3;
/// `CR0.NE`, native x87 exception reporting.
pub const CR0_NE: u64 = 1 << 5;
/// `CR0.WP`, supervisor write protection.
pub const CR0_WP: u64 = 1 << 16;

/// `CR4.OSFXSR`, enables FXSAVE/FXRSTOR and SSE.
pub const CR4_OSFXSR: u64 = 1 << 9;
/// `CR4.OSXMMEXCPT`, unmasked SIMD exceptions report as #XM.
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;
/// `CR4.SMEP`, supervisor mode execution prevention.
pub const CR4_SMEP: u64 = 1 << 20;
/// `CR4.SMAP`, supervisor mode access prevention.
pub const CR4_SMAP: u64 = 1 << 21;

/// Writes one byte to an I/O port.
///
/// # Safety
///
/// The caller must own the device behind `port` and know the effect of the
/// write. K1 drives only the legacy PIC, the PIT and a 16550 UART this way.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: `out` has no effect on memory or on Rust-visible state.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    }
}

/// Reads one byte from an I/O port.
///
/// # Safety
///
/// See [`outb`]. Reads can have side effects on the addressed device.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: `in` has no effect on memory or on Rust-visible state.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags))
    }
    value
}

/// Reads a model-specific register.
///
/// # Safety
///
/// `msr` must exist on this CPU; reading an unimplemented MSR raises #GP.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: the caller guarantees the MSR exists.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags))
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Writes a model-specific register.
///
/// # Safety
///
/// `msr` must exist and `value` must be legal for it. Several MSRs written here
/// (EFER, STAR, LSTAR, FMASK, APIC_BASE) change how the CPU dispatches
/// privilege transitions, so a wrong value is not a recoverable error.
#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees the MSR and the value.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        )
    }
}

/// Result of one `cpuid` leaf.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cpuid {
    /// EAX output.
    pub eax: u32,
    /// EBX output.
    pub ebx: u32,
    /// ECX output.
    pub ecx: u32,
    /// EDX output.
    pub edx: u32,
}

/// Executes `cpuid` for `leaf`/`subleaf`.
#[inline]
#[must_use]
pub fn cpuid(leaf: u32, subleaf: u32) -> Cpuid {
    let r = __cpuid_count(leaf, subleaf);
    Cpuid {
        eax: r.eax,
        ebx: r.ebx,
        ecx: r.ecx,
        edx: r.edx,
    }
}

/// Reads the time-stamp counter.
#[inline]
#[must_use]
pub fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` is available on every CPU this kernel accepts (checked in
    // the feature gate) and is readable at CPL 0 regardless of CR4.TSD.
    unsafe { _rdtsc() }
}

macro_rules! control_register {
    ($name:literal, $read:ident, $write:ident, $doc:literal) => {
        #[doc = $doc]
        #[inline]
        #[must_use]
        pub fn $read() -> u64 {
            let value: u64;
            // SAFETY: reading a control register has no side effect.
            unsafe { asm!(concat!("mov {}, ", $name), out(reg) value, options(nomem, nostack, preserves_flags)) }
            value
        }

        #[doc = concat!("Writes ", $name, ".\n\n# Safety\n\nThe value must keep the machine in a state this kernel can execute in. Paging, privilege checks and FP availability all depend on these registers.")]
        #[inline]
        pub unsafe fn $write(value: u64) {
            // SAFETY: the caller guarantees the value is legal for the current
            // machine state.
            unsafe { asm!(concat!("mov ", $name, ", {}"), in(reg) value, options(nostack, preserves_flags)) }
        }
    };
}

control_register!("cr0", read_cr0, write_cr0, "Reads CR0.");
control_register!("cr3", read_cr3, write_cr3, "Reads CR3.");
control_register!("cr4", read_cr4, write_cr4, "Reads CR4.");

/// Reads CR2, the linear address of the last page fault.
#[inline]
#[must_use]
pub fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: reading CR2 has no side effect.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) }
    value
}

/// Disables maskable interrupts.
///
/// # Safety
///
/// Callers must re-establish the interrupt state they promised to their own
/// callers. K1 runs every kernel path with interrupts masked and re-enables
/// them only by returning to user mode through `iretq`.
#[inline]
pub unsafe fn disable_interrupts() {
    // SAFETY: `cli` at CPL 0 with IOPL 0 is always permitted.
    unsafe { asm!("cli", options(nomem, nostack)) }
}

/// Halts the CPU with interrupts masked and never returns.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli` and `hlt` at CPL 0 are permitted; the loop is the
        // terminal state of the machine.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) }
    }
}

/// Invalidates one TLB entry.
///
/// # Safety
///
/// The caller must have already made the corresponding page-table change
/// visible; invalidating before the store would leave a stale translation.
#[inline]
pub unsafe fn invlpg(addr: u64) {
    // SAFETY: `invlpg` only affects the TLB.
    unsafe { asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags)) }
}

/// Feature bits this kernel checks before it commits to a platform profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    /// Long mode.
    pub long_mode: bool,
    /// Time-stamp counter.
    pub tsc: bool,
    /// Model-specific registers.
    pub msr: bool,
    /// Physical address extension.
    pub pae: bool,
    /// Local APIC present.
    pub apic: bool,
    /// Global page extension.
    pub pge: bool,
    /// Page attribute table.
    pub pat: bool,
    /// FXSAVE/FXRSTOR.
    pub fxsr: bool,
    /// SSE.
    pub sse: bool,
    /// SSE2.
    pub sse2: bool,
    /// No-execute page bit.
    pub nx: bool,
    /// SYSCALL/SYSRET.
    pub syscall: bool,
    /// 1 GiB pages.
    pub page1gb: bool,
    /// Supervisor mode execution prevention.
    pub smep: bool,
    /// Supervisor mode access prevention.
    pub smap: bool,
    /// Invariant TSC advertised by the CPU.
    pub invariant_tsc: bool,
    /// x2APIC supported.
    pub x2apic: bool,
}

impl Features {
    /// Reads every feature bit the V0 profile depends on.
    #[must_use]
    pub fn detect() -> Self {
        let max_basic = cpuid(0, 0).eax;
        let leaf1 = cpuid(1, 0);
        let max_ext = cpuid(0x8000_0000, 0).eax;
        let ext1 = if max_ext >= 0x8000_0001 {
            cpuid(0x8000_0001, 0)
        } else {
            Cpuid::default()
        };
        let ext7 = if max_ext >= 0x8000_0007 {
            cpuid(0x8000_0007, 0)
        } else {
            Cpuid::default()
        };
        let leaf7 = if max_basic >= 7 {
            cpuid(7, 0)
        } else {
            Cpuid::default()
        };

        Self {
            long_mode: ext1.edx & (1 << 29) != 0,
            tsc: leaf1.edx & (1 << 4) != 0,
            msr: leaf1.edx & (1 << 5) != 0,
            pae: leaf1.edx & (1 << 6) != 0,
            apic: leaf1.edx & (1 << 9) != 0,
            pge: leaf1.edx & (1 << 13) != 0,
            pat: leaf1.edx & (1 << 16) != 0,
            fxsr: leaf1.edx & (1 << 24) != 0,
            sse: leaf1.edx & (1 << 25) != 0,
            sse2: leaf1.edx & (1 << 26) != 0,
            nx: ext1.edx & (1 << 20) != 0,
            syscall: ext1.edx & (1 << 11) != 0,
            page1gb: ext1.edx & (1 << 26) != 0,
            smep: leaf7.ebx & (1 << 7) != 0,
            smap: leaf7.ebx & (1 << 20) != 0,
            invariant_tsc: ext7.edx & (1 << 8) != 0,
            x2apic: leaf1.ecx & (1 << 21) != 0,
        }
    }

    /// Features without which the V0 profile cannot be established at all.
    /// Absence is a refusal to run, never a silent downgrade.
    #[must_use]
    pub fn missing_mandatory(&self) -> &'static str {
        if !self.long_mode {
            "long_mode"
        } else if !self.msr {
            "msr"
        } else if !self.pae {
            "pae"
        } else if !self.tsc {
            "tsc"
        } else if !self.apic {
            "apic"
        } else if !self.fxsr {
            "fxsr"
        } else if !self.sse {
            "sse"
        } else if !self.sse2 {
            "sse2"
        } else if !self.nx {
            "nx"
        } else if !self.syscall {
            "syscall"
        } else {
            ""
        }
    }
}
