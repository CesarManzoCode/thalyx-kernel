//! Physical frame allocator.
//!
//! A bitmap over every frame below the direct-map limit. Bitmap allocation is
//! chosen over a free list threaded through the free frames themselves because
//! it keeps the frame contents untouched while free, which is what makes
//! "frames are handed out zeroed" a statement about the allocator rather than
//! about whoever released the frame last.
//!
//! Every allocation names an owner and is counted against it. What that gives
//! is the conservation check that a domain teardown returns exactly the frames
//! the domain was charged.
//!
//! A frame is not returned to the pool the moment its last mapping is removed.
//! Another processor may still hold the translation, and handing the frame to a
//! new owner before every processor has invalidated is exactly the dangerous
//! reuse the memory contract forbids. So a released frame goes to a quarantine
//! stamped with the invalidation generation at which it was retired, and comes
//! back only once every online processor has flushed at that generation or
//! later. A quarantine that fills up retains its frames rather than releasing
//! them early: a visible, counted leak is the conservative failure, and an
//! unsafe reuse is not.

use thalyx_boot_protocol::{MemoryRegion, PAGE_SIZE, region_kind};

use super::{Frame, Owner};

/// Whether any frame is in quarantine, readable without the control lock so
/// a tick can skip the drain when there is nothing to drain.
static QUARANTINE_PENDING: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Whether the quarantine holds anything.
#[must_use]
pub fn quarantine_pending() -> bool {
    QUARANTINE_PENDING.load(core::sync::atomic::Ordering::Acquire) != 0
}

/// Owners the allocator can count separately: the kernel, every domain slot and
/// every scope slot.
const OWNER_SLOTS: usize = 1 + crate::limits::MAX_DOMAINS + crate::limits::MAX_SCOPES;

/// Why an allocation failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AllocError {
    /// No free frame is left.
    OutOfMemory,
}

/// Frames the quarantine can hold at once.
const QUARANTINE_SLOTS: usize = 1024;

/// A frame waiting for every processor to have invalidated its translations.
#[derive(Clone, Copy)]
struct Quarantined {
    used: bool,
    frame: Frame,
    owner: Owner,
    generation: u64,
}

impl Quarantined {
    const fn empty() -> Self {
        Self {
            used: false,
            frame: Frame::containing(0),
            owner: Owner::Kernel,
            generation: 0,
        }
    }
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
    quarantine: [Quarantined; QUARANTINE_SLOTS],
    quarantined: usize,
    quarantine_peak: usize,
    quarantine_retained: usize,
    quarantine_released: usize,
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
            quarantine: [Quarantined::empty(); QUARANTINE_SLOTS],
            quarantined: 0,
            quarantine_peak: 0,
            quarantine_retained: 0,
            quarantine_released: 0,
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

    /// Frames waiting for an invalidation before they can be handed out again.
    #[must_use]
    pub const fn quarantined(&self) -> usize {
        self.quarantined
    }

    /// Frames of `owner` currently in quarantine.
    ///
    /// Reported next to the owner's remaining charge so the two can be compared:
    /// a teardown that left frames charged to a dead domain is either deferred
    /// reclamation, in which case these two numbers are equal, or a leak, in
    /// which case they are not.
    #[must_use]
    pub fn quarantined_for(&self, owner: Owner) -> usize {
        self.quarantine
            .iter()
            .filter(|slot| slot.used && slot.owner == owner)
            .count()
    }

    /// Moves the quarantined frames of `from` onto `to`'s account.
    ///
    /// A frame in quarantine still belongs to an owner's tally, and an owner
    /// whose table slot is about to be reused must not have frames credited
    /// to whoever takes the slot next when they are finally released. The
    /// frames themselves are untouched: they wait out the same invalidation.
    /// Returns how many moved.
    pub fn reassign_quarantine(&mut self, from: Owner, to: Owner) -> usize {
        let mut moved = 0;
        for slot in &mut self.quarantine {
            if slot.used && slot.owner == from {
                slot.owner = to;
                moved += 1;
            }
        }
        let (source, target) = (from.slot(), to.slot());
        self.charged[source] = self.charged[source].saturating_sub(moved);
        self.charged[target] = self.charged[target].saturating_add(moved);
        moved
    }

    /// Most frames the quarantine has held at once.
    #[must_use]
    pub const fn quarantine_peak(&self) -> usize {
        self.quarantine_peak
    }

    /// Frames the quarantine could not accept and is therefore holding for the
    /// rest of the run. They stay charged to their owner: a leak that is
    /// counted is a cost, an early release would be a correctness failure.
    #[must_use]
    pub const fn quarantine_retained(&self) -> usize {
        self.quarantine_retained
    }

    /// Frames the quarantine has released back into the pool.
    #[must_use]
    pub const fn quarantine_released(&self) -> usize {
        self.quarantine_released
    }

