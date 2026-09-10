//! K4 user domain `k4disk`: the block driver the durability claims rest on.
//!
//! It is the K3 driver's transport with two things added and one taken away.
//! Added: a service interface, so the state service reaches the medium through
//! a capability rather than through a device of its own; and a fault engine,
//! so a run can decide exactly which writes reach the medium and which do not.
//! Taken away: nothing. It still has a capability over one device narrowed to
//! mapping, interrupts and DMA, no configuration space, no bus mastering, no
//! reset, and no way to reach the interrupt table.
//!
//! The fault engine is scaffolding and is marked as such everywhere it appears.
//! It exists because the only honest way to show that recovery works is to cut
//! the write sequence at a named point and boot again on what is left. The cut
//! is deterministic: the harness names a point, and the writes after it are the
//! ones that never happened. That is a stronger statement than a random
//! unflushed-write model would make, and it is also a narrower one -- it says
//! nothing about what a real controller does with a write it has acknowledged.
//!
//! What the engine can do to one write:
//!
//! * **drop it** -- reply as if it had been issued, and never issue it;
//! * **tear it** -- issue only its first sector and then latch, which is what a
//!   power cut in the middle of a multi-sector store looks like;
//! * **refuse it** -- answer that the medium failed;
//! * **hold it** -- issue the write after the next one, so a sequence the
//!   service has not flushed is not the order the medium sees;
//! * **latch** -- refuse every further write, whatever it is, so nothing can
//!   slip through between the service asking for the run to end and it ending.
//!
//! Every one of those is counted, and the counts are what the gate compares
//! against the medium.

#![no_std]
#![no_main]

use core::sync::atomic::{Ordering, fence};

use thalyx_abi::boot_handle;
use thalyx_abi::generated::{dma_profile, right};
use thalyx_user_k4fmt::pkg::{
    DiskReply, DiskRequest, IOBUF_PAGES, IOBUF_VADDR, REGION_VADDR, RING_GRANTED_PAGES, RING_PAGES,
    RING_VADDR, bit, disk_op, disk_slot, disk_status, note,
};
use thalyx_user_k4fmt::{Pod, fault_mode, geometry};
use thalyx_user_rt as rt;
use thalyx_user_rt::k2::{self, report};

/// Base of this program's FP pattern.
const FP_BASE: u64 = 0xD91E_4444_5555_4001;

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

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

const FEATURE_VERSION_1: u32 = 1 << 0;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const BLK_IN: u32 = 0;
const BLK_OUT: u32 = 1;
const BLK_FLUSH: u32 = 4;

/// Entries this driver publishes in the queue.
const QUEUE_DEPTH: u16 = 8;

// Offsets inside the driver's private object.
const DESC_OFFSET: u64 = 0x0000;
const AVAIL_OFFSET: u64 = 0x0100;
const USED_OFFSET: u64 = 0x0200;
const HEADER_OFFSET: u64 = 0x1000;
const STATUS_OFFSET: u64 = 0x1010;
/// One block of the driver's own, where a held write waits. Granted to the
/// device, because a held write still has to be issued from somewhere the
/// device can read.
const HELD_OFFSET: u64 = 0x2000;

const SECTOR_BYTES: u64 = geometry::SECTOR_SIZE;
const BLOCK_BYTES: u64 = geometry::BLOCK_SIZE;
const SECTORS_PER_BLOCK: u64 = BLOCK_BYTES / SECTOR_BYTES;

/// How long the driver waits for one completion.
const IO_DEADLINE_NS: u64 = 3_000_000_000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

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

/// Reads a word of memory the device also reads and writes.
///
/// # Safety
///
/// `address` must be inside an object the supervisor mapped for this domain.
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

/// The queue, and the addresses the device was given for each region.
struct Queue {
    base: u64,
    ring_iova: u64,
    iobuf_iova: u64,
    size: u16,
    avail_index: u16,
    used_index: u16,
    notify: u64,
    published: u32,
}

impl Queue {
    fn desc(&self, index: u16) -> u64 {
        self.base + DESC_OFFSET + u64::from(index) * 16
    }

