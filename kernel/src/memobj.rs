//! Memory objects, their mappings and the seal.
//!
//! A memory object is a run of resident physical pages with maximum rights and
//! a seal state. V0 allocates it contiguously, which is an implementation
//! choice and not a contract: it lets the kernel read the object's bytes as one
//! slice through the direct map, which is what the image reader and the mediated
//! copies need, and it turns fragmentation into a refused reservation rather
//! than a partial object.
//!
//! Every installed mapping is recorded. That reverse index is not bookkeeping
//! for its own sake: sealing has to *withdraw* writers, not merely refuse new
//! ones, and revoking the capability that authorised a mapping has to invalidate
//! the mapping too. Neither is possible from the object alone.
//!
//! `seal` follows the conservative route the memory contract describes:
//! `Mutable → Sealing → Sealed`, publishing the sealed state only after the
//! writable aliases are gone. K2 is uniprocessor with no DMA, so "gone" means
//! the mappings were removed and the local translations invalidated; the
//! cross-core acknowledgement and the device quiescence that a full seal needs
//! belong to K3 and are not claimed here.

use thalyx_abi::generated::memory_state;
use thalyx_boot_protocol::PAGE_SIZE;

use crate::mm::Frame;
use crate::obj::{GrantId, ScopeId};

/// Lifecycle of a memory object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Free table slot.
    Empty,
    /// Writable, subject to rights.
    Mutable,
    /// Writers refused; existing writable aliases being withdrawn.
    Sealing,
    /// No writer exists inside the declared perimeter.
    Sealed,
}

impl State {
    /// Interface value reported in a memory descriptor.
    #[must_use]
    pub const fn abi(self) -> u32 {
        match self {
            State::Empty => 0,
            State::Mutable => memory_state::MUTABLE,
            State::Sealing => memory_state::SEALING,
            State::Sealed => memory_state::SEALED,
        }
    }

    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            State::Empty => "empty",
            State::Mutable => "mutable",
            State::Sealing => "sealing",
            State::Sealed => "sealed",
        }
    }
}

/// A run of resident pages with rights and a seal state.
pub struct MemoryObject {
    /// Lifecycle state.
    pub state: State,
    /// Generation of this table slot.
    pub generation: u32,
    /// Diagnostic identity, unique within the boot epoch.
    pub id: u64,
    /// First physical frame.
    pub base: Frame,
    /// Length in 4 KiB pages.
    pub pages: u32,
    /// Rights no capability over this object may exceed.
    pub max_rights: u32,
    /// Scope charged for the pages.
    pub sponsor: ScopeId,
    /// Mappings currently installed.
    pub map_count: u32,
    /// Mappings currently installed that permit writing.
    pub writable_maps: u32,
    /// Diagnostic label. A name grants nothing.
    pub label: [u8; 16],
    /// Capability entries naming this object.
    pub refs: u32,
}

impl MemoryObject {
    /// A free slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            state: State::Empty,
            generation: 0,
            id: 0,
            base: Frame::containing(0),
            pages: 0,
            max_rights: 0,
            sponsor: 0,
            map_count: 0,
            writable_maps: 0,
            label: [0; 16],
            refs: 0,
        }
    }

    /// Label as text, for diagnostic records.
    #[must_use]
    pub fn label_str(&self) -> &str {
        let len = self.label.iter().position(|byte| *byte == 0).unwrap_or(16);
        core::str::from_utf8(&self.label[..len]).unwrap_or("?")
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.pages as u64 * PAGE_SIZE
    }

    /// The object's bytes, seen through the direct map.
    ///
    /// # Safety
    ///
    /// The caller must hold the machine lock, so no other context can be
    /// mapping, unmapping or freeing the object while the slice exists. The
    /// pages are contiguous and covered by the direct map by construction.
    #[must_use]
    pub unsafe fn bytes(&self) -> &'static [u8] {
        // SAFETY: the object owns `pages` contiguous frames starting at `base`,
        // every one of which the direct map covers, and the caller guarantees
        // exclusive access for the lifetime of the borrow.
        unsafe { core::slice::from_raw_parts(self.base.hhdm_ptr(), self.len() as usize) }
    }

    /// The object's bytes, mutably.
    ///
    /// # Safety
    ///
    /// As [`MemoryObject::bytes`], and the object must not be sealed: a sealed
    /// object promises that nothing inside the perimeter writes it, and the
    /// kernel is inside that perimeter.
    #[must_use]
    pub unsafe fn bytes_mut(&mut self) -> &'static mut [u8] {
        // SAFETY: as `bytes`, with the caller's additional guarantee that the
        // object is not sealed.
        unsafe { core::slice::from_raw_parts_mut(self.base.hhdm_ptr(), self.len() as usize) }
    }
}

/// One installed mapping.
///
/// The record names the object, the domain, the range and the grant whose
/// authority installed it. Revoking that grant therefore has something concrete
/// to withdraw, and sealing has something concrete to count.
#[derive(Clone, Copy, Debug)]
pub struct MapRecord {
    /// Whether the record is in use.
    pub used: bool,
    /// Memory object index.
    pub memory: u16,
    /// Generation the memory object had when the mapping was installed.
    pub memory_generation: u32,
    /// Domain index the mapping lives in.
    pub domain: u16,
    /// Generation the domain had when the mapping was installed.
    pub domain_generation: u32,
    /// First virtual address.
    pub vaddr: u64,
    /// First page of the object.
    pub offset_pages: u32,
    /// Length in pages.
    pub pages: u32,
    /// Rights the mapping carries.
    pub rights: u32,
    /// Grant whose authority installed the mapping. Revoking it has to
    /// withdraw the mapping too: invalidating the handle alone would leave a
    /// translation the hardware still honours.
    pub grant: GrantId,
    /// Scope charged for the mapping's metadata.
    pub scope: ScopeId,
}

impl MapRecord {
    /// A free record.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            used: false,
            memory: 0,
            memory_generation: 0,
            domain: 0,
            domain_generation: 0,
            vaddr: 0,
            offset_pages: 0,
            pages: 0,
            rights: 0,
            grant: crate::obj::NO_GRANT,
            scope: 0,
        }
    }
}