    /// Hands a frame to the quarantine instead of to the pool.
    ///
    /// The stamp is a fresh invalidation generation, so the frame is released
    /// only after every online processor has flushed at a point later than this
    /// call — whether or not the caller published an invalidation of its own.
    pub fn retire(&mut self, frame: Frame, owner: Owner) {
        let generation = crate::tlb::retire_stamp();
        QUARANTINE_PENDING.store(1, core::sync::atomic::Ordering::Release);
        for slot in &mut self.quarantine {
            if slot.used {
                continue;
            }
            *slot = Quarantined {
                used: true,
                frame,
                owner,
                generation,
            };
            self.quarantined += 1;
            if self.quarantined > self.quarantine_peak {
                self.quarantine_peak = self.quarantined;
            }
            return;
        }
        self.quarantine_retained += 1;
    }

    /// Releases every quarantined frame every processor has invalidated past.
    ///
    /// Returns how many frames came back. The condition is read from the
    /// invalidation module rather than supplied by the caller, so there is no
    /// argument to get wrong.
    pub fn drain_quarantine(&mut self) -> usize {
        let safe = crate::tlb::safe_generation();
        let mut released = 0usize;
        for index in 0..QUARANTINE_SLOTS {
            let slot = self.quarantine[index];
            if !slot.used || slot.generation > safe {
                continue;
            }
            self.quarantine[index].used = false;
            self.quarantined -= 1;
            released += 1;
            // SAFETY: every online processor has flushed at a generation later
            // than the one this frame was retired at, so no cached translation
            // can reach it, and the caller of `retire` had already removed
            // every page-table entry naming it.
            unsafe { self.release(slot.frame, slot.owner) };
        }
        self.quarantine_released += released;
        if self.quarantined == 0 {
            QUARANTINE_PENDING.store(0, core::sync::atomic::Ordering::Release);
        }
        released
    }

    /// Allocates one zeroed frame below `limit` and charges it to `owner`.
    ///
    /// The application-processor trampoline needs this: a start-up interrupt
    /// names its entry page in one byte, so the page has to be below one
    /// mebibyte, and an allocator that can only say "some frame" cannot answer.
    pub fn alloc_below(&mut self, limit: u64, owner: Owner) -> Result<Frame, AllocError> {
        let bound = ((limit / PAGE_SIZE) as usize).min(self.frames);
        // Frame zero is skipped: a physical address of zero is the value this
        // kernel uses to mean "none" in several places, and handing it out as a
        // trampoline page would make the two indistinguishable.
        for index in 1..bound {
            if self.test(index) {
                continue;
            }
            self.set(index);
            self.free -= 1;
            self.charged[owner.slot()] += 1;
            let frame = Frame::containing((index as u64) * PAGE_SIZE);
            // SAFETY: the frame was free, so no other owner holds a reference
            // to it, and the direct map covers it because the bitmap only ever
            // tracked frames below the map limit.
            unsafe { core::ptr::write_bytes(frame.hhdm_ptr(), 0, PAGE_SIZE as usize) };
            return Ok(frame);
        }
        Err(AllocError::OutOfMemory)
    }

    /// Allocates one zeroed frame and charges it to `owner`.
    ///
    /// Zeroing happens here, on the way out, so a frame released by one owner
    /// cannot reach another owner carrying residual bytes even if the releasing
    /// path was interrupted.
    pub fn alloc(&mut self, owner: Owner) -> Result<Frame, AllocError> {
        if self.free == 0 {
            self.drain_quarantine();
        }
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
        self.drain_quarantine();
        let count = count as usize;
        // From where the last allocation left off, and only then from the
        // beginning. The low frames are the kernel's own and are taken for the
        // life of the run, so a search that always starts at zero walks that
        // prefix bit by bit on every call -- thousands of tests to find a
        // single page, paid by every object created and by every step a
        // program's heap grows.
        let start = self.hint.min(self.frames);
        if let Some(frame) = self.take_run(start, self.frames, count, owner) {
            return Ok(frame);
        }
        if let Some(frame) = self.take_run(
            0,
            start.saturating_add(count).min(self.frames),
            count,
            owner,
        ) {
            return Ok(frame);
        }
        Err(AllocError::OutOfMemory)
    }

    /// Takes `count` contiguous free frames inside `[from, to)`, or nothing.
    fn take_run(&mut self, from: usize, to: usize, count: usize, owner: Owner) -> Option<Frame> {
        let mut index = from;
        while index + count <= to {
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
                self.hint = if index + count >= self.frames {
                    0
                } else {
                    index + count
                };
                return Some(Frame::containing((index as u64) * PAGE_SIZE));
            }
            // `index + run` is the first frame that is taken, so the next run
            // cannot start before the frame after it.
            index += run + 1;
        }
        None
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
        // The search resumes where frames are known to be free. Without this
        // the hint only ever moves forward, so a program that creates and
        // destroys an object in a loop walks further into the pool on every
        // turn and never comes back to the run it just gave up -- which is
        // both a longer search and a colder set of pages than the one it had.
        if index < self.hint {
            self.hint = index;
        }
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
