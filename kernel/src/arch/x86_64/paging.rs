//! Four-level x86_64 page tables.
//!
//! One address space is one PML4. The kernel half is not copied into each
//! address space: the three PML4 slots the kernel uses point at PDPTs that are
//! *shared* by every address space, so a kernel mapping installed after a
//! domain was created is still visible in that domain. Copying the slots
//! instead would make later kernel mappings silently missing in older spaces.
//!
//! Slot 256 is the direct map, slot 257 the kernel's dynamic region (kernel
//! stacks, their guard pages and device mappings) and slot 511 the kernel
//! image.

use thalyx_boot_protocol::{HHDM_BASE, KERNEL_DYNAMIC_BASE, KERNEL_IMAGE_BASE, PAGE_SIZE};

use crate::mm::frame::{AllocError, FrameAllocator};
use crate::mm::{Frame, Owner, Rights, RightsError};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const WRITE_THROUGH: u64 = 1 << 3;
const CACHE_DISABLE: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;

/// Leaf flag reported by [`AddressSpace::translate`]: the page is writable.
pub const FLAG_WRITABLE: u64 = WRITABLE;
/// Leaf flag reported by [`AddressSpace::translate`]: the page is reachable
/// from ring 3.
pub const FLAG_USER: u64 = USER;
/// Leaf flag reported by [`AddressSpace::translate`]: the page is not

const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const ENTRIES: usize = 512;

/// PML4 slot of the direct map.
pub const SLOT_HHDM: usize = 256;
/// PML4 slot of the kernel's dynamic region.
pub const SLOT_DYNAMIC: usize = 257;
/// PML4 slot of the kernel image.
pub const SLOT_IMAGE: usize = 511;

const KERNEL_SLOTS: [usize; 3] = [SLOT_HHDM, SLOT_DYNAMIC, SLOT_IMAGE];

const _: () = assert!(HHDM_BASE >> 39 & 0x1FF == SLOT_HHDM as u64);
const _: () = assert!(KERNEL_DYNAMIC_BASE >> 39 & 0x1FF == SLOT_DYNAMIC as u64);
const _: () = assert!(KERNEL_IMAGE_BASE >> 39 & 0x1FF == SLOT_IMAGE as u64);

/// Why a mapping request was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapError {
    /// The rights combination is not representable or violates W^X.
    Rights(RightsError),
    /// The virtual address is not canonical or not page aligned.
    BadAddress,
    /// Something is already mapped there.
    AlreadyMapped,
    /// A large page covers the address; K1 never splits one.
    LargePageInTheWay,
    /// No frame was available for a page table.
    OutOfMemory,
}

impl From<AllocError> for MapError {
    fn from(_: AllocError) -> Self {
        MapError::OutOfMemory
    }
}

impl MapError {
    /// Short name used in diagnostic records.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MapError::Rights(e) => e.name(),
            MapError::BadAddress => "bad_address",
            MapError::AlreadyMapped => "already_mapped",
            MapError::LargePageInTheWay => "large_page_in_the_way",
            MapError::OutOfMemory => "out_of_memory",
        }
    }
}

const fn canonical(addr: u64) -> bool {
    let top = addr >> 47;
    top == 0 || top == 0x1FFFF
}

const fn index(addr: u64, level: u32) -> usize {
    ((addr >> (12 + 9 * level)) & 0x1FF) as usize
}

fn table(frame: Frame) -> &'static mut [u64; ENTRIES] {
    // SAFETY: `frame` is a page-table frame this kernel allocated and mapped
    // through the direct map. Page tables are only reachable from an
    // `AddressSpace`, which owns them, and K1 is uniprocessor with interrupts
    // masked in kernel context, so no other reference can exist at the same
    // time.
    unsafe { &mut *(frame.hhdm_addr() as *mut [u64; ENTRIES]) }
}

fn leaf_flags(rights: Rights) -> u64 {
    let mut flags = PRESENT;
    if rights.write {
        flags |= WRITABLE;
    }
    if rights.user {
        flags |= USER;
    }
    if !rights.execute {
        flags |= NO_EXECUTE;
    }
    if rights.uncacheable {
        flags |= CACHE_DISABLE | WRITE_THROUGH;
    }
    flags
}

/// A page-table hierarchy rooted at one PML4.
pub struct AddressSpace {
    root: Frame,
}

impl AddressSpace {
    /// Allocates an empty address space.
    pub fn new(alloc: &mut FrameAllocator, owner: Owner) -> Result<Self, MapError> {
        Ok(Self {
            root: alloc.alloc(owner)?,
        })
    }

