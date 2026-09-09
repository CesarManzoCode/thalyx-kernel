//! Minimal user-side runtime for K1 test domains.
//!
//! This is not the platform library of `runtime/`. It has no heap, no I/O, no
//! threads and no compatibility layer; it exists because K1 needs user code
//! that can enter, report and stop, and duplicating that in every test domain
//! would hide the ABI boundary rather than expose it. It is replaced, not
//! extended, when the real user runtime arrives.

#![no_std]

pub mod k2;

use thalyx_abi::{note, scaffold, status};

/// Emits one diagnostic note. Returns the calling domain's identifier.
///
/// K1 scaffolding: this is a diagnostic channel, not IPC and not a control
/// receipt. Events emitted through it may be coalesced by the kernel.
pub fn diag_note(kind: u64, a: u64, b: u64) -> u64 {
    // SAFETY: `DIAG_NOTE` takes four integers and no pointer. The kernel never
    // dereferences any of them, so there is no memory precondition to uphold.
    let (st, aux) = unsafe { thalyx_abi::invoke(scaffold::DIAG_NOTE, 0, kind, a, b, 0, 0) };
    if st == status::OK { aux } else { u64::MAX }
}

/// Reports progress. `counter` must increase monotonically within a domain.
pub fn progress(counter: u64, payload: u64) -> u64 {
    diag_note(note::PROGRESS, counter, payload)
}

/// Reports the result of a self-check the program ran on its own state.
pub fn self_check(check: u64, passed: bool) {
    diag_note(note::SELF_CHECK, check, u64::from(passed));
}

/// Announces a deliberate illegal access before performing it, so the fault
/// that follows can be matched to an intent instead of inferred.
pub fn probe_intent(probe: u64, target: u64) {
    diag_note(note::PROBE_INTENT, probe, target);
}

/// Self-check identifiers carried in the first value of a `SELF_CHECK` note.
/// They are defined once here so a program, the kernel record and the gate all
/// refer to the same number.
pub mod check {
    /// The kernel reported the interface version the program was built for.
    pub const ABI_VERSION: u64 = 0x01;
    /// The FP pattern the domain planted. Second value is the pattern itself.
    pub const FP_PATTERN: u64 = 0x02;
    /// The program's workload produced a live result.
    pub const WORK_OBSERVED: u64 = 0x03;
    /// Reached code that a working protection boundary makes unreachable.
    pub const UNREACHABLE: u64 = 0xFE;
}

/// Establishes a domain's identity and FP state.
///
/// The FP pattern is derived from the domain identifier the kernel returns, so
/// two domains built from the same image still plant different values. Without
/// that, "each domain found its own pattern intact" would be satisfied by a
/// kernel that never switched FP state at all.
///
/// Returns the pattern, which the caller checks after each interval of work.
pub fn establish(fp_base: u64) -> u64 {
    let (major, minor) = abi_version();
    let matches = major == thalyx_abi::VERSION_MAJOR && minor == thalyx_abi::VERSION_MINOR;
    let domain = diag_note(note::SELF_CHECK, check::ABI_VERSION, u64::from(matches));
    let pattern = fp_base ^ domain.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    diag_note(note::SELF_CHECK, check::FP_PATTERN, pattern);
    fp::seed(pattern);
    pattern
}

/// Queries the kernel ABI version. Returns `(major, minor)`.
pub fn abi_version() -> (u16, u16) {
    // SAFETY: the version query takes no handle, no descriptor and no pointer.
    let (st, packed) =
        unsafe { thalyx_abi::invoke(thalyx_abi::entry::VERSION_QUERY, 0, 0, 0, 0, 0, 0) };
    if st != status::OK {
        return (u16::MAX, u16::MAX);
    }
    (((packed >> 16) & 0xFFFF) as u16, (packed & 0xFFFF) as u16)
}

/// Terminates the calling domain voluntarily.
pub fn exit(code: u64) -> ! {
    loop {
        // SAFETY: `DIAG_EXIT` takes integers only. It does not return on
        // success; the loop covers a kernel that rejected the entry.
        unsafe {
            thalyx_abi::invoke(scaffold::DIAG_EXIT, 0, code, 0, 0, 0, 0);
        }
        core::hint::spin_loop();
    }
}

/// Integer work with a loop-carried dependency, so it cannot be vectorised or
/// folded away and the domain genuinely occupies the CPU between notes.
///
/// The return value must be consumed by the caller; otherwise the whole loop is
/// dead code.
#[inline(never)]
#[must_use]
pub fn burn(iterations: u64, seed: u64) -> u64 {
    let mut x = seed | 1;
    let mut i = 0u64;
    while i < iterations {
        // xorshift64: every step depends on the previous one.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        i += 1;
    }
    x
}

/// Defines the ELF entry point of a K1 user domain.
///
/// The kernel enters at `_start` with a 16-byte-aligned stack minus eight
/// bytes, which is the alignment a System V function sees just after a `call`,
/// and with every other general-purpose register zeroed.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn _start() -> ! {
            let main: fn() -> ! = $main;
            main()
        }
    };
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    diag_note(note::SELF_CHECK, u64::MAX, 0);
    exit(u64::MAX)
}