    /// Publishes one request against `data` and notifies the device.
    ///
    /// `length` of zero is a flush, which is a two-descriptor chain: there is
    /// no such thing as a zero-length descriptor.
    fn publish(&mut self, request_type: u32, sector: u64, data: u64, length: u32) {
        let header = self.base + HEADER_OFFSET;
        let head: u16 = 0;
        // SAFETY: every address below is inside an object the supervisor mapped
        // writable for this domain, and each write is naturally aligned.
        unsafe {
            ring_write::<u32>(header, request_type);
            ring_write::<u32>(header + 4, 0);
            ring_write::<u64>(header + 8, sector);
            ring_write::<u8>(self.base + STATUS_OFFSET, 0xFF);

            let status_index = if length == 0 { head + 1 } else { head + 2 };
            ring_write(
                self.desc(head),
                Desc {
                    addr: self.ring_iova + HEADER_OFFSET,
                    len: 16,
                    flags: DESC_NEXT,
                    next: status_index,
                },
            );
            if length != 0 {
                ring_write(
                    self.desc(head + 1),
                    Desc {
                        addr: data,
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
                    addr: self.ring_iova + STATUS_OFFSET,
                    len: 1,
                    flags: DESC_WRITE,
                    next: 0,
                },
            );

            let slot = self.avail_index % self.size;
            ring_write::<u16>(self.base + AVAIL_OFFSET + 4 + u64::from(slot) * 2, head);
        }
        self.published = length + 1;

        // The descriptors and the ring entry must be visible before the index
        // that publishes them, and the index before the notification.
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
    fn consume(&mut self) -> Option<u32> {
        // SAFETY: inside the mapped object.
        let device_index: u16 = unsafe { ring_read(self.base + USED_OFFSET + 2) };
        fence(Ordering::Acquire);
        let distance = device_index.wrapping_sub(self.used_index);
        if distance == 0 {
            return None;
        }
        if distance > self.size {
            k2::note(report::RING_REJECTED, u64::from(distance));
            return None;
        }
        let slot = self.used_index % self.size;
        // SAFETY: inside the mapped object.
        let entry: UsedElem =
            unsafe { ring_read(self.base + USED_OFFSET + 4 + u64::from(slot) * 8) };
        if entry.id != 0 || entry.len > self.published {
            k2::note(report::RING_REJECTED, u64::from(entry.id));
            return None;
        }
        self.used_index = self.used_index.wrapping_add(1);
        k2::note(report::RING_ACCEPTED, u64::from(entry.id));
        Some(entry.len)
    }

    /// Waits for the one request in flight and returns the device's status byte.
    fn complete(&mut self, irq: u64) -> Option<u8> {
        let deadline = k2::now_ns() + IO_DEADLINE_NS;
        let mut rounds = 0u32;
        loop {
            if self.consume().is_some() {
                // SAFETY: inside the mapped object; the device wrote this byte.
                return Some(unsafe { ring_read(self.base + STATUS_OFFSET) });
            }
            match k2::signal_wait(irq, bit::DEVICE, deadline) {
                Ok(_) => {}
                Err(code) => {
                    k2::note(report::UNEXPECTED, code as u64);
                    return None;
                }
            }
            rounds += 1;
            if rounds > 128 {
                return None;
            }
        }
    }
}

/// What the harness has done to this run's writes, and what it still owes.
#[derive(Default)]
struct Faults {
    /// Nothing further reaches the medium.
    latched: bool,
    /// A write that was held, waiting for the next one to go first.
    held: Option<(u64, u32)>,
    writes_issued: u64,
    writes_suppressed: u64,
    sectors_written: u64,
    flushes: u64,
}

/// Brings the transport up, in the order the specification requires.
fn initialise(
    common: u64,
    notify_base: u64,
    multiplier: u32,
    ring: u64,
    ring_iova: u64,
    iobuf_iova: u64,
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

        // Only what this driver implements is offered back. In particular this
        // driver does not negotiate a write cache feature, so a flush is what
        // the specification says it is rather than what a feature bit changed
        // it into.
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
        mmio_write::<u64>(common + QUEUE_DESC, ring_iova + DESC_OFFSET);
        mmio_write::<u64>(common + QUEUE_DRIVER, ring_iova + AVAIL_OFFSET);
        mmio_write::<u64>(common + QUEUE_DEVICE, ring_iova + USED_OFFSET);
        mmio_write::<u16>(common + QUEUE_MSIX_VECTOR, 0);
        let notify_off = mmio_read::<u16>(common + QUEUE_NOTIFY_OFF);
        mmio_write::<u16>(common + QUEUE_ENABLE, 1);
        mmio_write::<u8>(
            common + DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        let ready = mmio_read::<u8>(common + DEVICE_STATUS);
        k2::note(report::TRANSPORT_READY, u64::from(ready));
        if ready & STATUS_DRIVER_OK == 0 {
            return None;
        }

        Some(Queue {
            base: ring,
            ring_iova,
            iobuf_iova,
            size,
            avail_index: 0,
            used_index: 0,
            notify: notify_base + u64::from(notify_off) * u64::from(multiplier),
            published: 0,
        })
    }
}

/// Issues one transfer and waits for it. Returns false on any refusal.
fn transfer(queue: &mut Queue, irq: u64, kind: u32, block: u64, data: u64, bytes: u64) -> bool {
    queue.publish(kind, block * SECTORS_PER_BLOCK, data, bytes as u32);
    match queue.complete(irq) {
        Some(0) => true,
        Some(code) => {
            k2::note(report::UNEXPECTED, u64::from(code));
            false
        }
        None => false,
    }
}

/// Serves one block request, applying whatever the harness asked for.
fn serve(queue: &mut Queue, irq: u64, faults: &mut Faults, request: &DiskRequest) -> DiskReply {
    let mut reply = DiskReply {
        status: disk_status::OK,
        applied: fault_mode::NONE,
        ..DiskReply::default()
    };

    let bounded = request.count as u64 != 0
        && (request.count as u64) <= IOBUF_PAGES
        && (request.buffer_block as u64) < IOBUF_PAGES
        && (request.buffer_block as u64 + request.count as u64) <= IOBUF_PAGES;
    let data = queue.iobuf_iova + u64::from(request.buffer_block) * BLOCK_BYTES;
    let bytes = u64::from(request.count) * BLOCK_BYTES;

    match request.op {
        disk_op::READ => {
            if !bounded {
                reply.status = disk_status::INVALID;
            } else if !transfer(queue, irq, BLK_IN, request.block, data, bytes) {
                reply.status = disk_status::IO_ERROR;
            } else {
                k2::note(
                    note::DISK_READ,
                    request.block | (u64::from(request.count) << 48),
                );
                reply.transferred = u64::from(request.count);
            }
        }
        disk_op::WRITE => {
            if !bounded {
                reply.status = disk_status::INVALID;
            } else if faults.latched {
                reply.status = disk_status::LATCHED;
                faults.writes_suppressed += 1;
            } else {
                reply = write_with_faults(queue, irq, faults, request, data, bytes);
            }
        }
        disk_op::FLUSH => {
            // A held write is released here and not before: a flush is the
            // point at which the medium stops being allowed to reorder.
            if faults.latched {
                reply.status = disk_status::LATCHED;
            } else if !release_held(queue, irq, faults) || !transfer(queue, irq, BLK_FLUSH, 0, 0, 0)
            {
                reply.status = disk_status::IO_ERROR;
            } else {
                faults.flushes += 1;
                k2::note(note::DISK_FLUSH, faults.flushes);
            }
        }
        disk_op::LATCH => {
            faults.latched = true;
            // A write that was being held is not issued now: latching means
            // nothing further reaches the medium, and a held write is the most
            // obvious thing that would otherwise slip through.
            if faults.held.take().is_some() {
                faults.writes_suppressed += 1;
            }
            k2::note(note::DISK_LATCHED, faults.writes_issued);
        }
        disk_op::STATS => {}
        _ => reply.status = disk_status::INVALID,
    }

    reply.writes_issued = faults.writes_issued;
    reply.writes_suppressed = faults.writes_suppressed;
    reply.flushes = faults.flushes;
    reply.sectors_written = faults.sectors_written;
    reply
}

/// The write path, which is where every fault mode lands.
fn write_with_faults(
    queue: &mut Queue,
    irq: u64,
    faults: &mut Faults,
    request: &DiskRequest,
    data: u64,
    bytes: u64,
) -> DiskReply {
    let mut reply = DiskReply {
        status: disk_status::OK,
        applied: request.fault_mode,
        ..DiskReply::default()
    };

    match request.fault_mode {
        fault_mode::DROP_WRITE => {
            faults.writes_suppressed += 1;
            k2::note(
                note::DISK_SUPPRESSED,
                u64::from(fault_mode::DROP_WRITE) | (request.block << 8),
            );
            return reply;
        }
        fault_mode::IO_ERROR => {
            faults.writes_suppressed += 1;
            k2::note(note::DISK_IO_ERROR, request.block);
            reply.status = disk_status::IO_ERROR;
            return reply;
        }
        fault_mode::REORDER => {
            // Held until the next flush, and issued then. That is what a medium
            // is allowed to do with a write nobody has flushed, and it is the
            // only version of "reordered" this driver can produce without
            // inventing a behaviour no device has. A latch before that flush
            // loses it, which is also what a power cut would do.
            //
            // Only a single-block write can be held: the buffer it waits in is
            // one block, and a driver that quietly held less than it was given
            // would be a second fault nobody asked for.
            if request.count != 1 || faults.held.is_some() {
                reply.status = disk_status::INVALID;
                return reply;
            }
            // SAFETY: both ranges are inside objects the supervisor mapped
            // writable for this domain, one block long, and they do not
            // overlap: one is the shared buffer, the other the driver's own.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (IOBUF_VADDR + u64::from(request.buffer_block) * BLOCK_BYTES) as *const u8,
                    (queue.base + HELD_OFFSET) as *mut u8,
                    BLOCK_BYTES as usize,
                );
            }
            faults.held = Some((request.block, 1));
            k2::note(note::DISK_REORDERED, request.block);
            return reply;
        }
        _ => {}
    }

