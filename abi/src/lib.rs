//! Thalyx-Kernel binary interface (ABI), V0.
//!
//! This crate holds the parts of the kernel ABI that K1 actually implements,
//! plus the descriptor header whose layout `vault/architecture/abi.md` already
//! fixes. It deliberately does **not** define the object, capability, memory or
//! IPC opcode space: that table is assigned in K2 from a single schema, and
//! publishing provisional numbers here would announce compatibility with
//! numbers that are not assigned yet.
//!
//! Two entry namespaces exist and they are not equivalent:
//!
//! * [`entry`] — assigned ABI entries. Only the reserved version query exists
//!   in V0. Numbers in this namespace are part of the interface.
//! * [`scaffold`] — K1 diagnostic scaffolding. Every identifier has bit 63 set,
//!   is documented as temporary, and is removed when K2 introduces the
//!   supervisor fault channel and the control-receipt plane. Nothing in this
//!   namespace is an ABI commitment.

#![no_std]

/// Assigned ABI entries. Stable numbering starts in K2; V0 assigns only the
/// reserved version query, which carries no authority over any object.
pub mod entry {
    /// Reserved entry that reports the interface version. Takes no handle and
    /// no descriptor, and never inspects user memory.
    pub const VERSION_QUERY: u64 = 0;
}

/// K1 diagnostic scaffolding. **Not ABI.**
///
/// K1 has no supervisor domain, no endpoints and no control-receipt objects, so
/// a user domain cannot yet report progress or terminate through the mechanisms
/// the architecture specifies. These two entries stand in for that, they carry
/// only register-sized integers (the kernel never dereferences a user pointer
/// on their behalf), and they are removed in K2.
pub mod scaffold {
    /// Bit set on every scaffolding entry so that a scaffolding call can never
    /// be mistaken for an assigned ABI entry.
    pub const MARKER: u64 = 1 << 63;

    /// Emit one diagnostic note on the kernel's diagnostic plane.
    ///
    /// `operation` is the note kind, `descriptor` and `len` carry two opaque
    /// u64 values. Returns the caller's domain identifier in the auxiliary
    /// value.
    pub const DIAG_NOTE: u64 = MARKER | 1;

    /// Terminate the calling domain voluntarily. `operation` carries an opaque
    /// exit code recorded in the diagnostic plane.
    pub const DIAG_EXIT: u64 = MARKER | 2;
}

/// Note kinds accepted by [`scaffold::DIAG_NOTE`]. K1 scaffolding.
pub mod note {
    /// Monotonic progress report. First value is the program's progress
    /// counter, second value is program-defined.
    pub const PROGRESS: u64 = 1;
    /// Result of a self-check the program performed on its own state.
    /// First value is a check identifier, second value is 1 for pass, 0 for
    /// fail.
    pub const SELF_CHECK: u64 = 2;
    /// The program is about to execute a deliberate illegal access. First
    /// value identifies the probe, second value is the target address.
    pub const PROBE_INTENT: u64 = 3;
}

/// Status returned in RAX. Zero is success; every error is negative.
///
/// V0 defines only the statuses K1 can actually produce. The remaining base
/// errors named in the ABI contract (invalid handle, wrong type, insufficient
/// rights, expired, closed scope, exhausted limit, full queue, dead peer,
/// cancellation, pending operation) require objects that do not exist yet and
/// are assigned with the K2 schema.
pub mod status {
    /// Operation completed.
    pub const OK: i64 = 0;
    /// The entry identifier is not implemented by this kernel revision.
    pub const UNSUPPORTED_ENTRY: i64 = -1;
    /// Arguments were structurally rejected before any effect.
    pub const INVALID_ARGUMENT: i64 = -2;
    /// The requested interface version is not supported.
    pub const INCOMPATIBLE_VERSION: i64 = -3;
}

/// Interface version reported by [`entry::VERSION_QUERY`].
pub const VERSION_MAJOR: u16 = 0;
/// Minor interface version. K1 is revision 1 of the V0 interface.
pub const VERSION_MINOR: u16 = 1;

/// Packs a version pair the way [`entry::VERSION_QUERY`] returns it in RDX.
#[must_use]
pub const fn pack_version(major: u16, minor: u16) -> u64 {
    ((major as u64) << 16) | (minor as u64)
}

/// Common descriptor header, 32 bytes, little-endian, explicit padding.
///
/// The layout is fixed by `vault/architecture/abi.md`. No operation in K1 uses
/// it; it is defined here so the layout has a machine-checked definition before
/// the first descriptor crosses the boundary in K2.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorHeader {
    /// Major interface version of the descriptor.
    pub major: u16,
    /// Minor interface version of the descriptor.
    pub minor: u16,
    /// Operation selector within the addressed object's type.
    pub opcode: u32,
    /// Operation flags; unknown bits are rejected, never ignored.
    pub flags: u32,
    /// Total descriptor length in bytes, header included.
    pub total_len: u32,
    /// Client correlation value. Not authorization and not durable dedupe.
    pub cookie: u64,
    /// Reserved, must be zero.
    pub reserved: u64,
}

const _: () = assert!(core::mem::size_of::<DescriptorHeader>() == 32);
const _: () = assert!(core::mem::align_of::<DescriptorHeader>() == 8);

/// Largest descriptor accepted by V0.
pub const MAX_DESCRIPTOR_LEN: u32 = 4096;
/// Largest inline IPC payload accepted by V0.
pub const MAX_INLINE_PAYLOAD_LEN: u32 = 256;

/// Raw kernel entry stub.
///
/// Register contract published by `vault/architecture/abi.md`: RAX carries the
/// entry, RDI a handle, RSI an operation, RDX a descriptor pointer, R10 its
/// length, R8 flags and R9 a monotonic deadline. RAX returns the status and RDX
/// an auxiliary value defined by the operation. RCX and R11 are destroyed by
/// the `syscall` instruction itself.
///
/// # Safety
///
/// The caller is responsible for the meaning of the arguments for the entry it
/// selects. For every entry K1 implements, all arguments are plain integers and
/// the kernel dereferences none of them, so no memory precondition applies
/// there; entries that take descriptor pointers do not exist yet.
///
/// The kernel switches to a private stack on entry and never reads or writes
/// the caller's stack, which is what makes `nostack` correct here.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn invoke(
    entry: u64,
    handle: u64,
    operation: u64,
    descriptor: u64,
    len: u64,
    flags: u64,
    deadline: u64,
) -> (i64, u64) {
    let status: i64;
    let aux: u64;
    // SAFETY: the caller guarantees the argument contract of the selected
    // entry. The clobber list matches the published stub contract: RCX and R11
    // are destroyed by `syscall`, RAX and RDX are overwritten with the results,
    // and every other named register is an input the kernel does not modify.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") entry => status,
            in("rdi") handle,
            in("rsi") operation,
            inlateout("rdx") descriptor => aux,
            in("r10") len,
            in("r8") flags,
            in("r9") deadline,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (status, aux)
}
