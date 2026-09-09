//! Thalyx-Kernel binary interface (ABI), V0.
//!
//! The interface is generated, not written. [`abi/schema/v0.json`] is the single
//! place where an entry number, an operation code, a rights bit, an error value
//! or a structure layout is decided; `tools/gen_abi.py` emits the Rust bindings
//! in [`generated`], the layout fixtures in [`fixture`] and the C header in
//! `abi/include/thalyx_abi.h` from it. `tools/check_abi.py` regenerates all
//! three and fails when a committed file differs, so the three languages cannot
//! drift apart quietly.
//!
//! [`abi/schema/v0.json`]: https://github.com/CesarManzoCode/thalyx-kernel/blob/main/abi/schema/v0.json
//!
//! Three namespaces exist and they are not equivalent:
//!
//! * [`generated`] — the assigned V0 interface. Re-exported at the crate root.
//! * [`fixture`] — encoded sample structures and the values they must decode to.
//! * [`scaffold`] — K1 diagnostic scaffolding, kept only so the K1 regression
//!   image stays executable. No K2 program uses it, and every identifier in it
//!   has bit 63 set so it can never be confused with an assigned entry.

#![no_std]

pub mod fixture;
pub mod generated;

pub use generated::*;

/// K1 diagnostic scaffolding. **Not ABI.**
///
/// K1 had no supervisor, no endpoints and no control-receipt objects, so a user
/// domain could not report progress or terminate through the mechanisms the
/// architecture specifies. These two entries stood in for that. K2 provides the
/// real mechanisms — [`generated::entry::EXIT`], endpoints and the control log —
/// and no K2 domain invokes anything here. They remain because the K1 evidence
/// run must stay reproducible on the kernel that K2 leaves behind; the K2 gate
/// refuses any run in which a K2 domain touched them.
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

/// Largest descriptor accepted by V0.
pub const MAX_DESCRIPTOR_LEN: u32 = generated::limit::MAX_DESCRIPTOR_LEN as u32;
/// Largest inline IPC payload accepted by V0.
pub const MAX_INLINE_PAYLOAD_LEN: u32 = generated::limit::MAX_INLINE_PAYLOAD as u32;
/// Capabilities one message may carry.
pub const MAX_MESSAGE_CAPS: usize = generated::limit::MAX_CAPS_PER_MESSAGE as usize;

/// Packs a version pair the way [`generated::entry::VERSION_QUERY`] returns it
/// in RDX.
#[must_use]
pub const fn pack_version(major: u16, minor: u16) -> u64 {
    ((major as u64) << 16) | (minor as u64)
}

/// Builds a handle from a table slot and a generation.
///
/// Zero is never a valid handle: generations start at one, so no live entry can
/// encode to zero even in slot zero.
#[must_use]
pub const fn handle(slot: u32, generation: u32) -> u64 {
    ((generation as u64) << 32) | (slot as u64)
}

/// Table slot a handle names.
#[must_use]
pub const fn handle_slot(value: u64) -> u32 {
    (value & 0xFFFF_FFFF) as u32
}

/// Generation a handle carries.
#[must_use]
pub const fn handle_generation(value: u64) -> u32 {
    (value >> 32) as u32
}

/// Handle of a capability the kernel or a supervisor installed in a slot that
/// had never been used before, whose generation is therefore one.
#[must_use]
pub const fn boot_handle(slot: u32) -> u64 {
    handle(slot, 1)
}

const _: () = assert!(boot_handle(0) != 0);

/// Object type a capability names, taken from an operation code.
#[must_use]
pub const fn op_type(operation: u32) -> u32 {
    operation >> 16
}

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
/// For an entry that takes a descriptor, `descriptor` must point at `len`
/// readable bytes of the caller's memory, and at `len` writable bytes when the
/// operation returns one. The kernel validates the range and refuses it rather
/// than faulting, so a wrong pointer is an error and not undefined behaviour on
/// the kernel side; it is still the caller's own memory that is read or
/// written, which is why this is `unsafe`.
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
