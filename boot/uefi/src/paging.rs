//! Bootstrap page tables.
//!
//! The loader builds a complete four-level hierarchy rather than editing the
//! firmware's. Editing would mean reasoning about whatever mapping granularity
//! and attributes this particular firmware chose; building means the state the
//! kernel is entered with is entirely the loader's, and the kernel replaces it
//! with its own within a few hundred instructions.
//!
//! Three regions exist in it, and no more:
//!
//! * an identity map, because the instructions between loading CR3 and jumping
//!   to the kernel execute at their physical addresses;
//! * the direct map at the address the hand-off contract fixes, because the
//!   kernel's stack pointer and its view of physical memory both live there;
//! * the kernel image at its link addresses, one mapping per segment with the
//!   rights that segment's program header declares.
//!
//! Nothing is mapped user-accessible. The identity map is executable for the
//! duration of the transition and is gone as soon as the kernel installs its
//! own tables.

/// Present.
pub const PRESENT: u64 = 1 << 0;
/// Writable.
pub const WRITABLE: u64 = 1 << 1;
/// Large page at PDPT or PD level.
pub const HUGE: u64 = 1 << 7;
/// Not executable. Requires `IA32_EFER.NXE`.
pub const NO_EXECUTE: u64 = 1 << 63;

const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
/// Bytes per 4 KiB page.
pub const PAGE_SIZE: u64 = 4096;
/// Bytes per 2 MiB page.
pub const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

/// Bump allocator over a block of pages reserved for page tables.
pub struct Pool {
    base: u64,
    pages: usize,
    used: usize,
}

impl Pool {
    /// Wraps a reserved, page-aligned block.
    ///
    /// # Safety
    ///
    /// `base` must be the physical address of `pages` reserved, identity-mapped
    /// pages that nothing else uses.
    pub const unsafe fn new(base: u64, pages: usize) -> Self {
        Self {
            base,
            pages,
            used: 0,
        }
    }

    /// Pages reserved for the block.
    #[must_use]
    pub const fn pages(&self) -> usize {
        self.pages
    }

    /// Pages handed out so far.
    #[must_use]
    pub const fn used(&self) -> usize {
        self.used
    }

    fn take(&mut self) -> Option<u64> {
        if self.used == self.pages {
            return None;
        }
        let address = self.base + (self.used as u64) * PAGE_SIZE;
        self.used += 1;
        // SAFETY: the block is reserved, identity mapped and this page has not
        // been handed out before.
        unsafe { core::ptr::write_bytes(address as *mut u8, 0, PAGE_SIZE as usize) };
        Some(address)
    }
}

/// Why a mapping could not be installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The table pool ran out of pages.
    OutOfTables,
    /// A mapping already covers the address.
    AlreadyMapped,
    /// A large page covers the address.
    LargePageInTheWay,
    /// The address is not aligned for the requested page size.
    Misaligned,
}

/// A page-table hierarchy under construction.
pub struct Tables {
    root: u64,
    pool: Pool,
}

fn slot(address: u64, level: u32) -> usize {
    ((address >> (12 + 9 * level)) & 0x1FF) as usize
}

fn entries(table: u64) -> &'static mut [u64; 512] {
    // SAFETY: every table address comes from the pool, which hands out
    // reserved, identity-mapped, zeroed pages.
    unsafe { &mut *(table as *mut [u64; 512]) }
}

impl Tables {
    /// Allocates the PML4 from `pool`.
    pub fn new(mut pool: Pool) -> Result<Self, Error> {
        let root = pool.take().ok_or(Error::OutOfTables)?;
        Ok(Self { root, pool })
    }

    /// Physical address of the PML4, for CR3.
    #[must_use]
    pub const fn root(&self) -> u64 {
        self.root
    }

    /// The table pool, for reporting how much of it was used.
    #[must_use]
    pub const fn pool(&self) -> &Pool {
        &self.pool
    }

    fn child(&mut self, table: u64, index: usize) -> Result<u64, Error> {
        let table = entries(table);
        if table[index] & PRESENT != 0 {
            if table[index] & HUGE != 0 {
                return Err(Error::LargePageInTheWay);
            }
            return Ok(table[index] & ADDRESS_MASK);
        }
        let child = self.pool.take().ok_or(Error::OutOfTables)?;
        table[index] = child | PRESENT | WRITABLE;
        Ok(child)
    }

    /// Maps one 2 MiB page.
    pub fn map_large(&mut self, vaddr: u64, phys: u64, flags: u64) -> Result<(), Error> {
        if vaddr % LARGE_PAGE_SIZE != 0 || phys % LARGE_PAGE_SIZE != 0 {
            return Err(Error::Misaligned);
        }
        let root = self.root;
        let pdpt = self.child(root, slot(vaddr, 3))?;
        let pd = self.child(pdpt, slot(vaddr, 2))?;
        let table = entries(pd);
        let index = slot(vaddr, 1);
        if table[index] & PRESENT != 0 {
            return Err(Error::AlreadyMapped);
        }
        table[index] = phys | flags | HUGE | PRESENT;
        Ok(())
    }

    /// Maps one 4 KiB page.
    pub fn map(&mut self, vaddr: u64, phys: u64, flags: u64) -> Result<(), Error> {
        if vaddr % PAGE_SIZE != 0 || phys % PAGE_SIZE != 0 {
            return Err(Error::Misaligned);
        }
        let root = self.root;
        let pdpt = self.child(root, slot(vaddr, 3))?;
        let pd = self.child(pdpt, slot(vaddr, 2))?;
        let pt = self.child(pd, slot(vaddr, 1))?;
        let table = entries(pt);
        let index = slot(vaddr, 0);
        if table[index] & PRESENT != 0 {
            return Err(Error::AlreadyMapped);
        }
        table[index] = phys | flags | PRESENT;
        Ok(())
    }

    /// Maps `count` 2 MiB pages starting at `vaddr`/`phys`.
    pub fn map_large_range(
        &mut self,
        vaddr: u64,
        phys: u64,
        count: u64,
        flags: u64,
    ) -> Result<(), Error> {
        for index in 0..count {
            self.map_large(
                vaddr + index * LARGE_PAGE_SIZE,
                phys + index * LARGE_PAGE_SIZE,
                flags,
            )?;
        }
        Ok(())
    }

    /// Maps `count` 4 KiB pages starting at `vaddr`/`phys`.
    pub fn map_range(
        &mut self,
        vaddr: u64,
        phys: u64,
        count: u64,
        flags: u64,
    ) -> Result<(), Error> {
        for index in 0..count {
            self.map(vaddr + index * PAGE_SIZE, phys + index * PAGE_SIZE, flags)?;
        }
        Ok(())
    }
}