    let torn = request.fault_mode == fault_mode::TEAR_WRITE;
    let issue = if torn { SECTOR_BYTES } else { bytes };
    if !transfer(queue, irq, BLK_OUT, request.block, data, issue) {
        reply.status = disk_status::IO_ERROR;
        return reply;
    }
    faults.writes_issued += 1;
    faults.sectors_written += issue / SECTOR_BYTES;
    k2::note(
        note::DISK_WRITE,
        request.block | (u64::from(request.count) << 48),
    );
    if torn {
        // A tear is a power cut in the middle of a store. Nothing after it
        // reaches the medium, including the rest of this record.
        faults.writes_suppressed += 1;
        faults.latched = true;
        k2::note(
            note::DISK_SUPPRESSED,
            u64::from(fault_mode::TEAR_WRITE) | (request.block << 8),
        );
        k2::note(note::DISK_LATCHED, faults.writes_issued);
        reply.transferred = 0;
        return reply;
    }
    reply.transferred = u64::from(request.count);
    reply
}

/// Issues a write that was held, if there is one. Called at a flush.
fn release_held(queue: &mut Queue, irq: u64, faults: &mut Faults) -> bool {
    let Some((block, count)) = faults.held.take() else {
        return true;
    };
    let held = queue.ring_iova + HELD_OFFSET;
    if !transfer(
        queue,
        irq,
        BLK_OUT,
        block,
        held,
        u64::from(count) * BLOCK_BYTES,
    ) {
        return false;
    }
    faults.writes_issued += 1;
    faults.sectors_written += SECTORS_PER_BLOCK;
    k2::note(note::DISK_WRITE, block | (u64::from(count) << 48));
    true
}

