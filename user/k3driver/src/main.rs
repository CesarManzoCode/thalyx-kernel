//! K3 user domain `k3driver`: a virtio block driver that is not in the kernel.
//!
//! What this domain holds is the whole argument. It has a capability over one
//! device, narrowed to mapping, interrupts and DMA; the four register windows
//! the kernel validated, mapped at addresses the kernel chose; one signal the
//! interrupt raises; and one memory object, three pages of which it grants the
//! device. It has no configuration space, no bus mastering, no reset, and no
//! way to reach the table that decides where an interrupt is written. Every one
//! of those is something a driver could otherwise use to reach past its device,
//! and each is refused here rather than merely unused.
//!
//! The transport protocol is followed as the specification states it and not as
//! an emulator tolerates it: reset, acknowledge, driver, read the device's
//! features, offer back only what this driver actually implements, set
//! features-ok and *read it back*, configure the queue, then driver-ok, and only
//! then publish a buffer. Nothing is negotiated that is not implemented: no
//! packed ring, no indirect descriptors, no event index, no queue reset.
//!
//! Completions are validated rather than trusted. A used entry names a
//! descriptor identifier that must be one this driver actually published and
//! has not already retired, and a length that must fit what it published. The
//! ring index is sixteen bits and wraps by design, so the distance is taken
//! modulo that width and bounded by the queue size, which is the only
//! comparison that means anything. The validator is also run against rings this
//! program builds itself, in memory the device cannot reach, precisely because
//! a conforming emulated device never produces the entries it has to reject —
//! those runs are marked as synthetic and prove the validator, not the device.

#![no_std]
#![no_main]

use core::sync::atomic::{Ordering, compiler_fence, fence};

use thalyx_abi::boot_handle;
use thalyx_abi::generated::{dma_profile, right, status};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report};
use thalyx_user_rt::k3::{self, bit, driver_slot};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0xD91E_2222_3333_4001;

// Offsets inside the virtio common configuration structure, virtio 1.2 §4.1.4.3.
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const NUM_QUEUES: u64 = 0x12;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

// Device status bits.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

/// `VIRTIO_F_VERSION_1`, feature bit 32.
const FEATURE_VERSION_1: u32 = 1 << 0;

// Descriptor flags.
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

// Block request types.
const BLK_IN: u32 = 0;
const BLK_OUT: u32 = 1;
const BLK_FLUSH: u32 = 4;

/// Descriptors this driver publishes. Small on purpose: a queue this driver
/// cannot fill is a queue whose bounds it never exercises.
const QUEUE_DEPTH: u16 = 8;

// Offsets inside the shared memory object.
const DESC_OFFSET: u64 = 0x0000;
const AVAIL_OFFSET: u64 = 0x0100;
const USED_OFFSET: u64 = 0x0200;
const HEADER_OFFSET: u64 = 0x1000;
const STATUS_OFFSET: u64 = 0x1010;
const DATA_OFFSET: u64 = 0x2000;
/// The fourth page, which is never granted to the device.
const SCRATCH_OFFSET: u64 = 0x3000;
/// Bytes of one block, in the units virtio counts sectors in.
const SECTOR_BYTES: u64 = 512;

/// A split-ring descriptor.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// A used-ring element.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsedElem {
    id: u32,
    len: u32,
}

/// Reads a register of a mapped device window.
///
/// # Safety
///
/// `address` must be inside a window the kernel mapped for this domain, and the
/// access width must be one the device defines at that offset.
unsafe fn mmio_read<T: Copy>(address: u64) -> T {
    // SAFETY: the caller guarantees the address and the width.
    unsafe { core::ptr::read_volatile(address as *const T) }
}

/// Writes a register of a mapped device window.
///
/// # Safety
///
/// As [`mmio_read`].
unsafe fn mmio_write<T: Copy>(address: u64, value: T) {
    // SAFETY: the caller guarantees the address and the width.
    unsafe { core::ptr::write_volatile(address as *mut T, value) }
}

