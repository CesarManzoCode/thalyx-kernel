//! Cross-processor invalidation of translations, and the reclamation that
//! depends on it.
//!
//! Removing a page-table entry does not remove the translation another
//! processor has already cached. The mechanism here is the conservative one the
//! memory and concurrency contracts describe, and it is deliberately a
//! rendezvous rather than a per-range optimisation:
//!
//! * a **generation** counts published invalidations. Every removal or
//!   narrowing of a mapping bumps it, after the entry itself is written;
//! * every processor records the generation it has flushed to. Flushing is
//!   complete — a `CR3` reload, which retires every entry because this kernel
//!   marks no mapping global — so a later generation subsumes every earlier
//!   one. That is what makes coalescing safe: an acknowledgement of generation
//!   three cannot leave an obligation from generation two behind, which is the
//!   race a range-tracking scheme has to solve separately;
//! * the initiator releases every lock, tells the other processors, and waits
//!   until each of them has recorded a generation at least as new as the one it
//!   published. The acknowledgement names the generation, not merely that an
//!   interrupt arrived;
//! * a processor about to run a user thread refreshes first. That closes the
//!   race a snapshot of "processors currently in this address space" cannot: a
//!   processor entering afterwards was not in the snapshot, and here it does not
//!   need to be.
//!
//! Waiting never happens with a lock held, and the wait itself services
//! incoming requests, so two processors that shoot down at the same time make
//! progress instead of waiting for each other. A wait that runs out of patience
//! is a *failure to observe quiescence*, not permission to proceed: the frames
//! stay in quarantine and the run says so.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::{cpu, lapic, trap};
use crate::limits::MAX_CPUS;

/// Published invalidations. Starts at one so a processor that has recorded
/// nothing is distinguishable from one that has recorded the first generation.
static GENERATION: AtomicU64 = AtomicU64::new(1);
/// Generation each processor has flushed to.
static SEEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(1) }; MAX_CPUS];
/// Bitmask of processors that have completed their handshake with the kernel.
static ONLINE: AtomicU64 = AtomicU64::new(0);

/// Invalidations published.
static PUBLISHED: AtomicU64 = AtomicU64::new(0);
/// Interrupts sent to announce them.
static IPIS: AtomicU64 = AtomicU64::new(0);
/// Complete flushes performed, on every processor.
static FLUSHES: AtomicU64 = AtomicU64::new(0);
/// Waits that ran out of patience before every processor acknowledged.
static TIMEOUTS: AtomicU64 = AtomicU64::new(0);
/// Longest wait observed, in spins.
static MAX_SPINS: AtomicU64 = AtomicU64::new(0);

/// Spins a wait allows before it gives up and reports a failure to observe
/// quiescence. Large enough that an ordinary kernel critical section on another
/// processor finishes inside it, small enough that a wedged processor is
/// reported instead of hanging the machine.
const WAIT_SPIN_LIMIT: u64 = 200_000_000;

/// Bit per processor each domain's address space has ever been dispatched
/// on.
///
/// A count of who is in the space *right now* answers a different and much
/// weaker question: an invalidation has to reach every processor that could
/// hold a translation, and a processor that ran here a microsecond ago still
/// could. The mask is what makes "reached everyone who could have cached it"
/// checkable after the fact. Set by the scheduler at dispatch, without a
/// lock; cleared when the domain's slot is reused.
static SPACE_MASKS: [AtomicU64; crate::limits::MAX_DOMAINS] =
    [const { AtomicU64::new(0) }; crate::limits::MAX_DOMAINS];

/// Bit per processor that may hold a translation of each domain's address
/// space **right now**.
///
/// The narrower question, and the one an invalidation actually has to answer.
/// A processor enters the set when it dispatches a thread of the domain and
/// leaves it when it loads a different space: this kernel marks no mapping
/// global and uses no address-space identifiers, so writing `CR3` retires
/// every entry of the space being left, and a processor that has left holds
/// nothing of it. The wider mask above stays what it is -- every processor the
/// space was ever on -- because that is what an *acknowledgement* has to be
/// checked against after the fact, and the two are different claims.
///
/// Linux keeps the same set, in `mm_cpumask`, and for the same reason: sending
/// an interrupt to a processor that cannot hold the translation is a
/// microsecond spent to make a processor flush nothing.
static SPACE_LIVE: [AtomicU64; crate::limits::MAX_DOMAINS] =
    [const { AtomicU64::new(0) }; crate::limits::MAX_DOMAINS];