    /// Value to load into CR3 to activate this space.
    #[must_use]
    pub const fn cr3(&self) -> u64 {
        self.root.addr()
    }

    /// Allocates the kernel's three shared PDPTs in this space. Called once, on
    /// the kernel's own address space.
    pub fn create_kernel_slots(&mut self, alloc: &mut FrameAllocator) -> Result<(), MapError> {
        let root = table(self.root);
        for slot in KERNEL_SLOTS {
            if root[slot] & PRESENT == 0 {
                let frame = alloc.alloc(Owner::Kernel)?;
                root[slot] = frame.addr() | PRESENT | WRITABLE;
            }
        }
        Ok(())
    }

    /// Points this space's kernel slots at the kernel's shared PDPTs.
    ///
    /// Sharing rather than copying is what makes a kernel stack mapped after
    /// this domain existed reachable from inside it.
    pub fn attach_kernel(&mut self, kernel: &AddressSpace) {
        let source = table(kernel.root);
        let target = table(self.root);
        for slot in KERNEL_SLOTS {
            target[slot] = source[slot];
        }
    }

    /// Maps one 4 KiB frame.
    pub fn map(
        &mut self,
        vaddr: u64,
        frame: Frame,
        rights: Rights,
        alloc: &mut FrameAllocator,
        owner: Owner,
    ) -> Result<(), MapError> {
        rights.validate().map_err(MapError::Rights)?;
        if !canonical(vaddr) || vaddr % PAGE_SIZE != 0 {
            return Err(MapError::BadAddress);
        }
        let entry = self.entry_for(vaddr, rights.user, alloc, owner)?;
        if *entry & PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        *entry = frame.addr() | leaf_flags(rights);
        Ok(())
    }

    /// Maps a 2 MiB-aligned range with 2 MiB entries.
    ///
    /// Only used for the kernel's own direct map, where the kernel already owns
    /// every frame and the mapping never has to be split.
    pub fn map_large_range(
        &mut self,
        vaddr: u64,
        phys: u64,
        count: u64,
        rights: Rights,
        alloc: &mut FrameAllocator,
    ) -> Result<(), MapError> {
        rights.validate().map_err(MapError::Rights)?;
        const LARGE: u64 = 2 * 1024 * 1024;
        if vaddr % LARGE != 0 || phys % LARGE != 0 {
            return Err(MapError::BadAddress);
        }
        for i in 0..count {
            let va = vaddr + i * LARGE;
            let pa = phys + i * LARGE;
            let pdpt = self.child(self.root, index(va, 3), rights.user, alloc, Owner::Kernel)?;
            let pd = self.child(pdpt, index(va, 2), rights.user, alloc, Owner::Kernel)?;
            let entries = table(pd);
            let slot = index(va, 1);
            if entries[slot] & PRESENT != 0 {
                return Err(MapError::AlreadyMapped);
            }
            entries[slot] = pa | leaf_flags(rights) | HUGE;
        }
        Ok(())
    }

    fn child(
        &self,
        parent: Frame,
        slot: usize,
        user: bool,
        alloc: &mut FrameAllocator,
        owner: Owner,
    ) -> Result<Frame, MapError> {
        let entries = table(parent);
        if entries[slot] & PRESENT != 0 {
            if entries[slot] & HUGE != 0 {
                return Err(MapError::LargePageInTheWay);
            }
            // An interior entry has to permit at least what any leaf below it
            // permits, so it is widened here and never narrowed. Access is
            // still decided by the leaf.
            if user {
                entries[slot] |= USER;
            }
            entries[slot] |= WRITABLE;
            return Ok(Frame::containing(entries[slot] & ADDRESS_MASK));
        }
        let frame = alloc.alloc(owner)?;
        let mut flags = PRESENT | WRITABLE;
        if user {
            flags |= USER;
        }
        entries[slot] = frame.addr() | flags;
        Ok(frame)
    }

