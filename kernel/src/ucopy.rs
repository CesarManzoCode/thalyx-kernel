//! Copying between the kernel and a domain's memory.
//!
//! Two rules from the IPC and ABI contracts decide the shape of this module.
//! First, a descriptor is copied once into kernel memory and validated there:
//! validating a structure in memory the caller can still write is validating
//! something that may no longer be what is used. Second, an access to a user
//! buffer may fail and must never turn into an arbitrary kernel read.
//!
//! The failure rule is met by resolving every page through the domain's own
//! page tables and copying through the direct map, rather than by dereferencing
//! the user address and recovering from a fault. That means the kernel never
//! takes a page fault on a user pointer at all: a bad address is an error
//! before the first byte moves. It also means SMAP stays enabled with nothing
//! to disable, because the kernel touches the frame, not the user mapping.
//!
//! What copying once does **not** buy is a coherent snapshot of anything the
//! descriptor refers to. The bytes are a message from an adversary: every
//! field, length, offset and reserved word is checked after the copy, and a
//! buffer the descriptor points at is still free to change.

use thalyx_boot_protocol::{PAGE_SIZE, USER_MAX_ADDR, USER_MIN_ADDR};

use crate::arch::x86_64::paging::{AddressSpace, FLAG_USER, FLAG_WRITABLE};

/// Largest descriptor any assigned operation uses.
///
/// The interface allows 4 KiB; no operation of this revision needs more than
/// this, and the difference is what keeps the staging buffer small enough to
/// live on a kernel stack. An operation that grew past it would fail the
/// assertion below rather than overflow anything.
pub const MAX_OP_DESCRIPTOR: usize = 512;

const _: () = {
    let mut index = 0;
    while index < thalyx_abi::generated::OPERATIONS.len() {
        assert!(
            thalyx_abi::generated::OPERATIONS[index].descriptor_len as usize <= MAX_OP_DESCRIPTOR
        );
        index += 1;
    }
};

/// Staging buffer a descriptor is copied into before it is validated.
///
/// Aligned so a generated structure can be read out of it without an unaligned
/// access, and sized so it fits comfortably on a kernel stack.
#[repr(C, align(8))]
pub struct Staging {
    /// The bytes.
    pub bytes: [u8; MAX_OP_DESCRIPTOR],
    /// Whether a handler has written a response into it.
    ///
    /// A refusal usually leaves the buffer untouched, and returning zeros to a
    /// caller that asked for a report would be worse than returning nothing.
    /// Some operations, though, refuse *with* an explanation -- a retirement
    /// that cannot proceed says what is still outstanding -- and the answer is
    /// useless if the dispatcher discards it because the status was negative.
    /// This is what tells the two apart.
    pub filled: bool,
}

impl Staging {
    /// An empty staging area.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_OP_DESCRIPTOR],
            filled: false,
        }
    }

    /// Reads a structure out of the buffer at `offset`.
    ///
    /// Every generated structure is made of integers and arrays of integers, so
    /// any bit pattern is a valid value and the bytes need no validation to be
    /// read. What they mean still does, which is what the operation handlers do
    /// next.
    #[must_use]
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        debug_assert!(offset + core::mem::size_of::<T>() <= MAX_OP_DESCRIPTOR);
        // SAFETY: the caller's operation specification bounds `offset` plus the
        // size of `T` by the descriptor length, which is at most the buffer
        // size; `T` is a generated integer structure with no invalid bit
        // patterns; the read is explicitly unaligned.
        unsafe { core::ptr::read_unaligned(self.bytes.as_ptr().add(offset).cast::<T>()) }
    }

    /// Writes a structure into the buffer at `offset`.
    pub fn write<T: Copy>(&mut self, offset: usize, value: T) {
        debug_assert!(offset + core::mem::size_of::<T>() <= MAX_OP_DESCRIPTOR);
        // SAFETY: as `read`, and the buffer is exclusively borrowed here.
        unsafe {
            core::ptr::write_unaligned(self.bytes.as_mut_ptr().add(offset).cast::<T>(), value);
        }
    }
}

impl Default for Staging {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a user access was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    /// The range is not a canonical user range, or its length overflows.
    Range,
    /// A page of the range is not mapped in the domain.
    Unmapped,
    /// A page of the range is not reachable from ring 3.
    NotUser,
    /// A page of the range is not writable and the access needed to write.
    NotWritable,
}

impl Fault {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Fault::Range => "range",
            Fault::Unmapped => "unmapped",
            Fault::NotUser => "not_user",
            Fault::NotWritable => "not_writable",
        }
    }
}

/// Checks that `[addr, addr + len)` is a plausible user range before any page
/// is resolved, using arithmetic that cannot overflow.
fn bounded(addr: u64, len: u64) -> Result<(), Fault> {
    if len == 0 {
        return Err(Fault::Range);
    }
    let end = addr.checked_add(len).ok_or(Fault::Range)?;
    if addr < USER_MIN_ADDR || end > USER_MAX_ADDR {
        return Err(Fault::Range);
    }
    Ok(())
}

/// Runs `f` over the range one page-bounded chunk at a time.
fn walk(
    space: &AddressSpace,
    addr: u64,
    len: u64,
    writable: bool,
    mut f: impl FnMut(u64, usize, usize),
) -> Result<(), Fault> {
    bounded(addr, len)?;
    let mut offset = 0u64;
    while offset < len {
        let virt = addr + offset;
        let page_offset = virt % PAGE_SIZE;
        let chunk = (PAGE_SIZE - page_offset).min(len - offset);
        let (phys, flags) = space.translate(virt).ok_or(Fault::Unmapped)?;
        if flags & FLAG_USER == 0 {
            return Err(Fault::NotUser);
        }
        if writable && flags & FLAG_WRITABLE == 0 {
            return Err(Fault::NotWritable);
        }
        f(phys, offset as usize, chunk as usize);
        offset += chunk;
    }
    Ok(())
}

/// Copies `len` bytes from the domain's memory into `destination`.
pub fn copy_in(
    space: &AddressSpace,
    addr: u64,
    len: u64,
    destination: &mut [u8],
) -> Result<(), Fault> {
    if (len as usize) > destination.len() {
        return Err(Fault::Range);
    }
    walk(space, addr, len, false, |phys, offset, chunk| {
        // SAFETY: `phys` came from this domain's page tables, so it names a
        // frame the direct map covers, and `chunk` stays inside that page. The
        // destination slice was bounds-checked above. The machine lock is held
        // and this is a uniprocessor kernel with interrupts masked, so nothing
        // can unmap the page between the translation and the copy.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (thalyx_boot_protocol::HHDM_BASE + phys) as *const u8,
                destination.as_mut_ptr().add(offset),
                chunk,
            );
        }
    })
}

/// Copies `source` into the domain's memory.
pub fn copy_out(space: &AddressSpace, addr: u64, source: &[u8]) -> Result<(), Fault> {
    let len = source.len() as u64;
    walk(space, addr, len, true, |phys, offset, chunk| {
        // SAFETY: as `copy_in`, with the additional check that the mapping is
        // writable, which `walk` performed before calling this.
        unsafe {
            core::ptr::copy_nonoverlapping(
                source.as_ptr().add(offset),
                (thalyx_boot_protocol::HHDM_BASE + phys) as *mut u8,
                chunk,
            );
        }
    })
}
