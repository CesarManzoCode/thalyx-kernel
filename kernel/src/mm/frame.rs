//! Physical frame allocator.
//!
//! A bitmap over every frame below the direct-map limit. Bitmap allocation is
//! chosen over a free list threaded through the free frames themselves because
//! it keeps the frame contents untouched while free, which is what makes
//! "frames are handed out zeroed" a statement about the allocator rather than
//! about whoever released the frame last.
//!
//! Every allocation names an owner and is counted against it. Nothing here
//! implements the sponsorship transfer, retention accounting or reservation
//! ordering the resource contract requires; those need scopes, which K1 does
//! not have. What it does provide is the conservation check that a domain
//! teardown returns exactly the frames the domain was charged.

use thalyx_boot_protocol::{MemoryRegion, PAGE_SIZE, region_kind};

use super::{Frame, Owner};

/// Owners the allocator can count separately: the kernel, every domain slot and
/// every scope slot.
const OWNER_SLOTS: usize = 1 + crate::limits::MAX_DOMAINS + crate::limits::MAX_SCOPES;

/// Why an allocation failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocError {
    /// No free frame is left.
    OutOfMemory,
}

/// Bitmap frame allocator over `[0, limit)`.
pub struct FrameAllocator {
    /// One bit per frame; set means allocated or permanently unavailable.
    bitmap: &'static mut [u64],
    frames: usize,
    free: usize,
    usable: usize,
    hint: usize,
    charged: [usize; OWNER_SLOTS],
}

impl FrameAllocator {
    /// Builds the allocator from the loader's memory map.
    ///
    /// The bitmap starts fully set, so any frame the map does not explicitly
    /// describe as usable stays unavailable. Usable regions are then cleared,
    /// and finally the bitmap's own frames are taken back.
    ///
    /// # Safety
    ///
    /// `regions` must be the loader's classification of physical memory and the
    /// direct map must already cover every usable frame it names. The caller
    /// must not have handed any of that memory to anything else.
    pub unsafe fn new(regions: &[MemoryRegion], limit_frames: usize) -> Option<Self> {
        let bitmap_words = limit_frames.div_ceil(64);
        let bitmap_bytes = bitmap_words * 8;
        let bitmap_frames = bitmap_bytes.div_ceil(PAGE_SIZE as usize);

        // Place the bitmap in the first usable region large enough to hold it.
        let mut storage: Option<u64> = None;
        for region in regions {
            if region.kind != region_kind::USABLE {
                continue;
            }
            if (region.pages as usize) >= bitmap_frames {
                storage = Some(region.base);
                break;
            }
        }
        let storage = storage?;

        // SAFETY: `storage` is the base of a usable region of at least
        // `bitmap_frames` pages, the direct map covers it, and no other owner
        // exists yet because this is the allocator's construction.
        let bitmap: &'static mut [u64] = unsafe {
            core::slice::from_raw_parts_mut(
                (super::Frame::containing(storage)).hhdm_addr() as *mut u64,
                bitmap_words,
            )
        };
        bitmap.fill(u64::MAX);

        let mut allocator = Self {
            bitmap,
            frames: limit_frames,
            free: 0,
            usable: 0,
            hint: 0,
            charged: [0; OWNER_SLOTS],
        };

        for region in regions {
            if region.kind != region_kind::USABLE {
                continue;
            }
            let first = (region.base / PAGE_SIZE) as usize;
            let count = region.pages as usize;
            for index in first..first.saturating_add(count).min(limit_frames) {
                if allocator.test(index) {
                    allocator.clear(index);
                    allocator.free += 1;
                    allocator.usable += 1;
                }
            }
        }

        let bitmap_first = (storage / PAGE_SIZE) as usize;
        for index in bitmap_first..bitmap_first + bitmap_frames {
            if !allocator.test(index) {
                allocator.set(index);
                allocator.free -= 1;
            }
        }
        allocator.charged[Owner::Kernel.slot()] += bitmap_frames;