/// Space each processor has loaded, or `usize::MAX`.
static CURRENT_SPACE: [core::sync::atomic::AtomicUsize; MAX_CPUS] =
    [const { core::sync::atomic::AtomicUsize::new(usize::MAX) }; MAX_CPUS];

/// Notes that `cpu` is dispatching a thread of `domain`.
///
/// The store into the live set is sequentially consistent, and so is the load
/// of the generation that [`refresh_local`] performs on the way to running
/// that thread. Together with the matching pair on the other side -- publish
/// the generation, then read the live set -- one of the two processors always
/// sees the other: either this processor is in the set the invalidation waits
/// for, or its own refresh has already retired everything the invalidation
/// removed. There is no third case, and that is the whole of why an
/// invalidation may skip a processor.
#[inline]
pub fn note_dispatch(domain: usize, cpu: usize) {
    let bit = 1u64 << cpu;
    if let Some(mask) = SPACE_MASKS.get(domain)
        && mask.load(Ordering::Relaxed) & bit == 0
    {
        mask.fetch_or(bit, Ordering::AcqRel);
    }
    // Only this processor ever sets or clears its own bit, so a relaxed read
    // of it is exact. A bit that is already set was set by a store this
    // processor made and has not undone, and that store is ordered before
    // everything since -- including this dispatch's refresh. The pairing holds
    // without writing it again.
    if let Some(live) = SPACE_LIVE.get(domain)
        && live.load(Ordering::Relaxed) & bit == 0
    {
        live.fetch_or(bit, Ordering::SeqCst);
    }
}

/// Processors `domain`'s address space has ever been dispatched on.
#[must_use]
pub fn space_mask(domain: usize) -> u64 {
    SPACE_MASKS
        .get(domain)
        .map_or(0, |mask| mask.load(Ordering::Acquire))
}

/// Processors that may hold a translation of `domain`'s space right now.
#[must_use]
pub fn live_mask(domain: usize) -> u64 {
    SPACE_LIVE
        .get(domain)
        .map_or(0, |mask| mask.load(Ordering::SeqCst))
}

/// Forgets the masks of a domain whose slot is being reused.
pub fn forget_space(domain: usize) {
    if let Some(mask) = SPACE_MASKS.get(domain) {
        mask.store(0, Ordering::Release);
    }
    if let Some(live) = SPACE_LIVE.get(domain) {
        live.store(0, Ordering::SeqCst);
    }
}

/// Loads `cr3` on this processor if it is not the space already loaded.
///
/// # Safety
///
/// `cr3` must be the root of an address space that shares the kernel's upper
/// half, so the code and stack executing here stay mapped across the write.
pub unsafe fn switch_space(cr3: u64, domain: usize) {
    let me = crate::percpu::index();
    if me >= MAX_CPUS {
        return;
    }
    let bit = 1u64 << me;
    if cr3 == 0 || cr3 == cpu::read_cr3() {
        // Nothing is loaded and nothing is retired, so nothing this processor
        // holds has changed and the sets stay as they are.
        return;
    }
    let leaving = CURRENT_SPACE[me].swap(domain, Ordering::Relaxed);
    // SAFETY: the caller's contract.
    unsafe { cpu::write_cr3(cr3) };
    // The write retired every entry of the space just left -- no mapping in
    // this kernel is global and there are no address-space identifiers -- so
    // this processor now holds nothing of it. Cleared afterwards, so the set
    // never says "gone" while a translation is still cached.
    if leaving != domain
        && let Some(live) = SPACE_LIVE.get(leaving)
        && live.load(Ordering::Relaxed) & bit != 0
    {
        live.fetch_and(!bit, Ordering::SeqCst);
    }
}

/// Records that `cpu` participates in invalidation from now on.
///
/// A processor is added only after it has flushed once, so it can never be
/// waited for at a generation it never saw.
pub fn mark_online(cpu: usize) {
    if cpu >= MAX_CPUS {
        return;
    }
    SEEN[cpu].store(GENERATION.load(Ordering::Acquire), Ordering::Release);
    ONLINE.fetch_or(1u64 << cpu, Ordering::AcqRel);
}

/// Removes `cpu` from the set an invalidation waits for.
///
/// A parked processor cannot acknowledge anything, and waiting for it would
/// turn the end of a run into a hang. It is removed only after it has stopped
/// scheduling, so it holds no translation anyone still cares about.
pub fn mark_offline(cpu: usize) {
    if cpu >= MAX_CPUS {
        return;
    }
    SEEN[cpu].store(u64::MAX, Ordering::Release);
    ONLINE.fetch_and(!(1u64 << cpu), Ordering::AcqRel);
}