/// x87/SSE2 state probes.
///
/// K1 saves and restores the whole legacy FP/SSE area eagerly on every context
/// switch. These helpers let a domain plant a value only it knows and check
/// later that the value survived preemption unchanged, which is the observable
/// half of INV-22. They cannot show the absence of a leak in the other
/// direction on their own; a second domain planting a different pattern and
/// finding its own is what makes the pair meaningful.
pub mod fp {
    /// Number of 128-bit registers this probe covers.
    pub const REGISTERS: usize = 8;

    fn pattern_words(pattern: u64) -> [u64; REGISTERS * 2] {
        let mut words = [0u64; REGISTERS * 2];
        let mut i = 0;
        while i < REGISTERS * 2 {
            words[i] =
                pattern.rotate_left((i as u32) * 7) ^ (0x0101_0101_0101_0101u64 * (i as u64 + 1));
            i += 1;
        }
        words
    }

    /// Builds a valid MXCSR value that differs per pattern: all exceptions
    /// masked, reserved bits clear, only the rounding-control field varies.
    #[must_use]
    pub const fn mxcsr_for(pattern: u64) -> u32 {
        0x1F80 | (((pattern & 0b11) as u32) << 13)
    }

    /// Loads `pattern` into xmm0..xmm7 and MXCSR.
    pub fn seed(pattern: u64) {
        let words = pattern_words(pattern);
        let mxcsr = mxcsr_for(pattern);
        // SAFETY: the target is built with `-sse,+soft-float`, so the compiler
        // never allocates an xmm register or reads MXCSR, and clobbering them
        // without declaring them cannot invalidate any value it is tracking.
        // The only memory the block touches is `words`, read through `buf` for
        // exactly 128 bytes, and `mxcsr`, read for four bytes. SSE is usable
        // because the kernel enables CR4.OSFXSR and clears CR0.EM before any
        // user instruction runs; if it did not, this would fault as #UD and be
        // contained like any other user fault.
        unsafe {
            core::arch::asm!(
                "ldmxcsr [{csr}]",
                "movdqu xmm0, [{buf} + 0]",
                "movdqu xmm1, [{buf} + 16]",
                "movdqu xmm2, [{buf} + 32]",
                "movdqu xmm3, [{buf} + 48]",
                "movdqu xmm4, [{buf} + 64]",
                "movdqu xmm5, [{buf} + 80]",
                "movdqu xmm6, [{buf} + 96]",
                "movdqu xmm7, [{buf} + 112]",
                buf = in(reg) words.as_ptr(),
                csr = in(reg) &mxcsr,
                options(readonly, nostack, preserves_flags),
            );
        }
    }

    /// Returns true when xmm0..xmm7 and MXCSR still hold `pattern`.
    #[must_use]
    pub fn verify(pattern: u64) -> bool {
        let mut observed = [0u64; REGISTERS * 2];
        let mut mxcsr: u32 = 0;
        // SAFETY: same reasoning as `seed`. The block writes exactly 128 bytes
        // through `buf` and four bytes through `csr`, both of which point at
        // live local storage of at least that size.
        unsafe {
            core::arch::asm!(
                "movdqu [{buf} + 0], xmm0",
                "movdqu [{buf} + 16], xmm1",
                "movdqu [{buf} + 32], xmm2",
                "movdqu [{buf} + 48], xmm3",
                "movdqu [{buf} + 64], xmm4",
                "movdqu [{buf} + 80], xmm5",
                "movdqu [{buf} + 96], xmm6",
                "movdqu [{buf} + 112], xmm7",
                "stmxcsr [{csr}]",
                buf = in(reg) observed.as_mut_ptr(),
                csr = in(reg) &mut mxcsr,
                options(nostack, preserves_flags),
            );
        }
        observed == pattern_words(pattern) && mxcsr == mxcsr_for(pattern)
    }
}

/// Deliberate probes of the protection boundary.
///
/// Every probe is written as inline assembly so the access reaches the CPU
/// exactly as written. Expressed as Rust loads and stores these would be
/// undefined behaviour, and the compiler would be free to delete or reorder the
/// very instruction the experiment is about.
pub mod probe {
    /// Reads eight bytes from `addr`.
    ///
    /// # Safety
    ///
    /// The caller is asserting nothing about `addr`: the point of this function
    /// is to execute a load the domain is not entitled to perform. It is
    /// `unsafe` because a caller that passes a legitimate address gets a real
    /// load with all the usual obligations.
    pub unsafe fn read_u64(addr: u64) -> u64 {
        let value: u64;
        // SAFETY: the block performs one load from `addr` and writes no memory.
        unsafe {
            core::arch::asm!(
                "mov {out}, qword ptr [{addr}]",
                addr = in(reg) addr,
                out = out(reg) value,
                options(readonly, nostack, preserves_flags),
            );
        }
        value
    }

    /// Writes one byte to `addr`.
    ///
    /// # Safety
    ///
    /// See [`read_u64`]. This exists to execute a store the domain is not
    /// entitled to perform.
    pub unsafe fn write_u8(addr: u64, value: u8) {
        // SAFETY: the block performs one store to `addr` and reads no other
        // memory.
        unsafe {
            core::arch::asm!(
                "mov byte ptr [{addr}], {value}",
                addr = in(reg) addr,
                value = in(reg_byte) value,
                options(nostack, preserves_flags),
            );
        }
    }
}