        Some(allocator)
    }

    fn test(&self, index: usize) -> bool {
        self.bitmap[index / 64] & (1u64 << (index % 64)) != 0
    }

    fn set(&mut self, index: usize) {
        self.bitmap[index / 64] |= 1u64 << (index % 64);
    }

    fn clear(&mut self, index: usize) {
        self.bitmap[index / 64] &= !(1u64 << (index % 64));
    }

    /// Total frames the allocator tracks.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Frames that were usable when the allocator was built.
    #[must_use]
    pub const fn usable(&self) -> usize {
        self.usable
    }

    /// Frames currently free.
    #[must_use]
    pub const fn free_frames(&self) -> usize {
        self.free
    }

    /// Frames currently charged to `owner`.
    #[must_use]
    pub fn charged(&self, owner: Owner) -> usize {
        self.charged[owner.slot()]
    }

    /// Allocates one zeroed frame and charges it to `owner`.
    ///
    /// Zeroing happens here, on the way out, so a frame released by one owner
    /// cannot reach another owner carrying residual bytes even if the releasing
    /// path was interrupted.
    pub fn alloc(&mut self, owner: Owner) -> Result<Frame, AllocError> {
        let start = self.hint;
        let mut index = start;
        loop {
            if !self.test(index) {
                self.set(index);
                self.free -= 1;
                self.charged[owner.slot()] += 1;
                self.hint = if index + 1 >= self.frames {
                    0
                } else {
                    index + 1
                };
                let frame = Frame::containing((index as u64) * PAGE_SIZE);
                // SAFETY: the frame was free, so no other owner holds a
                // reference to it, and the direct map covers it because the
                // bitmap only ever tracked frames below the map limit.
                unsafe { core::ptr::write_bytes(frame.hhdm_ptr(), 0, PAGE_SIZE as usize) };
                return Ok(frame);
            }
            index += 1;
            if index >= self.frames {
                index = 0;
            }
            if index == start {
                return Err(AllocError::OutOfMemory);
            }
        }
    }

    /// Allocates `count` physically contiguous zeroed frames.
    ///
    /// Memory objects are contiguous in V0. That is an implementation choice,
    /// not a property of the contract: it lets the kernel read an object's
    /// bytes as one slice through the direct map, which is what the ELF reader
    /// and the mediated copies need. Fragmentation therefore shows up as a
    /// refused reservation rather than as a partial object, which is the
    /// behaviour the resource contract already requires of an exhausted pool.
    pub fn alloc_contiguous(&mut self, count: u64, owner: Owner) -> Result<Frame, AllocError> {
        if count == 0 {
            return Err(AllocError::OutOfMemory);
        }
        let count = count as usize;
        let mut index = 0usize;
        while index + count <= self.frames {
            let mut run = 0usize;
            while run < count && !self.test(index + run) {
                run += 1;
            }
            if run == count {
                for offset in 0..count {
                    let frame = index + offset;
                    self.set(frame);
                    self.free -= 1;
                    self.charged[owner.slot()] += 1;
                    let frame = Frame::containing((frame as u64) * PAGE_SIZE);
                    // SAFETY: the frame was free, so no other owner holds a
                    // reference to it, and the direct map covers it because the
                    // bitmap only ever tracked frames below the map limit.
                    unsafe { core::ptr::write_bytes(frame.hhdm_ptr(), 0, PAGE_SIZE as usize) };
                }
                return Ok(Frame::containing((index as u64) * PAGE_SIZE));
            }
            // `index + run` is the first frame that is taken, so the next run
            // cannot start before the frame after it.
            index += run + 1;
        }
        Err(AllocError::OutOfMemory)
    }

    /// Returns a frame charged to `owner`.
    ///
    /// # Safety
    ///
    /// No mapping, device or other reference to `frame` may remain. In K1 that
    /// is established by construction: frames are freed only from a domain
    /// teardown that has already removed the domain's page tables, on a core
    /// that is not running in that address space, with no DMA in the system.
    pub unsafe fn release(&mut self, frame: Frame, owner: Owner) {
        let index = frame.index();
        if index >= self.frames || !self.test(index) {
            return;
        }
        self.clear(index);
        self.free += 1;
        let slot = owner.slot();
        self.charged[slot] = self.charged[slot].saturating_sub(1);
    }

    /// Adds a physical range to the free pool, charging nothing.
    ///
    /// Used to reclaim loader structures and boot modules once their contents
    /// have been copied into memory the kernel owns.
    ///
    /// # Safety
    ///
    /// The range must be RAM that nothing references any more.
    pub unsafe fn reclaim(&mut self, base: u64, pages: u64) -> usize {
        let first = (base / PAGE_SIZE) as usize;
        let mut recovered = 0;
        for index in first..first.saturating_add(pages as usize).min(self.frames) {
            if self.test(index) {
                self.clear(index);
                self.free += 1;
                recovered += 1;
            }
        }
        recovered
    }
}