/// Reads a word of the shared memory the device also reads and writes.
///
/// # Safety
///
/// `address` must be inside the object the supervisor mapped at
/// [`k3::RING_VADDR`].
unsafe fn ring_read<T: Copy>(address: u64) -> T {
    // SAFETY: the caller guarantees the address.
    unsafe { core::ptr::read_volatile(address as *const T) }
}

/// Writes a word of that memory.
///
/// # Safety
///
/// As [`ring_read`].
unsafe fn ring_write<T: Copy>(address: u64, value: T) {
    // SAFETY: the caller guarantees the address.
    unsafe { core::ptr::write_volatile(address as *mut T, value) }
}

/// What the driver keeps about the queue it drives.
struct Queue {
    /// Where the driver reads and writes the rings.
    base: u64,
    /// Where the device reads and writes them.
    iova: u64,
    /// Entries.
    size: u16,
    /// Next index this driver will publish at.
    avail_index: u16,
    /// Last used index this driver consumed.
    used_index: u16,
    /// Address the driver writes to tell the device there is work.
    notify: u64,
    /// Descriptor heads currently published.
    in_flight: [bool; QUEUE_DEPTH as usize],
    /// Length this driver published for each head.
    published_len: [u32; QUEUE_DEPTH as usize],
}

impl Queue {
    fn desc(&self, index: u16) -> u64 {
        self.base + DESC_OFFSET + u64::from(index) * 16
    }

    /// Publishes a three-descriptor block request and notifies the device.
    fn publish(&mut self, request_type: u32, sector: u64, length: u32, head: u16) {
        let header = self.base + HEADER_OFFSET;
        // SAFETY: every address below is inside the object the supervisor
        // mapped writable for this domain, and each write is naturally aligned.
        unsafe {
            ring_write::<u32>(header, request_type);
            ring_write::<u32>(header + 4, 0);
            ring_write::<u64>(header + 8, sector);
            ring_write::<u8>(self.base + STATUS_OFFSET, 0xFF);

            // Header: the device reads it. Data: written by the device for a
            // read and read by it for a write. Status: written by the device.
            //
            // A request with no payload -- a flush -- is a two-descriptor
            // chain, and the second descriptor is the one straight after the
            // header. Two things make that necessary rather than tidy: there is
            // no such thing as a zero-length descriptor, and this queue is
            // eight entries deep, so a chain that always reserved three would
            // put the last request's status descriptor past the end of the
            // table. Either mistake produces the same symptom -- a chain the
            // device refuses and therefore never completes.
            let status_index = if length == 0 { head + 1 } else { head + 2 };
            ring_write(
                self.desc(head),
                Desc {
                    addr: self.iova + HEADER_OFFSET,
                    len: 16,
                    flags: DESC_NEXT,
                    next: status_index,
                },
            );
            if length != 0 {
                ring_write(
                    self.desc(head + 1),
                    Desc {
                        addr: self.iova + DATA_OFFSET,
                        len: length,
                        flags: if request_type == BLK_IN {
                            DESC_NEXT | DESC_WRITE
                        } else {
                            DESC_NEXT
                        },
                        next: status_index,
                    },
                );
            }
            ring_write(
                self.desc(status_index),
                Desc {
                    addr: self.iova + STATUS_OFFSET,
                    len: 1,
                    flags: DESC_WRITE,
                    next: 0,
                },
            );

            let slot = self.avail_index % self.size;
            ring_write::<u16>(self.base + AVAIL_OFFSET + 4 + u64::from(slot) * 2, head);
        }
        self.in_flight[head as usize] = true;
        self.published_len[head as usize] = length + 1;

        // The descriptors and the ring entry must be visible before the index
        // that publishes them, and the index before the notification. Neither
        // is implied by the other.
        fence(Ordering::Release);
        self.avail_index = self.avail_index.wrapping_add(1);
        // SAFETY: inside the mapped object.
        unsafe { ring_write::<u16>(self.base + AVAIL_OFFSET + 2, self.avail_index) };
        fence(Ordering::SeqCst);
        // SAFETY: the notify window the kernel mapped, at the offset the device
        // reported for this queue.
        unsafe { mmio_write::<u16>(self.notify, 0) };
    }