fn run() -> ! {
    rt::establish(FP_BASE);
    let device = boot_handle(disk_slot::DEVICE);
    let irq = boot_handle(disk_slot::IRQ);
    let ring_cap = boot_handle(disk_slot::RING);
    let iobuf_cap = boot_handle(disk_slot::IOBUF);
    let service = boot_handle(disk_slot::SERVICE);

    let info = match k2::device_query(device) {
        Ok(info) => info,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(1)
        }
    };
    let session = info.session;
    k2::note(report::DEVICE_OBSERVED, u64::from(session));

    let ring_grant = match k2::device_dma_map(
        device,
        ring_cap,
        0,
        RING_GRANTED_PAGES as u32,
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
    let iobuf_grant = match k2::device_dma_map(
        device,
        iobuf_cap,
        0,
        IOBUF_PAGES as u32,
        right::MEMORY_READ | right::MEMORY_WRITE,
        0,
        session,
    ) {
        Ok(grant) => grant,
        Err(code) => {
            k2::note(report::UNEXPECTED, code as u64);
            rt::exit(3)
        }
    };
    k2::note(report::DMA_GRANTED, ring_grant.iova);
    k2::note(report::DMA_GRANTED, iobuf_grant.iova);
    if ring_grant.profile != dma_profile::WEAK_TRUSTED_DRIVER {
        k2::note(report::UNEXPECTED, u64::from(ring_grant.profile));
    }

    // The page past the granted ones is this driver's own. It is not granted,
    // and it is checked here rather than assumed, because the whole argument
    // for the fault engine is that the driver decides what the device sees.
    if RING_GRANTED_PAGES >= RING_PAGES {
        k2::note(report::UNEXPECTED, RING_PAGES);
    }

    let Some(mut queue) = initialise(
        REGION_VADDR[0],
        REGION_VADDR[1],
        info.regions[1].notify_off_multiplier,
        RING_VADDR,
        ring_grant.iova,
        iobuf_grant.iova,
    ) else {
        rt::exit(4)
    };
    k2::note(note::DISK_READY, u64::from(queue.size));

    let mut faults = Faults::default();
    loop {
        let (message, invocation) = match k2::endpoint_receive(service, 0, false) {
            Ok(pair) => pair,
            Err(code) => {
                k2::note(report::UNEXPECTED, code as u64);
                rt::exit(5)
            }
        };
        let request = match DiskRequest::read_from(&message.payload, 0) {
            Some(request) if message.header.payload_len as usize >= DiskRequest::SIZE => request,
            _ => {
                let reply = DiskReply {
                    status: disk_status::INVALID,
                    ..DiskReply::default()
                };
                let _ = k2::invocation_reply(invocation, 0, reply.as_bytes());
                continue;
            }
        };
        let reply = serve(&mut queue, irq, &mut faults, &request);
        if let Err(code) =
            k2::invocation_reply(invocation, u64::from(reply.status), reply.as_bytes())
        {
            k2::note(report::UNEXPECTED, code as u64);
        }
    }
}

rt::entry!(run);
