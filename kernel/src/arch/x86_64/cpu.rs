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
/// `IA32_FS_BASE`: base of the `FS` segment. The kernel never addresses
/// through it; it holds the running user thread's own thread pointer.
pub const MSR_FS_BASE: u32 = 0xC000_0100;
/// `IA32_GS_BASE`: base of the `GS` segment at the current privilege level.
pub const MSR_GS_BASE: u32 = 0xC000_0101;
/// `IA32_KERNEL_GS_BASE`: the base `swapgs` exchanges with `IA32_GS_BASE`.
pub const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;
/// Base of the x2APIC register block in the model-specific register space.
pub const MSR_X2APIC_BASE: u32 = 0x800;

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
/// `CR0.NW`, not write-through. Set by INIT; must be clear for normal caching.
pub const CR0_NW: u64 = 1 << 29;
/// `CR0.CD`, cache disable. Set by INIT; must be clear for normal caching.
pub const CR0_CD: u64 = 1 << 30;

/// `CR4.OSFXSR`, enables FXSAVE/FXRSTOR and SSE.
pub const CR4_OSFXSR: u64 = 1 << 9;
/// `CR4.OSXMMEXCPT`, unmasked SIMD exceptions report as #XM.
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;
/// `CR4.PGE`, global page extension. Never enabled: this kernel marks no
/// mapping global, which is what makes a `CR3` reload a complete invalidation.
pub const CR4_PGE: u64 = 1 << 7;
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

/// The local APIC identifier of the processor executing this call, from
/// `CPUID`.
///
/// Read here rather than from the controller's own register because it is
/// available before anything is mapped or enabled, and because the two
/// interfaces report the identifier differently: the eight-bit initial
/// identifier of leaf 1 is not the 32-bit x2APIC identifier of leaf 0x0B, and
/// treating one as the other renames processors above 255.
#[must_use]
pub fn apic_id_from_cpuid(x2apic: bool) -> u32 {
    if x2apic && cpuid(0, 0).eax >= 0x0B {
        return cpuid(0x0B, 0).edx;
    }
    cpuid(1, 0).ebx >> 24
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

/// Waits for the next interrupt with interrupts enabled, then masks them again.
///
/// This is the only place in the kernel where interrupts are enabled outside
/// user mode. It exists because a kernel that has nothing to run but something
/// to wait for — a deadline, a timer — has to let the timer interrupt in. The
/// `sti; hlt` pair is atomic with respect to interrupt delivery: `sti` takes
/// effect after the following instruction, so an interrupt cannot arrive in the
/// gap and leave the CPU halted with nothing to wake it.
///
/// # Safety
///
/// The caller must hold no lock: the interrupt handler that runs here takes the
/// machine lock, and it may switch away from this context entirely.
#[inline]
pub unsafe fn wait_for_interrupt() {
    // SAFETY: `sti`, `hlt` and `cli` at CPL 0 are permitted, and the caller
    // guarantees no lock is held across the window.
    unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) }
}

/// Runs `body` with interrupts enabled, then masks them again.
///
/// The second place interrupts are enabled outside user mode, with the same
/// justification and the same condition as [`wait_for_interrupt`]: an idle
/// processor that watches for a wake instead of halting at once has to let the
/// interrupts in that a halted one would have taken -- a timer, a shootdown,
/// a reschedule.
///
/// # Safety
///
/// The caller must hold no lock: an interrupt handler that runs here takes
/// the machine lock, and it may switch away from this context entirely.
#[inline]
pub unsafe fn with_interrupts_enabled<T>(body: impl FnOnce() -> T) -> T {
    // SAFETY: `sti` and `cli` at CPL 0 are permitted, and the caller
    // guarantees no lock is held across the window.
    unsafe { asm!("sti", options(nomem, nostack)) };
    let value = body();
    // SAFETY: as above.
    unsafe { asm!("cli", options(nomem, nostack)) };
    value
}

/// Halts the CPU with interrupts masked and never returns.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `cli` and `hlt` at CPL 0 are permitted; the loop is the
        // terminal state of the machine.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) }
    }
}

/// Invalidates every translation this processor caches, global ones included.
///
/// A `CR3` reload retires every non-global entry and every paging-structure
/// cache entry for the space it names, which is the user half: this kernel
/// marks every kernel-half leaf global and no user-half leaf. The global
/// entries are retired by turning `CR4.PGE` off and on, and on a virtual
/// machine those two writes are the expensive kind, so this is for a
/// withdrawal in the kernel half; [`flush_tlb_user`] is the ordinary case.
///
/// # Safety
///
/// The value in `CR3` must still be the root of an address space in which the
/// code and stack executing here are mapped, which holds for every address
/// space this kernel builds because they all share the kernel's upper half.
#[inline]
pub unsafe fn flush_tlb_all() {
    // A `CR3` write retires every non-global entry; the kernel half is
    // global, and clearing and restoring `CR4.PGE` is the architecturally
    // defined way to retire those as well. Both writes keep every other bit.
    //
    // SAFETY: the values written back are the ones already loaded, apart
    // from the paging-global bit going off and on, which changes no
    // translation.
    unsafe {
        let cr4 = read_cr4();
        write_cr4(cr4 & !CR4_PGE);
        write_cr4(cr4);
    }
}

/// Invalidates every cached translation of the user half: the non-global
/// entries, which are the ones a `CR3` reload retires.
///
/// # Safety
///
/// As [`flush_tlb_all`].
#[inline]
pub unsafe fn flush_tlb_user() {
    // SAFETY: writing back the value already loaded changes no translation and
    // is the architecturally defined way to invalidate the non-global ones.
    unsafe { write_cr3(read_cr3()) }
}

/// Orders every earlier store and load before the next instruction.
///
/// `WRMSR` to an x2APIC register is not a serialising instruction, so a store
/// that publishes a request in memory is not automatically ordered before the
/// interrupt that announces it. The fence pair is the ordering the SDM
/// prescribes for exactly that case.
#[inline]
pub fn fence_before_wrmsr() {
    // SAFETY: fences change no architectural state beyond ordering.
    unsafe { asm!("mfence", "lfence", options(nostack, preserves_flags)) }
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