    /// Consumes one completion, validating it before believing it.
    ///
    /// Returns the descriptor head and the length the device reported, or
    /// `None` when nothing has completed. A malformed entry is rejected and
    /// reported; it never becomes a head this driver reuses.
    fn consume(&mut self) -> Option<(u16, u32)> {
        // SAFETY: inside the mapped object.
        let device_index: u16 = unsafe { ring_read(self.base + USED_OFFSET + 2) };
        fence(Ordering::Acquire);
        validate_used(
            self,
            device_index,
            |driver, slot| {
                // SAFETY: inside the mapped object.
                unsafe {
                    ring_read::<UsedElem>(driver.base + USED_OFFSET + 4 + u64::from(slot) * 8)
                }
            },
            false,
        )
    }
}

/// Decides whether one used-ring entry may be believed.
///
/// The index is sixteen bits and wraps by design, so "is there something new"
/// is a wrapping difference bounded by the queue size, not an integer
/// comparison. Anything else -- a head this driver never published, a head it
/// already retired, a length longer than what it offered -- is a device or a
/// ring saying something impossible, and is refused rather than followed.
fn validate_used(
    queue: &mut Queue,
    device_index: u16,
    read: impl Fn(&Queue, u16) -> UsedElem,
    synthetic: bool,
) -> Option<(u16, u32)> {
    let pending = device_index.wrapping_sub(queue.used_index);
    if pending == 0 {
        return None;
    }
    if pending > queue.size {
        k2::note(report::RING_REJECTED, u64::from(pending));
        return None;
    }
    let slot = queue.used_index % queue.size;
    let entry = read(queue, slot);
    if entry.id >= u32::from(QUEUE_DEPTH) {
        k2::note(report::RING_REJECTED, u64::from(entry.id));
        queue.used_index = queue.used_index.wrapping_add(1);
        return None;
    }
    let head = entry.id as u16;
    if !queue.in_flight[head as usize] {
        k2::note(report::RING_REJECTED, u64::from(head));
        queue.used_index = queue.used_index.wrapping_add(1);
        return None;
    }
    if entry.len > queue.published_len[head as usize] {
        k2::note(report::RING_REJECTED, u64::from(entry.len));
        queue.used_index = queue.used_index.wrapping_add(1);
        return None;
    }
    queue.used_index = queue.used_index.wrapping_add(1);
    if !synthetic {
        queue.in_flight[head as usize] = false;
        k2::note(report::RING_ACCEPTED, u64::from(head));
    }
    Some((head, entry.len))
}

/// Runs the validator against rings this program builds, in memory the device
/// cannot reach.
///
/// A conforming device never produces these entries, so without this the
/// rejecting branches would be code no run had ever entered. The page used here
/// is the fourth of the object and is deliberately outside the DMA grant, so
/// nothing but this program can have written what the validator reads.
fn validator_self_test(queue: &mut Queue) -> u64 {
    let scratch = queue.base + SCRATCH_OFFSET;
    let saved_used = queue.used_index;
    let saved_flight = queue.in_flight[0];
    let saved_len = queue.published_len[0];
    let mut rejected = 0u64;

    // Head zero is made to look published, with a length this driver could
    // plausibly have offered. Without it the over-length case would be refused
    // for the wrong reason -- not in flight -- and the case that must be
    // accepted could not be accepted by any correct validator.
    queue.in_flight[0] = true;
    queue.published_len[0] = SECTOR_BYTES as u32 + 1;

    // An identifier no descriptor table this driver published contains.
    let cases: [(u32, u32); 4] = [(u32::from(QUEUE_DEPTH), 1), (1, 1), (0, u32::MAX), (0, 1)];
    for (index, (id, len)) in cases.iter().enumerate() {
        // SAFETY: the scratch page is inside the object the supervisor mapped
        // writable for this domain, and is not part of the range granted to the
        // device.
        unsafe { ring_write(scratch, UsedElem { id: *id, len: *len }) };
        let before = queue.used_index;
        let outcome = validate_used(
            queue,
            before.wrapping_add(1),
            |_, _| {
                // SAFETY: as above.
                unsafe { ring_read::<UsedElem>(scratch) }
            },
            true,
        );
        // The fourth case names a head that is genuinely in flight with a
        // plausible length, so it must be accepted: a validator that rejected
        // everything would pass the first three for the wrong reason.
        let expected_reject = index < 3;
        if outcome.is_none() == expected_reject {
            if expected_reject {
                rejected += 1;
            }
        } else {
            k2::note(report::UNEXPECTED, index as u64);
        }
    }
    queue.used_index = saved_used;
    queue.in_flight[0] = saved_flight;
    queue.published_len[0] = saved_len;
    rejected
}