/// Processors currently participating.
#[must_use]
pub fn online_mask() -> u64 {
    ONLINE.load(Ordering::Acquire)
}

/// Publishes an invalidation and returns the generation that names it.
///
/// The caller must already have written the page-table change. On this
/// architecture ordinary stores are not reordered with each other, so the entry
/// is visible to any processor that observes this counter.
pub fn publish() -> u64 {
    PUBLISHED.fetch_add(1, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

/// A fresh generation for a frame entering quarantine.
///
/// It bumps the counter without counting an invalidation: nothing new has been
/// removed from a page table here, but the frame must not come back until every
/// processor has flushed at a point after this call, and only a strictly newer
/// generation expresses that.
pub fn retire_stamp() -> u64 {
    GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

/// Brings the calling processor up to the published generation.
///
/// Safe to call from anywhere, including from inside a lock's spin loop: it
/// takes no lock, and a complete flush is correct at any point.
pub fn refresh_local() {
    let cpu = crate::percpu::index();
    if cpu >= MAX_CPUS {
        return;
    }
    let wanted = GENERATION.load(Ordering::Acquire);
    if SEEN[cpu].load(Ordering::Relaxed) >= wanted {
        return;
    }
    // SAFETY: the value written back is the address space this processor is
    // already executing in, so the code and stack running here stay mapped.
    unsafe { cpu::flush_tlb_all() };
    FLUSHES.fetch_add(1, Ordering::Relaxed);
    SEEN[cpu].store(wanted, Ordering::Release);
}

/// The interrupt one processor sends another to make it refresh.
pub fn on_shootdown_interrupt() {
    if let Some(controller) = lapic::current() {
        controller.end_of_interrupt();
    }
    refresh_local();
}

fn notify(me: usize, targets: u64) {
    let Some(controller) = lapic::current() else {
        return;
    };
    let online = targets & ONLINE.load(Ordering::Acquire);
    for cpu in 0..MAX_CPUS {
        if cpu == me || online & (1u64 << cpu) == 0 {
            continue;
        }
        let apic_id = crate::smp::apic_id_of(cpu);
        if apic_id == u32::MAX {
            continue;
        }
        IPIS.fetch_add(1, Ordering::Relaxed);
        controller.send_fixed(apic_id, trap::TLB_VECTOR);
    }
}

/// Outcome of waiting for an invalidation to be acknowledged everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    /// Generation waited for.
    pub generation: u64,
    /// Processors that hold none of the invalidated translations: those that
    /// recorded this generation, plus those that could not have been holding
    /// one in the first place.
    pub acknowledged: u32,
    /// Processors that were online when the wait started. The claim is about
    /// all of them, however it was obtained for each.
    pub expected: u32,
    /// Processors that had the address space loaded and were therefore waited
    /// for. Equal to `expected` for an invalidation that names no space.
    pub live: u32,
    /// Of those, the ones this processor had to interrupt.
    pub interrupted: u32,
    /// Spins the wait took.
    pub spins: u64,
    /// Whether the wait gave up without every acknowledgement.
    pub timed_out: bool,
}

/// Waits until every online processor has flushed at `generation` or later.
///
/// # Panics
///
/// Never. A wait that does not complete returns with `timed_out` set, which the
/// caller must treat as "quiescence was not observed" rather than as an
/// acknowledgement.
///
/// The caller must hold no lock: this is where the concurrency contract's rule
/// that no lock is held across an acknowledgement wait is enforced by
/// construction, because the wait itself calls back into the local refresh.
#[must_use]
pub fn wait_for(generation: u64) -> Ack {
    wait_for_mask(generation, ONLINE.load(Ordering::Acquire))
}

/// Waits until every processor in `expected_mask` has flushed at `generation`
/// or later, announcing it to them first.
fn wait_for_mask(generation: u64, expected_mask: u64) -> Ack {
    let me = crate::percpu::index();
    refresh_local();
    let expected = expected_mask.count_ones();
    notify(me, expected_mask);

    let mut spins = 0u64;
    loop {
        let mut acknowledged = 0u32;
        let mut outstanding = false;
        for cpu in 0..MAX_CPUS {
            if expected_mask & (1u64 << cpu) == 0 {
                continue;
            }
            if SEEN[cpu].load(Ordering::Acquire) >= generation {
                acknowledged += 1;
            } else {
                outstanding = true;
            }
        }
        if !outstanding {
            let previous = MAX_SPINS.load(Ordering::Relaxed);
            if spins > previous {
                MAX_SPINS.store(spins, Ordering::Relaxed);
            }
            return Ack {
                generation,
                acknowledged,
                expected,
                live: expected,
                interrupted: expected.saturating_sub(1),
                spins,
                timed_out: false,
            };
        }
        spins += 1;
        if spins % 1_000_000 == 0 {
            // A processor spinning for a lock has interrupts masked and will
            // not take the announcement until it makes progress. Repeating it
            // costs one write and removes the dependency on the first one
            // having arrived at a convenient moment.
            notify(me, expected_mask);
        }
        if spins >= WAIT_SPIN_LIMIT {
            TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            return Ack {
                generation,
                acknowledged,
                expected,
                live: expected,
                interrupted: expected.saturating_sub(1),
                spins,
                timed_out: true,
            };
        }
        core::hint::spin_loop();
    }
}

/// Publishes an invalidation and waits for it, in one step.
///
/// The caller must hold no lock.
#[must_use]
pub fn shootdown() -> Ack {
    let generation = publish();
    wait_for(generation)
}

/// Publishes an invalidation of `domain`'s address space and waits for the
/// processors that could be holding one of its translations.
///
/// The generation is published *before* the live set is read, and a processor
/// joining the space stores into that set before it reads the generation. One
/// of the two orders always wins: a processor that is not waited for here has
/// already flushed at a generation at least this one. A processor that never
/// ran in this space has nothing of it to flush, which is the ordinary case
/// and the one this exists for -- an interrupt sent to it would make it
/// retire every translation it holds of somebody else's space, and wait for
/// it to happen.
pub fn shootdown_space(domain: usize) -> Ack {
    let generation = publish();
    let online = ONLINE.load(Ordering::SeqCst);
    let live = live_mask(domain) & online;
    let me = crate::percpu::index();
    let others = live & !(1u64 << me);
    // The claim is still about every processor, and it is still that none of
    // them holds a translation this invalidation removed. It is obtained two
    // ways. A processor with the space loaded is waited for. A processor
    // without it loaded left the space by writing `CR3`, which retired every
    // entry of it -- no mapping here is global and there are no address-space
    // identifiers -- so it was already holding nothing, and an interrupt
    // asking it to flush everything it holds of somebody else's space would
    // establish nothing this did not already know.
    let elsewhere = (online & !live).count_ones();
    if others == 0 {
        // This processor's own record is not moved either: it removed the
        // entries page by page as it went, which retires exactly what had to
        // be retired, and the object it just unmapped records that.
        return Ack {
            generation,
            acknowledged: online.count_ones(),
            expected: online.count_ones(),
            live: live.count_ones(),
            interrupted: 0,
            spins: 0,
            timed_out: false,
        };
    }
    let mut ack = wait_for_mask(generation, live);
    ack.acknowledged += elsewhere;
    ack.expected = online.count_ones();
    ack.live = live.count_ones();
    ack.interrupted = others.count_ones();
    ack
}

/// Whether every processor in `mask` has flushed at `generation` or later.
///
/// The precise form of the reclamation condition: a frame is unreachable once
/// every processor that could have cached a translation to it has flushed past
/// the invalidation that removed the last entry naming it. Processors that are
/// offline are not waited for -- a parked processor holds nothing anyone can
/// reach through.
#[must_use]
pub fn flushed_by(mask: u64, generation: u64) -> bool {
    let online = mask & ONLINE.load(Ordering::Acquire);
    for cpu in 0..MAX_CPUS {
        if online & (1u64 << cpu) == 0 {
            continue;
        }
        if SEEN[cpu].load(Ordering::Acquire) < generation {
            return false;
        }
    }
    true
}

/// The newest generation every online processor has already flushed at.
///
/// A frame whose last mapping was removed at or before this generation cannot
/// be reached through a cached translation on any processor, which is the
/// condition reclamation waits for.
#[must_use]
pub fn safe_generation() -> u64 {
    let online = ONLINE.load(Ordering::Acquire);
    if online == 0 {
        return GENERATION.load(Ordering::Acquire);
    }
    let mut safe = u64::MAX;
    for cpu in 0..MAX_CPUS {
        if online & (1u64 << cpu) == 0 {
            continue;
        }
        let seen = SEEN[cpu].load(Ordering::Acquire);
        if seen < safe {
            safe = seen;
        }
    }
    safe
}

/// Counters for the run's own record.
#[must_use]
pub fn counters() -> (u64, u64, u64, u64, u64) {
    (
        PUBLISHED.load(Ordering::Relaxed),
        IPIS.load(Ordering::Relaxed),
        FLUSHES.load(Ordering::Relaxed),
        TIMEOUTS.load(Ordering::Relaxed),
        MAX_SPINS.load(Ordering::Relaxed),
    )
}