    fn entry_for(
        &mut self,
        vaddr: u64,
        user: bool,
        alloc: &mut FrameAllocator,
        owner: Owner,
    ) -> Result<&'static mut u64, MapError> {
        let pdpt = self.child(self.root, index(vaddr, 3), user, alloc, owner)?;
        let pd = self.child(pdpt, index(vaddr, 2), user, alloc, owner)?;
        let pt = self.child(pd, index(vaddr, 1), user, alloc, owner)?;
        Ok(&mut table(pt)[index(vaddr, 0)])
    }

    /// Resolves a virtual address to `(physical address, leaf flags)`.
    #[must_use]
    pub fn translate(&self, vaddr: u64) -> Option<(u64, u64)> {
        if !canonical(vaddr) {
            return None;
        }
        let mut frame = self.root;
        let mut level = 3;
        loop {
            let entry = table(frame)[index(vaddr, level)];
            if entry & PRESENT == 0 {
                return None;
            }
            if level == 0 {
                return Some(((entry & ADDRESS_MASK) | (vaddr & 0xFFF), entry));
            }
            if entry & HUGE != 0 {
                let size = 1u64 << (12 + 9 * level);
                return Some(((entry & ADDRESS_MASK) | (vaddr & (size - 1)), entry));
            }
            frame = Frame::containing(entry & ADDRESS_MASK);
            level -= 1;
        }
    }

    /// Removes one 4 KiB mapping and returns the frame it named.
    ///
    /// Page tables are not freed: the caller may map the range again, and in K1
    /// the only unmapped ranges are kernel stack slots that are reused. The
    /// caller is responsible for invalidating the translation.
    pub fn unmap(&mut self, vaddr: u64) -> Option<Frame> {
        if !canonical(vaddr) || vaddr % PAGE_SIZE != 0 {
            return None;
        }
        let mut frame = self.root;
        let mut level = 3;
        while level > 0 {
            let entry = table(frame)[index(vaddr, level)];
            if entry & PRESENT == 0 || entry & HUGE != 0 {
                return None;
            }
            frame = Frame::containing(entry & ADDRESS_MASK);
            level -= 1;
        }
        let slot = &mut table(frame)[index(vaddr, 0)];
        if *slot & PRESENT == 0 {
            return None;
        }
        let mapped = Frame::containing(*slot & ADDRESS_MASK);
        *slot = 0;
        Some(mapped)
    }

    /// Frames returned by [`AddressSpace::destroy_user_half`].
    ///
    /// Reported separately because a domain is charged for both, and a teardown
    /// that returned only the data frames would look correct against a total.
    #[must_use]
    pub fn destroy_user_half(&mut self, alloc: &mut FrameAllocator, owner: Owner) -> Reclaimed {
        let mut counts = Reclaimed::default();
        let root = table(self.root);
        for slot in 0..SLOT_HHDM {
            let entry = root[slot];
            if entry & PRESENT == 0 {
                continue;
            }
            let pdpt = Frame::containing(entry & ADDRESS_MASK);
            self.destroy_level(pdpt, 3, alloc, owner, &mut counts);
            root[slot] = 0;
        }
        counts
    }

    fn destroy_level(
        &self,
        frame: Frame,
        level: u32,
        alloc: &mut FrameAllocator,
        owner: Owner,
        counts: &mut Reclaimed,
    ) {
        let entries = table(frame);
        for slot in 0..ENTRIES {
            let entry = entries[slot];
            if entry & PRESENT == 0 {
                continue;
            }
            let child = Frame::containing(entry & ADDRESS_MASK);
            if level == 1 || entry & HUGE != 0 {
                // Leaf: a data page charged to the domain. A large page here
                // would be a bug rather than a page to split, since no user
                // space is ever given one; freeing the frame it names is still
                // the right accounting.
                //
                // The frame goes to quarantine, not back to the pool. The
                // domain is dead and its space is inactive, but another
                // processor may still hold a translation from before it died,
                // and a translation is keyed by linear address rather than by
                // address space.
                alloc.retire(child, owner);
                counts.data_frames += 1;
            } else {
                self.destroy_level(child, level - 1, alloc, owner, counts);
            }
            entries[slot] = 0;
        }
        // Every entry of this table has just been cleared and the table itself
        // is unreachable from the inactive hierarchy. It still enters
        // quarantine: a paging-structure cache on another processor can hold an
        // interior entry as readily as a leaf.
        alloc.retire(frame, owner);
        counts.table_frames += 1;
    }

    /// Releases the PML4 itself. The kernel slots are shared and are cleared,
    /// not freed.
    ///
    /// # Safety
    ///
    /// The space must not be active on any CPU and its user half must already
    /// have been destroyed.
    pub unsafe fn release_root(self, alloc: &mut FrameAllocator, owner: Owner) {
        let root = table(self.root);
        for slot in KERNEL_SLOTS {
            root[slot] = 0;
        }
        // The caller guarantees the space is inactive and empty; quarantine
        // covers the paging-structure caches another processor may still hold.
        alloc.retire(self.root, owner);
    }
}

/// Frames returned by a user-half teardown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Frames that held domain data.
    pub data_frames: usize,
    /// Frames that held the domain's page tables.
    pub table_frames: usize,
}