/// Refusals this driver must receive, because the authority is not its.
fn negative_controls(device: u64, session: u32, ring: u64, iova: u64) -> u64 {
    let mut refused = 0u64;

    // Bus mastering and reset belong to recovery authority. This handle carries
    // neither right, and asking is how that is observed rather than assumed.
    if k2::expect_refusal(
        k2::device_set_master(device, true, session),
        status::INSUFFICIENT_RIGHTS,
    ) {
        refused += 1;
    }
    if k2::expect_refusal(
        k2::device_reset(device).map(|_| 0),
        status::INSUFFICIENT_RIGHTS,
    ) {
        refused += 1;
    }

    // Mapping a window needs authority over a domain to map it into. A driver
    // holds none, so it cannot place its own device anywhere it likes.
    if k2::expect_refusal(
        k2::device_map_region(device, boot_handle(driver_slot::DEVICE), 0, ring, session),
        status::WRONG_TYPE,
    ) {
        refused += 1;
    }

    // A window this device does not have.
    if k2::expect_refusal(
        k2::device_map_region(device, 0, 99, ring, session),
        status::INVALID_ARGUMENT,
    ) {
        refused += 1;
    }

    // The strong isolation profile. This machine programs no remapping unit, so
    // requiring it must be refused before anything is pinned, not granted in a
    // weaker form under the same name.
    match k2::device_dma_map(
        device,
        boot_handle(driver_slot::RING),
        0,
        1,
        right::MEMORY_READ,
        dma_profile::STRONG_IOMMU,
        session,
    ) {
        Err(code) if code == status::UNSUPPORTED_PROFILE => {
            k2::note(report::PROFILE_REFUSED, code as u64);
            refused += 1;
        }
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
        Ok(_) => k2::note(report::NOT_REFUSED, 0),
    }

    // A grant may not be revoked while the device can still be issuing
    // transactions against it.
    if k2::expect_refusal(
        k2::device_dma_unmap(device, iova, k3::RING_GRANTED_PAGES * 4096, session),
        status::STATE_CONFLICT,
    ) {
        refused += 1;
    }

    // A session number that is not the one the device is in.
    if k2::expect_refusal(
        k2::device_dma_map(
            device,
            boot_handle(driver_slot::RING),
            0,
            1,
            right::MEMORY_READ,
            0,
            session.wrapping_add(7),
        )
        .map(|_| 0),
        status::STATE_CONFLICT,
    ) {
        refused += 1;
    }

    refused
}

