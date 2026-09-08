//! Physical memory, address spaces and mapping rights.
//!
//! The types here are architecture independent; the page tables that implement
//! them are in `arch::x86_64::paging`. The split is the one the platform
//! contract requires: nothing above this module knows what a PML4 is, and
//! nothing below it knows what a domain is.

pub mod frame;

use thalyx_boot_protocol::{HHDM_BASE, PAGE_SIZE};

/// A 4 KiB physical frame, identified by its base address.
///
/// V0 has one page size for allocation and mapping. The direct map uses 2 MiB
/// entries, but that is a property of the kernel's own mapping of memory it
/// already owns, not an allocation unit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Frame(u64);

impl Frame {
    /// The frame containing `addr`.
    #[must_use]
    pub const fn containing(addr: u64) -> Self {
        Self(addr & !(PAGE_SIZE - 1))
    }

    /// Physical base address of the frame.
    #[must_use]
    pub const fn addr(self) -> u64 {
        self.0
    }

    /// Index of the frame in a zero-based frame numbering.
    #[must_use]
    pub const fn index(self) -> usize {
        (self.0 / PAGE_SIZE) as usize
    }

    /// Virtual address of this frame inside the direct map.
    ///
    /// Valid only while the frame lies below the direct map limit the loader
    /// established and the kernel re-established; callers that allocate frames
    /// from the frame allocator always satisfy that.
    #[must_use]
    pub const fn hhdm_addr(self) -> u64 {
        HHDM_BASE + self.0
    }

    /// Mutable pointer to the frame's contents through the direct map.
    #[must_use]
    pub const fn hhdm_ptr(self) -> *mut u8 {
        self.hhdm_addr() as *mut u8
    }
}

/// Access rights requested for a mapping.
///
/// The platform cannot express every combination: with ordinary x86_64 tables a
/// writable or executable user page is also readable. Mapping without `read` is
/// therefore rejected rather than silently upgraded, and `write` together with
/// `execute` is rejected outright by the W^X rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rights {
    /// Readable.
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// Executable.
    pub execute: bool,
    /// Reachable from CPL 3.
    pub user: bool,
    /// Uncacheable; used for device memory, never for RAM.
    pub uncacheable: bool,
}

impl Rights {
    /// Kernel read-write data.
    pub const KERNEL_RW: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: false,
        uncacheable: false,
    };
    /// Kernel read-only data.
    pub const KERNEL_RO: Self = Self {
        read: true,
        write: false,
        execute: false,
        user: false,
        uncacheable: false,
    };
    /// Kernel executable text.
    pub const KERNEL_RX: Self = Self {
        read: true,
        write: false,
        execute: true,
        user: false,
        uncacheable: false,
    };
    /// Uncacheable kernel device mapping.
    pub const KERNEL_DEVICE: Self = Self {
        read: true,
        write: true,
        execute: false,
        user: false,
        uncacheable: true,
    };

    /// Builds user rights from an ELF program header's permission bits.
    #[must_use]
    pub const fn user(read: bool, write: bool, execute: bool) -> Self {
        Self {
            read,
            write,
            execute,
            user: true,
            uncacheable: false,
        }
    }

    /// Rejects combinations the platform cannot represent or the memory
    /// contract forbids.
    #[must_use]
    pub const fn validate(self) -> Result<(), RightsError> {
        if !self.read && (self.write || self.execute) {
            return Err(RightsError::WriteOrExecuteWithoutRead);
        }
        if self.write && self.execute {
            return Err(RightsError::WriteAndExecute);
        }
        if !self.read {
            return Err(RightsError::NoAccess);
        }
        Ok(())
    }
}

/// Why a rights combination was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RightsError {
    /// x86_64 paging cannot deny reads to a writable or executable page.
    WriteOrExecuteWithoutRead,
    /// W^X: an executable mapping must not also be writable.
    WriteAndExecute,
    /// A mapping with no access at all is not a mapping.
    NoAccess,
}

impl RightsError {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            RightsError::WriteOrExecuteWithoutRead => "write_or_execute_without_read",
            RightsError::WriteAndExecute => "write_and_execute",
            RightsError::NoAccess => "no_access",
        }
    }
}

/// Who pays for a frame.
///
/// K1 has no scopes, so this is not the sponsorship model of the resource
/// contract: it is the minimum needed to prove that everything a domain was
/// charged is returned when the domain dies. The scope tree replaces it in K2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    /// Charged to the kernel itself: page tables of the kernel space, kernel
    /// stacks, the frame bitmap.
    Kernel,
    /// Charged to a domain, by identifier.
    Domain(u16),
}

impl Owner {
    const fn slot(self) -> usize {
        match self {
            Owner::Kernel => 0,
            Owner::Domain(id) => (id as usize) + 1,
        }
    }
}