/// Brings the transport up, in the order the specification requires.
///
/// Returns the queue, or `None` after telling the device the driver failed.
fn initialise(
    common: u64,
    notify_base: u64,
    multiplier: u32,
    ring: u64,
    iova: u64,
) -> Option<Queue> {
    // SAFETY: `common` is the common configuration window the kernel mapped for
    // this domain, and every offset below is one the transport defines.
    unsafe {
        mmio_write::<u8>(common + DEVICE_STATUS, 0);
        while mmio_read::<u8>(common + DEVICE_STATUS) != 0 {
            core::hint::spin_loop();
        }
        mmio_write::<u8>(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE);
        mmio_write::<u8>(common + DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        mmio_write::<u32>(common + DEVICE_FEATURE_SELECT, 1);
        let high = mmio_read::<u32>(common + DEVICE_FEATURE);
        if high & FEATURE_VERSION_1 == 0 {
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
            k2::note(report::UNEXPECTED, u64::from(high));
            return None;
        }

        // Only what this driver implements is offered back. Negotiating a
        // feature whose rules are not implemented is how a driver ends up
        // reading a ring in a format it does not parse.
        mmio_write::<u32>(common + DRIVER_FEATURE_SELECT, 0);
        mmio_write::<u32>(common + DRIVER_FEATURE, 0);
        mmio_write::<u32>(common + DRIVER_FEATURE_SELECT, 1);
        mmio_write::<u32>(common + DRIVER_FEATURE, FEATURE_VERSION_1);

        mmio_write::<u8>(
            common + DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        let confirmed = mmio_read::<u8>(common + DEVICE_STATUS);
        if confirmed & STATUS_FEATURES_OK == 0 {
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
            k2::note(report::UNEXPECTED, u64::from(confirmed));
            return None;
        }

        let queues = mmio_read::<u16>(common + NUM_QUEUES);
        if queues == 0 {
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
            return None;
        }
        mmio_write::<u16>(common + QUEUE_SELECT, 0);
        let offered = mmio_read::<u16>(common + QUEUE_SIZE);
        if offered == 0 {
            mmio_write::<u8>(common + DEVICE_STATUS, STATUS_FAILED);
            return None;
        }
        let size = if offered < QUEUE_DEPTH {
            offered
        } else {
            QUEUE_DEPTH
        };
        mmio_write::<u16>(common + QUEUE_SIZE, size);
        mmio_write::<u64>(common + QUEUE_DESC, iova + DESC_OFFSET);
        mmio_write::<u64>(common + QUEUE_DRIVER, iova + AVAIL_OFFSET);
        mmio_write::<u64>(common + QUEUE_DEVICE, iova + USED_OFFSET);
        mmio_write::<u16>(common + QUEUE_MSIX_VECTOR, 0);
        let vector = mmio_read::<u16>(common + QUEUE_MSIX_VECTOR);
        let notify_off = mmio_read::<u16>(common + QUEUE_NOTIFY_OFF);
        mmio_write::<u16>(common + QUEUE_ENABLE, 1);
        mmio_write::<u8>(
            common + DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        let ready = mmio_read::<u8>(common + DEVICE_STATUS);
        k2::note(
            report::TRANSPORT_READY,
            u64::from(ready) | u64::from(vector) << 8,
        );
        if ready & STATUS_DRIVER_OK == 0 {
            return None;
        }

        Some(Queue {
            base: ring,
            iova,
            size,
            avail_index: 0,
            used_index: 0,
            notify: notify_base + u64::from(notify_off) * u64::from(multiplier),
            in_flight: [false; QUEUE_DEPTH as usize],
            published_len: [0; QUEUE_DEPTH as usize],
        })
    }
}

/// Waits for the device's interrupt and consumes one completion.
fn complete(queue: &mut Queue, irq: u64, deadline: u64) -> Option<(u16, u32)> {
    let mut rounds = 0u32;
    loop {
        if let Some(done) = queue.consume() {
            return Some(done);
        }
        match k2::signal_wait(irq, bit::DEVICE, deadline) {
            Ok(_) => {}
            Err(code) if code == status::TIMED_OUT => {
                k2::note(report::UNEXPECTED, code as u64);
                return None;
            }
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                return None;
            }
        }
        rounds += 1;
        if rounds > 64 {
            return None;
        }
    }
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let device = boot_handle(driver_slot::DEVICE);
    let irq = boot_handle(driver_slot::IRQ);
    let ring_cap = boot_handle(driver_slot::RING);

    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(1)
        }
    };
    k2::note(report::DEVICE_OBSERVED, u64::from(info.session));
    let session = info.session;

    // The device's own addresses, granted by the kernel. Nothing here derives
    // one by arithmetic on a pointer of this program's.
    let grant = match k2::device_dma_map(
        device,
        ring_cap,
        0,
        k3::RING_GRANTED_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
        0,
        session,
    ) {
        Ok(grant) => grant,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(2)
        }
    };
    k2::note(report::DMA_GRANTED, grant.iova);
    if grant.profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(grant.profile));
    }

    let refused = negative_controls(device, session, k3::RING_VADDR, grant.iova);
    k2::note(report::REFUSED_AS_EXPECTED, refused);

    let common = k3::REGION_VADDR[0];
    let notify = k3::REGION_VADDR[1];
    let Some(mut queue) = initialise(
        common,
        notify,
        info.regions[1].notify_off_multiplier,
        k3::RING_VADDR,
        grant.iova,
    ) else {
        rt::exit(3)
    };

    let synthetic = validator_self_test(&mut queue);
    k2::note(report::VALIDATOR_SELF_TEST, synthetic);

    // A read of the first block, a write of a block this driver composed, and a
    // flush. The flush is here because the durable claim K4 will make needs the
    // ordering it provides, and because a driver that never issued one would
    // not have shown the device accepts it.
    let deadline = k2::now_ns() + 2_000_000_000;
    for (index, (kind, sector, length)) in [
        (BLK_IN, 0u64, SECTOR_BYTES as u32),
        (BLK_OUT, 1u64, SECTOR_BYTES as u32),
        (BLK_FLUSH, 0u64, 0u32),
    ]
    .iter()
    .enumerate()
    {
        if *kind == BLK_OUT {
            // Something recognisable, so a read-back would be able to tell this
            // driver's write from whatever the medium held.
            for word in 0..(SECTOR_BYTES / 8) {
                // SAFETY: inside the object the supervisor mapped writable.
                unsafe {
                    ring_write::<u64>(
                        queue.base + DATA_OFFSET + word * 8,
                        0x4B33_0000_0000_0000 | word,
                    );
                }
            }
        }
        compiler_fence(Ordering::SeqCst);
        queue.publish(*kind, *sector, *length, (index as u16) * 3);
        match complete(&mut queue, irq, deadline) {
            Some((head, len)) => {
                // SAFETY: inside the mapped object; the device wrote this byte.
                let reported: u8 = unsafe { ring_read(queue.base + STATUS_OFFSET) };
                k2::note(
                    report::BLOCK_COMPLETED,
                    u64::from(reported) | u64::from(len) << 8 | u64::from(head) << 40,
                );
                if reported != 0 {
                    k2::note(report::UNEXPECTED, u64::from(reported));
                }
            }
            None => {
                k2::note(report::UNEXPECTED, index as u64);
                rt::exit(4)
            }
        }
    }

    // The driver is done with the device. What happens next is not its
    // decision: recovery authority resets the device, and everything this
    // program still remembers about it stops being accepted.
    k2::note(report::DONE, 1);
    let _ = k2::signal_raise(boot_handle(driver_slot::DONE), bit::DRIVER_DONE);
    match k2::signal_wait(irq, bit::STALE, k2::now_ns() + 5_000_000_000) {
        Ok(_) => {}
        Err(code) => k2::note(report::UNEXPECTED, code as u64),
    }

    let mut stale = 0u64;
    if k2::expect_refusal(
        k2::device_dma_map(device, ring_cap, 0, 1, right::MEMORY_READ, 0, session).map(|_| 0),
        status::STATE_CONFLICT,
    ) {
        stale += 1;
    }
    if k2::expect_refusal(
        k2::device_dma_unmap(device, grant.iova, k3::RING_GRANTED_PAGES * 4096, session),
        status::STATE_CONFLICT,
    ) {
        stale += 1;
    }
    if k2::expect_refusal(
        k2::device_bind_irq(device, irq, bit::DEVICE, 0, session),
        status::STATE_CONFLICT,
    ) {
        stale += 1;
    }
    k2::note(report::STALE_SESSION_REFUSED, stale);

    // The new session is readable, which is how a driver would learn it has to
    // start again rather than guessing from a failure.
    if let Ok(after) = k2::device_query(device) {
        k2::note(report::DEVICE_OBSERVED, u64::from(after.session));
    }

    k2::note(report::DONE, 2);
    rt::exit(0)
}

rt::entry!(run);
